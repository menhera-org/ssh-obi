use std::process::ExitCode;

use clap::Parser;
use ssh_obi::protocol::supports_protocol_baseline;

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
        eprintln!("ssh-obi-server: detach mode is not implemented yet");
        return ExitCode::from(70);
    }

    eprintln!("ssh-obi-server: broker mode is not implemented yet");
    ExitCode::from(70)
}
