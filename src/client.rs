use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, ExitStatus as ProcessExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::bootstrap::{
    INSTALL_OK_MARKER, INSTALL_REQUIRED_MARKER, READY_MARKER, remote_shell_command,
};
use crate::cli::{ClientAction, ClientArgs};
use crate::protocol::{
    AttachSessionRequest, AttachedSession, DetachSessionRequest, ErrorMessage, ExitStatus, Frame,
    MessageType, NewSessionRequest, ProtocolError, PtyData, SessionBusy, SessionList,
    SessionListRequest, WindowSize,
};
use crate::session::{
    AutoSelection, SessionIdError, SessionInfo, auto_select, render_session_table,
};
use crate::terminal::{RawModeGuard, TerminalError};
use crate::transport::{FramedReader, FramedWriter};

const RECONNECT_DELAY: Duration = Duration::from_millis(250);

#[cfg(unix)]
type ClientRawModeGuard = RawModeGuard<std::io::Stdin>;

#[cfg(not(unix))]
type ClientRawModeGuard = RawModeGuard;

pub fn run_client(args: &ClientArgs) -> Result<ExitCode, ClientRunError> {
    if matches!(args.action, ClientAction::List | ClientAction::Detach) {
        return match run_client_attempt(args, &mut None, 0)? {
            ClientAttemptResult::Control(code) => Ok(code),
            ClientAttemptResult::Attached(_) => unreachable!("control action attached"),
        };
    }

    let mut attached_io = None;
    let mut next_args = args.clone();
    let mut known_session_id = args.session.clone();
    let mut reconnecting = false;
    let mut attempt_id = 0;

    loop {
        attempt_id += 1;
        let result = run_client_attempt(&next_args, &mut attached_io, attempt_id)?;
        let ClientAttemptResult::Attached(result) = result else {
            unreachable!("attach action returned a control result");
        };

        match result {
            Ok(success) => return Ok(success.exit_code),
            Err(failure) if is_ambiguous_disconnect(&failure.error) => {
                let reached_attached_session = failure.session_id.is_some();
                let Some(session_id) = failure.session_id.or_else(|| known_session_id.clone())
                else {
                    return Err(ClientRunError::ConnectionLostBeforeSessionKnown(Box::new(
                        failure.error,
                    )));
                };

                if reconnecting && !reached_attached_session {
                    return Err(ClientRunError::ReconnectFailed {
                        session_id,
                        source: Box::new(failure.error),
                    });
                }

                known_session_id = Some(session_id.clone());
                reconnecting = true;
                eprintln!("ssh-obi: connection lost; reconnecting session {session_id}");
                thread::sleep(RECONNECT_DELAY);

                next_args = args.clone();
                next_args.action = ClientAction::Attach;
                next_args.session = Some(session_id);
            }
            Err(failure) => return Err(failure.error),
        }
    }
}

fn run_client_attempt(
    args: &ClientArgs,
    attached_io: &mut Option<AttachedIo>,
    attempt_id: u64,
) -> Result<ClientAttemptResult, ClientRunError> {
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

    if let Err(err) = wait_for_bootstrap(&mut child_stdout, &mut child_stdin) {
        let _ = child.wait();
        return Err(err);
    }

    let initial_action = match handle_initial_action(args, &mut child_stdout, &mut child_stdin) {
        Ok(action) => action,
        Err(err) => {
            let _ = child.wait();
            return Err(err);
        }
    };
    if let InitialActionResult::Control(code) = initial_action {
        let status = child.wait().map_err(ClientRunError::WaitSsh)?;
        return Ok(ClientAttemptResult::Control(exit_code_with_ssh_status(
            code, status,
        )));
    }

    let InitialActionResult::Attached {
        session_id: initial_session_id,
    } = initial_action
    else {
        unreachable!("control action handled above");
    };
    let attached_io = ensure_attached_io(attached_io)?;
    let attached_result = run_attached_session(
        child_stdout,
        child_stdin,
        initial_session_id,
        attached_io,
        attempt_id,
    );
    let status = child.wait().map_err(ClientRunError::WaitSsh)?;
    Ok(ClientAttemptResult::Attached(attached_result.map(
        |mut success| {
            success.exit_code = exit_code_with_ssh_status(success.exit_code, status);
            success
        },
    )))
}

pub fn server_args_for_action(_args: &ClientArgs) -> Vec<&str> {
    Vec::new()
}

#[derive(Debug)]
enum ClientAttemptResult {
    Control(ExitCode),
    Attached(Result<AttachedSessionSuccess, AttachedSessionFailure>),
}

#[derive(Debug)]
struct AttachedSessionSuccess {
    exit_code: ExitCode,
}

#[derive(Debug)]
struct AttachedSessionFailure {
    session_id: Option<String>,
    error: ClientRunError,
}

