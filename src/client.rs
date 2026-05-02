use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

use crate::bootstrap::{
    INSTALL_OK_MARKER, INSTALL_REQUIRED_MARKER, READY_MARKER, remote_shell_command,
};
use crate::cli::{ClientAction, ClientArgs};
use crate::protocol::{
    AttachSessionRequest, DetachSessionRequest, ErrorMessage, ExitStatus, MessageType,
    NewSessionRequest, ProtocolError, PtyData, SessionBusy, SessionList, SessionListRequest,
};
use crate::session::{
    AutoSelection, SessionIdError, SessionInfo, auto_select, render_session_table,
};
use crate::transport::{FramedReader, FramedWriter};

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

    if let Some(code) = handle_control_action(args, &mut child_stdout, &mut child_stdin)? {
        let status = child.wait().map_err(ClientRunError::WaitSsh)?;
        return Ok(if status.success() {
            code
        } else {
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map(ExitCode::from)
                .unwrap_or(ExitCode::from(1))
        });
    }

    let attached_status = run_attached_session(child_stdout, child_stdin)?;
    let status = child.wait().map_err(ClientRunError::WaitSsh)?;
    Ok(if status.success() {
        attached_status
    } else {
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map(ExitCode::from)
            .unwrap_or(ExitCode::from(1))
    })
}

pub fn server_args_for_action(_args: &ClientArgs) -> Vec<&str> {
    Vec::new()
}

