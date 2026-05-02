use std::process::ExitCode;

use clap::Parser;
use ssh_obi::daemon::run_daemon;
use ssh_obi::protocol::supports_protocol_baseline;
use ssh_obi::server::{detach_from_env, run_broker_stdio};
use ssh_obi::session::{SessionId, generate_session_id};

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(long, conflicts_with_all = ["detach", "protocol_check"])]
    daemon: bool,

    #[arg(long, conflicts_with_all = ["daemon", "protocol_check"])]
    detach: bool,

    #[arg(long = "protocol-check", value_name = "BASELINE", conflicts_with_all = ["daemon", "detach"])]
    protocol_check: Option<String>,

    #[arg(long, value_name = "ID", requires = "daemon")]
    session: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

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

        if let Err(err) = run_daemon(session_id) {
            eprintln!("ssh-obi-server: daemon failed: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if args.detach {
        if let Err(err) = detach_from_env() {
            eprintln!("ssh-obi-server: detach failed: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if let Err(err) = run_broker_stdio() {
        eprintln!("ssh-obi-server: broker failed: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