impl AttachedSessionFailure {
    fn new(session_id: Option<String>, error: ClientRunError) -> Self {
        Self { session_id, error }
    }
}

#[derive(Debug)]
enum InitialActionResult {
    Control(ExitCode),
    Attached { session_id: Option<String> },
}

fn handle_initial_action<R, W>(
    args: &ClientArgs,
    reader: &mut R,
    writer: &mut W,
) -> Result<InitialActionResult, ClientRunError>
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
            Ok(InitialActionResult::Control(ExitCode::SUCCESS))
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
            Ok(InitialActionResult::Control(ExitCode::SUCCESS))
        }
        ClientAction::Attach => {
            let selection = if let Some(session_id) = &args.session {
                InitialClientSelection::Attach(session_id.clone())
            } else {
                select_session(reader, writer)?
            };

            let InitialClientSelection::Attach(session_id) = selection else {
                return Ok(InitialActionResult::Attached { session_id: None });
            };
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(
                MessageType::ATTACH_SESSION,
                0,
                &AttachSessionRequest {
                    session_id: session_id.clone(),
                },
            )?;
            framed_writer.flush()?;
            Ok(InitialActionResult::Attached {
                session_id: Some(session_id),
            })
        }
        ClientAction::New => {
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
            framed_writer.flush()?;
            Ok(InitialActionResult::Attached { session_id: None })
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

#[derive(Debug)]
struct AttachedIo {
    _raw_mode: Option<ClientRawModeGuard>,
    receiver: Receiver<AttachedEvent>,
    sender: Sender<AttachedEvent>,
    _stdin_thread: thread::JoinHandle<()>,
}

#[derive(Debug)]
enum AttachedEvent {
    Stdin(Vec<u8>),
    StdinEof,
    StdinError(io::Error),
    BrokerFrame {
        attempt_id: u64,
        result: Result<Option<Frame>, ProtocolError>,
    },
}

fn ensure_attached_io(attached_io: &mut Option<AttachedIo>) -> Result<&AttachedIo, ClientRunError> {
    if attached_io.is_none() {
        let raw_mode = enable_stdin_raw_mode()?;
        install_resize_handler();

        let (sender, receiver) = mpsc::channel();
        let stdin_sender = sender.clone();
        let stdin_thread = thread::spawn(move || read_stdin_events(stdin_sender));

        *attached_io = Some(AttachedIo {
            _raw_mode: raw_mode,
            receiver,
            sender,
            _stdin_thread: stdin_thread,
        });
    }

    Ok(attached_io.as_ref().expect("attached io initialized"))
}

fn read_stdin_events(sender: Sender<AttachedEvent>) {
    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 8192];

    loop {
        match stdin.read(&mut buf) {
            Ok(0) => {
                let _ = sender.send(AttachedEvent::StdinEof);
                return;
            }
            Ok(n) => {
                if sender
                    .send(AttachedEvent::Stdin(buf[..n].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                let _ = sender.send(AttachedEvent::StdinError(err));
                return;
            }
        }
    }
}

fn run_attached_session<R, W>(
    reader: R,
    writer: W,
    initial_session_id: Option<String>,
    attached_io: &AttachedIo,
    attempt_id: u64,
) -> Result<AttachedSessionSuccess, AttachedSessionFailure>
where
    R: Read + Send + 'static,
    W: Write,
{
    spawn_broker_reader(reader, attached_io.sender.clone(), attempt_id);
    let mut session_id = initial_session_id;
    let mut writer = FramedWriter::new(writer);
    let mut stdout = io::stdout().lock();
    let mut last_window_size = None;
    let mut stdin_open = true;

    send_window_size_if_changed(&mut writer, &mut last_window_size)
        .map_err(|err| AttachedSessionFailure::new(session_id.clone(), err))?;

    loop {
        if resize_pending() {
            send_window_size_if_changed(&mut writer, &mut last_window_size)
                .map_err(|err| AttachedSessionFailure::new(session_id.clone(), err))?;
        }

        let event = match attached_io.receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(AttachedSessionFailure::new(
                    session_id,
                    ClientRunError::UnexpectedBrokerEof,
                ));
            }
        };

        match event {
            AttachedEvent::Stdin(bytes) if stdin_open => {
                writer
                    .write_body(MessageType::PTY_DATA, 0, &PtyData { bytes })
                    .map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                writer.flush().map_err(|err| {
                    AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                })?;
            }
            AttachedEvent::Stdin(_) => {}
            AttachedEvent::StdinEof => {
                stdin_open = false;
            }
            AttachedEvent::StdinError(err) => {
                return Err(AttachedSessionFailure::new(
                    session_id,
                    ClientRunError::CopyStdin(err),
                ));
            }
            AttachedEvent::BrokerFrame {
                attempt_id: event_attempt_id,
                result,
            } if event_attempt_id == attempt_id => {
                let Some(frame) = result.map_err(|err| {
                    AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                })?
                else {
                    return Err(AttachedSessionFailure::new(
                        session_id,
                        ClientRunError::UnexpectedBrokerEof,
                    ));
                };

                if frame.msg_type() == MessageType::ATTACHED_SESSION {
                    let attached: AttachedSession = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                    if let Some(expected) = session_id.as_ref().cloned()
                        && expected != attached.session_id
                    {
                        return Err(AttachedSessionFailure::new(
                            session_id,
                            ClientRunError::SessionMismatch {
                                expected,
                                actual: attached.session_id,
                            },
                        ));
                    }
                    session_id = Some(attached.session_id);
                    continue;
                }

                if frame.msg_type() == MessageType::PTY_DATA {
                    let data: PtyData = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                    stdout.write_all(&data.bytes).map_err(|err| {
                        AttachedSessionFailure::new(
                            session_id.clone(),
                            ClientRunError::CopyStdout(err),
                        )
                    })?;
                    stdout.flush().map_err(|err| {
                        AttachedSessionFailure::new(
                            session_id.clone(),
                            ClientRunError::CopyStdout(err),
                        )
                    })?;
                    continue;
                }

                if frame.msg_type() == MessageType::CLIENT_SHOULD_EXIT {
                    return Ok(AttachedSessionSuccess {
                        exit_code: ExitCode::SUCCESS,
                    });
                }

                if frame.msg_type() == MessageType::EXIT_STATUS {
                    let status: ExitStatus = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                    let exit_code = exit_code_from_status(status)
                        .map_err(|err| AttachedSessionFailure::new(session_id.clone(), err))?;
                    return Ok(AttachedSessionSuccess { exit_code });
                }

                if frame.msg_type() == MessageType::SESSION_BUSY {
                    let busy: SessionBusy = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                    return Err(AttachedSessionFailure::new(
                        session_id,
                        ClientRunError::SessionBusy(busy.session_id),
                    ));
                }

                if frame.msg_type() == MessageType::ERROR {
                    let error: ErrorMessage = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(session_id.clone(), ClientRunError::from(err))
                    })?;
                    return Err(AttachedSessionFailure::new(
                        session_id,
                        ClientRunError::BrokerError(error.message),
                    ));
                }

                return Err(AttachedSessionFailure::new(
                    session_id,
                    ClientRunError::UnexpectedBrokerMessage(frame.msg_type().get()),
                ));
            }
            AttachedEvent::BrokerFrame { .. } => {}
        }
    }
}