fn handle_control_action<R, W>(
    args: &ClientArgs,
    reader: &mut R,
    writer: &mut W,
) -> Result<Option<ExitCode>, ClientRunError>
where
    R: Read,
    W: Write,
{
    match args.action {
        ClientAction::List => {
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(
                MessageType::SESSION_LIST_REQUEST,
                0,
                &SessionListRequest {
                    continue_after_response: false,
                },
            )?;
            framed_writer.flush()?;

            let mut framed_reader = FramedReader::new(reader);
            let frame = framed_reader
                .read_frame()?
                .ok_or(ClientRunError::UnexpectedBrokerEof)?;

            if frame.msg_type() != MessageType::SESSION_LIST {
                return Err(ClientRunError::UnexpectedBrokerMessage(
                    frame.msg_type().get(),
                ));
            }

            let list: SessionList = frame.decode_body()?;
            let sessions = list
                .sessions
                .into_iter()
                .map(SessionInfo::from_record)
                .collect::<Result<Vec<_>, _>>()?;
            print!("{}", render_session_table(&sessions, false));
            Ok(Some(ExitCode::SUCCESS))
        }
        ClientAction::Detach => {
            let session_id = args
                .session
                .clone()
                .ok_or(ClientRunError::MissingSessionForDetach)?;
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(
                MessageType::DETACH,
                0,
                &DetachSessionRequest { session_id },
            )?;
            framed_writer.flush()?;
            Ok(Some(ExitCode::SUCCESS))
        }
        ClientAction::Attach => {
            let selection = if let Some(session_id) = &args.session {
                InitialClientSelection::Attach(session_id.clone())
            } else {
                select_session(reader, writer)?
            };

            let InitialClientSelection::Attach(session_id) = selection else {
                return Ok(None);
            };
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(
                MessageType::ATTACH_SESSION,
                0,
                &AttachSessionRequest { session_id },
            )?;
            framed_writer.flush()?;
            Ok(None)
        }
        ClientAction::New => {
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
            framed_writer.flush()?;
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InitialClientSelection {
    Attach(String),
    NewAlreadySent,
}

fn select_session<R, W>(
    reader: &mut R,
    writer: &mut W,
) -> Result<InitialClientSelection, ClientRunError>
where
    R: Read,
    W: Write,
{
    {
        let mut framed_writer = FramedWriter::new(&mut *writer);
        framed_writer.write_body(
            MessageType::SESSION_LIST_REQUEST,
            0,
            &SessionListRequest {
                continue_after_response: true,
            },
        )?;
        framed_writer.flush()?;
    }

    let mut framed_reader = FramedReader::new(reader);
    let frame = framed_reader
        .read_frame()?
        .ok_or(ClientRunError::UnexpectedBrokerEof)?;

    if frame.msg_type() != MessageType::SESSION_LIST {
        return Err(ClientRunError::UnexpectedBrokerMessage(
            frame.msg_type().get(),
        ));
    }

    let list: SessionList = frame.decode_body()?;
    let sessions = list
        .sessions
        .into_iter()
        .map(SessionInfo::from_record)
        .collect::<Result<Vec<_>, _>>()?;

    match auto_select(&sessions) {
        AutoSelection::NewSession => {
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
            framed_writer.flush()?;
            Ok(InitialClientSelection::NewAlreadySent)
        }
        AutoSelection::Attach(session_id) => {
            Ok(InitialClientSelection::Attach(session_id.to_string()))
        }
        AutoSelection::Prompt => prompt_session_selection(&sessions, writer),
    }
}

fn prompt_session_selection<W>(
    sessions: &[SessionInfo],
    writer: &mut W,
) -> Result<InitialClientSelection, ClientRunError>
where
    W: Write,
{
    eprintln!("Select a session to attach:\n");
    eprint!("{}", render_session_table(sessions, true));
    eprint!("> ");
    io::stderr().flush().map_err(ClientRunError::Prompt)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(ClientRunError::Prompt)?;
    let answer = answer.trim();

    if answer == "n" {
        let mut framed_writer = FramedWriter::new(writer);
        framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
        framed_writer.flush()?;
        return Ok(InitialClientSelection::NewAlreadySent);
    }

    let selector = answer
        .parse::<usize>()
        .map_err(|_| ClientRunError::InvalidSessionSelection(answer.to_string()))?;
    if selector == 0 {
        return Err(ClientRunError::InvalidSessionSelection(answer.to_string()));
    }

    let Some(session) = sessions
        .iter()
        .filter(|session| session.is_selectable())
        .nth(selector - 1)
    else {
        return Err(ClientRunError::InvalidSessionSelection(answer.to_string()));
    };

    Ok(InitialClientSelection::Attach(session.id.to_string()))
}

fn run_attached_session<R, W>(reader: R, writer: W) -> Result<ExitCode, ClientRunError>
where
    R: Read,
    W: Write + Send + 'static,
{
    let _stdin_thread = thread::spawn(move || copy_stdin_to_pty(writer));
    let mut framed_reader = FramedReader::new(reader);
    let mut stdout = io::stdout().lock();

    while let Some(frame) = framed_reader.read_frame()? {
        if frame.msg_type() == MessageType::PTY_DATA {
            let data: PtyData = frame.decode_body()?;
            stdout
                .write_all(&data.bytes)
                .map_err(ClientRunError::CopyStdout)?;
            stdout.flush().map_err(ClientRunError::CopyStdout)?;
            continue;
        }

        if frame.msg_type() == MessageType::CLIENT_SHOULD_EXIT {
            return Ok(ExitCode::SUCCESS);
        }

        if frame.msg_type() == MessageType::EXIT_STATUS {
            let status: ExitStatus = frame.decode_body()?;
            return exit_code_from_status(status);
        }

        if frame.msg_type() == MessageType::SESSION_BUSY {
            let busy: SessionBusy = frame.decode_body()?;
            return Err(ClientRunError::SessionBusy(busy.session_id));
        }

        if frame.msg_type() == MessageType::ERROR {
            let error: ErrorMessage = frame.decode_body()?;
            return Err(ClientRunError::BrokerError(error.message));
        }

        return Err(ClientRunError::UnexpectedBrokerMessage(
            frame.msg_type().get(),
        ));
    }

    Err(ClientRunError::UnexpectedBrokerEof)
}

fn copy_stdin_to_pty<W>(writer: W) -> Result<(), ClientRunError>
where
    W: Write,
{
    let mut writer = FramedWriter::new(writer);
    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 8192];

    loop {
        match stdin.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                writer.write_body(
                    MessageType::PTY_DATA,
                    0,
                    &PtyData {
                        bytes: buf[..n].to_vec(),
                    },
                )?;
                writer.flush()?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(ClientRunError::CopyStdin(err)),
        }
    }
}

fn exit_code_from_status(status: ExitStatus) -> Result<ExitCode, ClientRunError> {
    if let Some(code) = status.code {
        return Ok(u8::try_from(code)
            .ok()
            .map(ExitCode::from)
            .unwrap_or(ExitCode::from(1)));
    }

    if status.signal.is_some() {
        return Ok(ExitCode::from(1));
    }

    Err(ClientRunError::MissingExitStatus)
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
    UnexpectedBrokerEof,
    UnexpectedBrokerMessage(u8),
    BrokerError(String),
    SessionBusy(String),
    Protocol(ProtocolError),
    InvalidSession(SessionIdError),
    InvalidSessionSelection(String),
    MissingSessionForDetach,
    WaitSsh(io::Error),
    CopyStdin(io::Error),
    CopyStdout(io::Error),
    MissingExitStatus,
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
            Self::UnexpectedBrokerEof => write!(f, "broker ended before sending a response"),
            Self::UnexpectedBrokerMessage(msg_type) => {
                write!(f, "broker returned unexpected message type {msg_type}")
            }
            Self::BrokerError(message) => write!(f, "broker returned error: {message}"),
            Self::SessionBusy(session_id) => {
                write!(f, "session {session_id} is currently in use")
            }
            Self::Protocol(err) => write!(f, "protocol error: {err}"),
            Self::InvalidSession(err) => write!(f, "invalid session from broker: {err}"),
            Self::InvalidSessionSelection(selection) => {
                write!(f, "invalid session selection: {selection}")
            }
            Self::MissingSessionForDetach => write!(f, "--detach requires --session ID"),
            Self::WaitSsh(err) => write!(f, "failed to wait for ssh: {err}"),
            Self::CopyStdin(err) => write!(f, "failed to copy stdin to session: {err}"),
            Self::CopyStdout(err) => write!(f, "failed to copy ssh stdout: {err}"),
            Self::MissingExitStatus => write!(f, "session ended without an exit status"),
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
            | Self::CopyStdin(err)
            | Self::CopyStdout(err) => Some(err),
            Self::Protocol(err) => Some(err),
            Self::InvalidSession(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProtocolError> for ClientRunError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<SessionIdError> for ClientRunError {
    fn from(value: SessionIdError) -> Self {
        Self::InvalidSession(value)
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
    fn attach_action_with_no_sessions_requests_new_session() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::SESSION_LIST,
                    0,
                    &SessionList {
                        sessions: Vec::new(),
                    },
                )
                .unwrap();
        }
        let mut output = Vec::new();

        let code = handle_control_action(
            &args(ClientAction::Attach),
            &mut input.as_slice(),
            &mut output,
        )
        .unwrap();
        assert_eq!(code, None);

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::SESSION_LIST_REQUEST);
        let request: SessionListRequest = frame.decode_body().unwrap();
        assert!(request.continue_after_response);

        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::NEW_SESSION);
        let _: NewSessionRequest = frame.decode_body().unwrap();
    }

    #[test]
    fn attach_action_writes_attach_request() {
        let mut args = args(ClientAction::Attach);
        args.session = Some("aaaaaaaa".to_string());
        let input = Vec::new();
        let mut output = Vec::new();

        let code = handle_control_action(&args, &mut input.as_slice(), &mut output).unwrap();
        assert_eq!(code, None);

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::ATTACH_SESSION);
        let request: AttachSessionRequest = frame.decode_body().unwrap();
        assert_eq!(request.session_id, "aaaaaaaa");
    }

    #[test]
    fn new_action_writes_new_session_request() {
        let input = Vec::new();
        let mut output = Vec::new();

        let code =
            handle_control_action(&args(ClientAction::New), &mut input.as_slice(), &mut output)
                .unwrap();
        assert_eq!(code, None);

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::NEW_SESSION);
        let _: NewSessionRequest = frame.decode_body().unwrap();
    }

    #[test]
    fn line_reader_does_not_require_utf8() {
        let mut bytes = &b"OBI-\xff\n"[..];
        let line = read_line_lossy(&mut bytes).unwrap().unwrap();
        assert_eq!(line, "OBI-\u{fffd}\n");
    }

    #[test]
    fn list_action_writes_request_and_prints_response() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::SESSION_LIST,
                    0,
                    &SessionList {
                        sessions: Vec::new(),
                    },
                )
                .unwrap();
        }

        let mut output = Vec::new();
        let code = handle_control_action(
            &args(ClientAction::List),
            &mut input.as_slice(),
            &mut output,
        )
        .unwrap();
        assert_eq!(code, Some(ExitCode::SUCCESS));

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::SESSION_LIST_REQUEST);
        let request: SessionListRequest = frame.decode_body().unwrap();
        assert!(!request.continue_after_response);
    }
}
