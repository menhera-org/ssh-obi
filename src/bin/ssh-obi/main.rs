use std::process::ExitCode;

use ssh_obi::cli::parse_client_args;
use ssh_obi::client::run_client;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    let parsed = match parse_client_args(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("ssh-obi: {err}");
            return ExitCode::from(2);
        }
    };

    match run_client(&parsed) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ssh-obi: {err}");
            ExitCode::from(1)
        }
    }
}