fn spawn_broker_reader<R>(reader: R, sender: Sender<AttachedEvent>, attempt_id: u64)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = FramedReader::new(reader);
        loop {
            let result = reader.read_frame();
            let should_stop = !matches!(result, Ok(Some(_)));
            if sender
                .send(AttachedEvent::BrokerFrame { attempt_id, result })
                .is_err()
            {
                return;
            }
            if should_stop {
                return;
            }
        }
    });
}

#[cfg(unix)]
fn enable_stdin_raw_mode() -> Result<Option<ClientRawModeGuard>, ClientRunError> {
    RawModeGuard::enable(io::stdin()).map_err(ClientRunError::Terminal)
}

#[cfg(not(unix))]
fn enable_stdin_raw_mode() -> Result<Option<ClientRawModeGuard>, ClientRunError> {
    RawModeGuard::enable().map_err(ClientRunError::Terminal)
}

fn send_window_size_if_changed<W>(
    writer: &mut FramedWriter<W>,
    last_window_size: &mut Option<WindowSize>,
) -> Result<(), ClientRunError>
where
    W: Write,
{
    let Some(window_size) = current_window_size().map_err(ClientRunError::WindowSize)? else {
        return Ok(());
    };

    if *last_window_size == Some(window_size.clone()) {
        return Ok(());
    }

    writer.write_body(MessageType::WINDOW_SIZE, 0, &window_size)?;
    writer.flush()?;
    *last_window_size = Some(window_size);
    Ok(())
}

#[cfg(unix)]
fn current_window_size() -> io::Result<Option<WindowSize>> {
    use std::os::fd::AsRawFd;

    let mut winsize = std::mem::MaybeUninit::<nix::libc::winsize>::uninit();
    let result = unsafe {
        nix::libc::ioctl(
            io::stdin().as_raw_fd(),
            nix::libc::TIOCGWINSZ,
            winsize.as_mut_ptr(),
        )
    };

    if result < 0 {
        let err = io::Error::last_os_error();
        if matches!(
            err.raw_os_error(),
            Some(code) if code == nix::libc::ENOTTY || code == nix::libc::EINVAL
        ) {
            return Ok(None);
        }
        return Err(err);
    }

    let winsize = unsafe { winsize.assume_init() };
    if winsize.ws_row == 0 || winsize.ws_col == 0 {
        return Ok(None);
    }

    Ok(Some(WindowSize {
        rows: winsize.ws_row,
        cols: winsize.ws_col,
        pixel_width: winsize.ws_xpixel,
        pixel_height: winsize.ws_ypixel,
    }))
}

