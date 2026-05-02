use std::ffi::OsString;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientArgs {
    pub action: ClientAction,
    pub session: Option<String>,
    pub ssh_args: Vec<OsString>,
    pub destination: OsString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAction {
    Attach,
    New,
    List,
    Detach,
}

impl ClientArgs {
    pub fn ssh_command_args(&self, remote_command: &str) -> Vec<OsString> {
        let mut args = Vec::with_capacity(self.ssh_args.len() + 3);
        if !self.ssh_args.iter().any(|arg| arg == "-T") {
            args.push(OsString::from("-T"));
        }
        args.extend(self.ssh_args.iter().cloned());
        args.push(self.destination.clone());
        args.push(OsString::from(remote_command));
        args
    }
}

pub fn parse_client_args<I, S>(args: I) -> Result<ClientArgs, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut action = ClientAction::Attach;
    let mut session = None;
    let mut ssh_args = Vec::new();
    let mut destination = None;
    let mut iter = args.into_iter().map(Into::into).peekable();

    while let Some(arg) = iter.next() {
        if destination.is_some() {
            return Err(CliError::RemoteCommandNotSupported);
        }

        if arg == "--" {
            let Some(dest) = iter.next() else {
                return Err(CliError::MissingDestination);
            };
            destination = Some(dest);
            if iter.peek().is_some() {
                return Err(CliError::RemoteCommandNotSupported);
            }
            break;
        }

        if arg == "--new" {
            set_action(&mut action, ClientAction::New)?;
            continue;
        }

        if arg == "--list" {
            set_action(&mut action, ClientAction::List)?;
            continue;
        }

        if arg == "--detach" {
            set_action(&mut action, ClientAction::Detach)?;
            continue;
        }

        if arg == "--session" {
            let Some(value) = iter.next() else {
                return Err(CliError::MissingOptionValue("--session"));
            };
            session = Some(os_to_string(value, "--session")?);
            continue;
        }

        if starts_with_dash(&arg) {
            if is_standalone_ssh_flag(&arg) {
                ssh_args.push(arg);
                continue;
            }

            if let Some(option) = ssh_option_needing_value(&arg) {
                ssh_args.push(arg);
                let Some(value) = iter.next() else {
                    return Err(CliError::MissingOptionValue(option));
                };
                ssh_args.push(value);
                continue;
            }

            if is_joined_ssh_option(&arg) {
                ssh_args.push(arg);
                continue;
            }

            return Err(CliError::UnsupportedOption(
                arg.to_string_lossy().into_owned(),
            ));
        }

        destination = Some(arg);
    }

    let destination = destination.ok_or(CliError::MissingDestination)?;

    if action == ClientAction::Detach && session.is_none() {
        return Err(CliError::DetachRequiresSession);
    }

    Ok(ClientArgs {
        action,
        session,
        ssh_args,
        destination,
    })
}

fn set_action(action: &mut ClientAction, next: ClientAction) -> Result<(), CliError> {
    if *action != ClientAction::Attach {
        return Err(CliError::ConflictingActions);
    }
    *action = next;
    Ok(())
}

fn starts_with_dash(arg: &OsString) -> bool {
    arg.as_encoded_bytes().first() == Some(&b'-')
}

fn os_to_string(value: OsString, option: &'static str) -> Result<String, CliError> {
    value
        .into_string()
        .map_err(|_| CliError::NonUtf8OptionValue(option))
}

fn is_standalone_ssh_flag(arg: &OsString) -> bool {
    matches!(
        arg.to_str(),
        Some(
            "-4" | "-6"
                | "-A"
                | "-a"
                | "-C"
                | "-f"
                | "-G"
                | "-g"
                | "-K"
                | "-k"
                | "-N"
                | "-n"
                | "-q"
                | "-T"
                | "-t"
                | "-V"
                | "-X"
                | "-x"
                | "-Y"
                | "-y"
        )
    ) || arg
        .to_str()
        .is_some_and(|value| value.len() > 1 && value[1..].bytes().all(|byte| byte == b'v'))
}

fn ssh_option_needing_value(arg: &OsString) -> Option<&'static str> {
    match arg.to_str()? {
        "-B" => Some("-B"),
        "-b" => Some("-b"),
        "-c" => Some("-c"),
        "-D" => Some("-D"),
        "-E" => Some("-E"),
        "-e" => Some("-e"),
        "-F" => Some("-F"),
        "-I" => Some("-I"),
        "-i" => Some("-i"),
        "-J" => Some("-J"),
        "-L" => Some("-L"),
        "-l" => Some("-l"),
        "-m" => Some("-m"),
        "-O" => Some("-O"),
        "-o" => Some("-o"),
        "-p" => Some("-p"),
        "-Q" => Some("-Q"),
        "-R" => Some("-R"),
        "-S" => Some("-S"),
        "-W" => Some("-W"),
        "-w" => Some("-w"),
        _ => None,
    }
}

fn is_joined_ssh_option(arg: &OsString) -> bool {
    let Some(value) = arg.to_str() else {
        return false;
    };

    matches!(
        value.as_bytes(),
        [
            b'-',
            b'B' | b'b'
                | b'c'
                | b'D'
                | b'E'
                | b'e'
                | b'F'
                | b'I'
                | b'i'
                | b'J'
                | b'L'
                | b'l'
                | b'm'
                | b'O'
                | b'o'
                | b'p'
                | b'Q'
                | b'R'
                | b'S'
                | b'W'
                | b'w',
            ..
        ]
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingDestination,
    MissingOptionValue(&'static str),
    NonUtf8OptionValue(&'static str),
    UnsupportedOption(String),
    ConflictingActions,
    DetachRequiresSession,
    RemoteCommandNotSupported,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDestination => write!(f, "missing SSH destination"),
            Self::MissingOptionValue(option) => write!(f, "missing value for {option}"),
            Self::NonUtf8OptionValue(option) => write!(f, "non-UTF-8 value for {option}"),
            Self::UnsupportedOption(option) => write!(f, "unsupported option: {option}"),
            Self::ConflictingActions => write!(f, "choose only one of --new, --list, or --detach"),
            Self::DetachRequiresSession => write!(f, "--detach requires --session ID"),
            Self::RemoteCommandNotSupported => {
                write!(f, "remote command arguments are not supported")
            }
        }
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_ssh_options_and_destination() {
        let parsed = parse_client_args(["-p", "2222", "-Jjump", "-vv", "host"]).unwrap();

        assert_eq!(parsed.action, ClientAction::Attach);
        assert_eq!(parsed.destination, "host");
        assert_eq!(parsed.ssh_args, ["-p", "2222", "-Jjump", "-vv"]);
    }

    #[test]
    fn detach_requires_session() {
        let err = parse_client_args(["--detach", "host"]).unwrap_err();
        assert_eq!(err, CliError::DetachRequiresSession);
    }

    #[test]
    fn remote_commands_are_rejected() {
        let err = parse_client_args(["host", "uptime"]).unwrap_err();
        assert_eq!(err, CliError::RemoteCommandNotSupported);
    }

    #[test]
    fn ssh_command_forces_no_tty() {
        let parsed = parse_client_args(["-p", "2222", "host"]).unwrap();
        let command = parsed.ssh_command_args("remote");

        assert_eq!(command, ["-T", "-p", "2222", "host", "remote"]);
    }
}
