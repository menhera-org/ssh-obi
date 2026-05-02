use std::process::ExitCode;

use ssh_obi::cli::{ClientAction, parse_client_args};

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    let parsed = match parse_client_args(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("ssh-obi: {err}");
            return ExitCode::from(2);
        }
    };

    let action = match parsed.action {
        ClientAction::Attach => "attach",
        ClientAction::New => "new",
        ClientAction::List => "list",
        ClientAction::Detach => "detach",
    };

    eprintln!(
        "ssh-obi: {action} is parsed but not implemented yet for {}",
        parsed.destination.to_string_lossy()
    );
    ExitCode::from(70)
}