#[cfg(not(unix))]
fn current_window_size() -> io::Result<Option<WindowSize>> {
    Ok(None)
}

#[cfg(unix)]
static RESIZE_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(unix)]
static RESIZE_HANDLER_INSTALLED: std::sync::Once = std::sync::Once::new();

#[cfg(unix)]
extern "C" fn handle_sigwinch(_: nix::libc::c_int) {
    RESIZE_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn install_resize_handler() {
    RESIZE_HANDLER_INSTALLED.call_once(|| {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

        let action = SigAction::new(
            SigHandler::Handler(handle_sigwinch),
            SaFlags::empty(),
            SigSet::empty(),
        );
        let _ = unsafe { sigaction(Signal::SIGWINCH, &action) };
    });
}

#[cfg(not(unix))]
fn install_resize_handler() {}

#[cfg(unix)]
fn resize_pending() -> bool {
    RESIZE_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(unix))]
fn resize_pending() -> bool {
    false
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

fn exit_code_with_ssh_status(code: ExitCode, status: ProcessExitStatus) -> ExitCode {
    if status.success() {
        return code;
    }

    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map(ExitCode::from)
        .unwrap_or(ExitCode::from(1))
}

fn is_ambiguous_disconnect(error: &ClientRunError) -> bool {
    matches!(
        error,
        ClientRunError::UnexpectedBrokerEof
            | ClientRunError::Protocol(ProtocolError::Io(_))
            | ClientRunError::Protocol(ProtocolError::TruncatedHeader)
            | ClientRunError::Protocol(ProtocolError::TruncatedPayload { .. })
    )
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
    SessionMismatch {
        expected: String,
        actual: String,
    },
    ConnectionLostBeforeSessionKnown(Box<ClientRunError>),
    ReconnectFailed {
        session_id: String,
        source: Box<ClientRunError>,
    },
    Protocol(ProtocolError),
    InvalidSession(SessionIdError),
    InvalidSessionSelection(String),
    MissingSessionForDetach,
    WaitSsh(io::Error),
    CopyStdin(io::Error),
    CopyStdout(io::Error),
    Terminal(TerminalError),
    WindowSize(io::Error),
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
            Self::SessionMismatch { expected, actual } => {
                write!(
                    f,
                    "broker attached session {actual}, but client requested {expected}"
                )
            }
            Self::ConnectionLostBeforeSessionKnown(source) => {
                write!(
                    f,
                    "connection lost before the broker identified the session: {source}"
                )
            }
            Self::ReconnectFailed { session_id, source } => {
                write!(f, "failed to reconnect session {session_id}: {source}")
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
            Self::Terminal(err) => write!(f, "{err}"),
            Self::WindowSize(err) => write!(f, "failed to read terminal window size: {err}"),
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
            | Self::CopyStdout(err)
            | Self::WindowSize(err) => Some(err),
            Self::Terminal(err) => Some(err),
            Self::Protocol(err) => Some(err),
            Self::InvalidSession(err) => Some(err),
            Self::ConnectionLostBeforeSessionKnown(err) => Some(err),
            Self::ReconnectFailed { source, .. } => Some(source),
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

        let action = handle_initial_action(
            &args(ClientAction::Attach),
            &mut input.as_slice(),
            &mut output,
        )
        .unwrap();
        assert!(matches!(
            action,
            InitialActionResult::Attached { session_id: None }
        ));

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

        let action = handle_initial_action(&args, &mut input.as_slice(), &mut output).unwrap();
        assert!(matches!(
            action,
            InitialActionResult::Attached {
                session_id: Some(id)
            } if id == "aaaaaaaa"
        ));

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

        let action =
            handle_initial_action(&args(ClientAction::New), &mut input.as_slice(), &mut output)
                .unwrap();
        assert!(matches!(
            action,
            InitialActionResult::Attached { session_id: None }
        ));

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
        let action = handle_initial_action(
            &args(ClientAction::List),
            &mut input.as_slice(),
            &mut output,
        )
        .unwrap();
        assert!(matches!(
            action,
            InitialActionResult::Control(code) if code == ExitCode::SUCCESS
        ));

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::SESSION_LIST_REQUEST);
        let request: SessionListRequest = frame.decode_body().unwrap();
        assert!(!request.continue_after_response);
    }
}
