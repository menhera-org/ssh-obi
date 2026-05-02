use std::process::ExitCode;

use clap::Parser;
use ssh_obi::protocol::supports_protocol_baseline;
use ssh_obi::server::detach_from_env;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(long, conflicts_with_all = ["detach", "protocol_check"])]
    daemon: bool,

    #[arg(long, conflicts_with_all = ["daemon", "protocol_check"])]
    detach: bool,

    #[arg(long = "protocol-check", value_name = "BASELINE", conflicts_with_all = ["daemon", "detach"])]
    protocol_check: Option<String>,
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
        eprintln!("ssh-obi-server: daemon mode is not implemented yet");
        return ExitCode::from(70);
    }

    if args.detach {
        if let Err(err) = detach_from_env() {
            eprintln!("ssh-obi-server: detach failed: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    eprintln!("ssh-obi-server: broker mode is not implemented yet");
    ExitCode::from(70)
}
