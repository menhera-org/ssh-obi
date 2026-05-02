use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

use crate::bootstrap::{
    INSTALL_OK_MARKER, INSTALL_REQUIRED_MARKER, READY_MARKER, remote_shell_command,
};
use crate::cli::ClientArgs;

pub fn run_client(args: &ClientArgs) -> Result<ExitCode, ClientRunError> {
    let server_args = server_args_for_action(args);
    let remote_command = remote_shell_command(&server_args);
    let ssh_args = args.ssh_command_args(&remote_command);

    let mut child = Command::new("ssh")
        .args(&ssh_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(ClientRunError::SpawnSsh)?;

    let mut child_stdin = child
        .stdin
        .take()
        .ok_or(ClientRunError::MissingChildStdin)?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or(ClientRunError::MissingChildStdout)?;

    wait_for_bootstrap(&mut child_stdout, &mut child_stdin)?;

    let stdout_thread = thread::spawn(move || -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        io::copy(&mut child_stdout, &mut stdout)?;
        stdout.flush()
    });

    let status = child.wait().map_err(ClientRunError::WaitSsh)?;
    stdout_thread
        .join()
        .map_err(|_| ClientRunError::StdoutThreadPanicked)?
        .map_err(ClientRunError::CopyStdout)?;

    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map(ExitCode::from)
        .unwrap_or(ExitCode::from(1)))
}

pub fn server_args_for_action(_args: &ClientArgs) -> Vec<&str> {
    Vec::new()
}

fn wait_for_bootstrap<R, W>(reader: &mut R, writer: &mut W) -> Result<(), ClientRunError>
where
    R: Read,
    W: Write,
{
    loop {
        let line = read_line_lossy(reader)?.ok_or(ClientRunError::UnexpectedBootstrapEof)?;
        let line = line.trim_end_matches(['\r', '\n']);

        if line == READY_MARKER {
            return Ok(());
        }

        if line == INSTALL_REQUIRED_MARKER {
            if confirm_install()? {
                writeln!(writer, "{INSTALL_OK_MARKER}").map_err(ClientRunError::WriteBootstrap)?;
                writer.flush().map_err(ClientRunError::WriteBootstrap)?;
                continue;
            }
            return Err(ClientRunError::InstallDeclined);
        }

        if let Some(message) = line.strip_prefix("OBI-ERROR ") {
            return Err(ClientRunError::RemoteBootstrap(message.to_string()));
        }

        if line.starts_with("OBI-") {
            return Err(ClientRunError::RemoteBootstrap(line.to_string()));
        }
    }
}

fn read_line_lossy<R: Read>(reader: &mut R) -> Result<Option<String>, ClientRunError> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match reader.read(&mut byte) {
            Ok(0) if bytes.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(1) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Ok(_) => unreachable!("one-byte read returned more than one byte"),
            Err(err) => return Err(ClientRunError::ReadBootstrap(err)),
        }
    }

    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn confirm_install() -> Result<bool, ClientRunError> {
    eprint!("installing ssh-obi on host, continue? [Y/n] ");
    io::stderr().flush().map_err(ClientRunError::Prompt)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(ClientRunError::Prompt)?;

    Ok(matches!(
        answer.trim(),
        "" | "y" | "Y" | "yes" | "YES" | "Yes"
    ))
}

#[derive(Debug)]
pub enum ClientRunError {
    SpawnSsh(io::Error),
    MissingChildStdin,
    MissingChildStdout,
    UnexpectedBootstrapEof,
    ReadBootstrap(io::Error),
    WriteBootstrap(io::Error),
    Prompt(io::Error),
    InstallDeclined,
    RemoteBootstrap(String),
    WaitSsh(io::Error),
    CopyStdout(io::Error),
    StdoutThreadPanicked,
}

impl fmt::Display for ClientRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpawnSsh(err) => write!(f, "failed to start ssh: {err}"),
            Self::MissingChildStdin => write!(f, "failed to capture ssh stdin"),
            Self::MissingChildStdout => write!(f, "failed to capture ssh stdout"),
            Self::UnexpectedBootstrapEof => {
                write!(f, "remote bootstrap ended before server became ready")
            }
            Self::ReadBootstrap(err) => write!(f, "failed to read bootstrap output: {err}"),
            Self::WriteBootstrap(err) => write!(f, "failed to write bootstrap input: {err}"),
            Self::Prompt(err) => write!(f, "failed to read install confirmation: {err}"),
            Self::InstallDeclined => write!(f, "install declined"),
            Self::RemoteBootstrap(message) => write!(f, "remote bootstrap failed: {message}"),
            Self::WaitSsh(err) => write!(f, "failed to wait for ssh: {err}"),
            Self::CopyStdout(err) => write!(f, "failed to copy ssh stdout: {err}"),
            Self::StdoutThreadPanicked => write!(f, "stdout copy thread panicked"),
        }
    }
}

impl std::error::Error for ClientRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpawnSsh(err)
            | Self::ReadBootstrap(err)
            | Self::WriteBootstrap(err)
            | Self::Prompt(err)
            | Self::WaitSsh(err)
            | Self::CopyStdout(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClientAction, ClientArgs};
    use std::ffi::OsString;

    fn args(action: ClientAction) -> ClientArgs {
        ClientArgs {
            action,
            session: None,
            ssh_args: Vec::new(),
            destination: OsString::from("host"),
        }
    }

    #[test]
    fn detach_action_uses_broker_default() {
        assert!(server_args_for_action(&args(ClientAction::Detach)).is_empty());
    }

    #[test]
    fn attach_action_uses_broker_default() {
        assert!(server_args_for_action(&args(ClientAction::Attach)).is_empty());
    }

    #[test]
    fn line_reader_does_not_require_utf8() {
        let mut bytes = &b"OBI-\xff\n"[..];
        let line = read_line_lossy(&mut bytes).unwrap().unwrap();
        assert_eq!(line, "OBI-\u{fffd}\n");
    }
}
