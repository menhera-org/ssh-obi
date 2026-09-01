use std::process::ExitCode;

use clap::{ArgGroup, Parser};
use ssh_obi::daemon::run_daemon;
use ssh_obi::protocol::{WindowSize, supports_protocol_baseline};
use ssh_obi::server::{
    detach_from_env, detach_local_session, list_local_sessions, run_broker_stdio,
};
use ssh_obi::session::{SessionId, generate_session_id};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about,
    group(ArgGroup::new("session_action").args(["daemon", "detach"]))
)]
struct Args {
    #[arg(long = "obi-version", conflicts_with_all = ["daemon", "detach", "list", "protocol_check"])]
    obi_version: bool,

    #[arg(long, conflicts_with_all = ["detach", "list", "protocol_check"])]
    daemon: bool,

    #[arg(
        long,
        conflicts_with_all = ["daemon", "list", "protocol_check"],
        help = "Detach the current session, or the session selected by --session"
    )]
    detach: bool,

    #[arg(
        long,
        conflicts_with_all = ["daemon", "detach", "protocol_check"],
        help = "List alive sessions for the current user"
    )]
    list: bool,

    #[arg(long = "protocol-check", value_name = "BASELINE", conflicts_with_all = ["daemon", "detach", "list"])]
    protocol_check: Option<String>,

    #[arg(
        long,
        value_name = "ID",
        requires = "session_action",
        conflicts_with_all = ["obi_version", "list", "protocol_check"],
        help = "Session ID for --daemon or --detach"
    )]
    session: Option<String>,

    #[arg(long, value_name = "ROWS", requires = "daemon")]
    rows: Option<u16>,

    #[arg(long, value_name = "COLS", requires = "daemon")]
    cols: Option<u16>,

    #[arg(long = "pixel-width", value_name = "PX", requires = "daemon")]
    pixel_width: Option<u16>,

    #[arg(long = "pixel-height", value_name = "PX", requires = "daemon")]
    pixel_height: Option<u16>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if args.obi_version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if let Some(baseline) = args.protocol_check {
        return if supports_protocol_baseline(&baseline) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }

    if args.daemon {
        let session_id = match args.session {
            Some(session) => match SessionId::new(session) {
                Ok(session_id) => session_id,
                Err(err) => {
                    eprintln!("ssh-obi-server: invalid session id: {err}");
                    return ExitCode::from(2);
                }
            },
            None => match generate_session_id(std::iter::empty()) {
                Ok(session_id) => session_id,
                Err(err) => {
                    eprintln!("ssh-obi-server: failed to generate session id: {err}");
                    return ExitCode::from(1);
                }
            },
        };

        let initial_window_size = match (args.rows, args.cols, args.pixel_width, args.pixel_height)
        {
            (None, None, None, None) => None,
            (Some(rows), Some(cols), pixel_width, pixel_height) => Some(WindowSize {
                rows,
                cols,
                pixel_width: pixel_width.unwrap_or(0),
                pixel_height: pixel_height.unwrap_or(0),
            }),
            _ => {
                eprintln!("ssh-obi-server: --rows and --cols are required together");
                return ExitCode::from(2);
            }
        };

        if let Err(err) = run_daemon(session_id, initial_window_size) {
            eprintln!("ssh-obi-server: daemon failed: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if args.detach {
        let result = match args.session.as_deref() {
            Some(session_id) => detach_local_session(session_id),
            None => detach_from_env(),
        };
        if let Err(err) = result {
            eprintln!("ssh-obi-server: detach failed: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if args.list {
        match list_local_sessions() {
            Ok(table) => {
                print!("{table}");
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                eprintln!("ssh-obi-server: list failed: {err}");
                return ExitCode::from(1);
            }
        }
    }

    if let Err(err) = run_broker_stdio() {
        eprintln!("ssh-obi-server: broker failed: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_accepts_session_target() {
        let args =
            Args::try_parse_from(["ssh-obi-server", "--detach", "--session", "aaaaaaaa"]).unwrap();

        assert!(args.detach);
        assert_eq!(args.session.as_deref(), Some("aaaaaaaa"));
    }

    #[test]
    fn detach_without_session_remains_valid() {
        let args = Args::try_parse_from(["ssh-obi-server", "--detach"]).unwrap();

        assert!(args.detach);
        assert!(args.session.is_none());
    }

    #[test]
    fn session_requires_daemon_or_detach_action() {
        assert!(Args::try_parse_from(["ssh-obi-server", "--session", "aaaaaaaa"]).is_err());
    }

    #[test]
    fn list_rejects_session_target() {
        assert!(
            Args::try_parse_from(["ssh-obi-server", "--list", "--session", "aaaaaaaa",]).is_err()
        );
    }
}
