use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, ExitStatus as ProcessExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::bootstrap::{
    INSTALL_OK_MARKER, INSTALL_REQUIRED_MARKER, READY_MARKER, remote_shell_command,
    terminal_type_from_env,
};
use crate::cli::{ClientAction, ClientArgs};
use crate::protocol::{
    AttachSessionRequest, AttachedSession, CAP_INITIAL_WINDOW_SIZE_V1, CAP_KEEPALIVE_V1,
    Capabilities, DetachSessionRequest, ErrorMessage, ExitStatus, Frame, Keepalive, MessageType,
    NewSessionRequest, ProtocolError, PtyData, SessionBusy, SessionList, SessionListRequest,
    WindowSize,
};
use crate::session::{
    AutoSelection, ExistingSelection, SessionIdError, SessionInfo, auto_select,
    auto_select_existing, render_detach_connect_table, render_session_table,
};
use crate::terminal::{RawModeGuard, TerminalError};
use crate::transport::{FramedReader, FramedWriter};

const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(120);
const RECONNECT_SLEEP_QUANTUM: Duration = Duration::from_secs(1);
const STDIN_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
#[cfg(test)]
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(test)]
const KEEPALIVE_TIMEOUT: Duration = Duration::from_millis(30);

#[cfg(unix)]
type ClientRawModeGuard = RawModeGuard<std::io::Stdin>;

#[cfg(not(unix))]
type ClientRawModeGuard = RawModeGuard;

pub fn run_client(args: &ClientArgs) -> Result<ExitCode, ClientRunError> {
    if args.action == ClientAction::Version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }

    if matches!(args.action, ClientAction::List | ClientAction::Detach) {
        return match run_client_attempt(args, &mut None, 0)? {
            ClientAttemptResult::Control(code) => Ok(code),
            ClientAttemptResult::DetachedForConnect { .. } => {
                unreachable!("control action prepared detach-connect")
            }
            ClientAttemptResult::Attached(_) => unreachable!("control action attached"),
        };
    }

    let mut initial_args = args.clone();
    if args.action == ClientAction::DetachConnect {
        let ClientAttemptResult::DetachedForConnect {
            session_id,
            exit_code,
        } = run_client_attempt(args, &mut None, 0)?
        else {
            unreachable!("detach-connect action returned an unexpected result");
        };
        if exit_code != ExitCode::SUCCESS {
            return Ok(exit_code);
        }
        initial_args.action = ClientAction::Attach;
        initial_args.session = Some(session_id);
    }

    let mut attached_io = None;
    let mut next_args = initial_args.clone();
    let mut known_session_id = initial_args.session.clone();
    let mut reconnect_attempts = 0;
    let mut recovering_known_session = args.action == ClientAction::DetachConnect;
    let mut attempt_id = 0u64;

    loop {
        attempt_id = attempt_id.saturating_add(1);
        let result = match run_client_attempt(&next_args, &mut attached_io, attempt_id) {
            Ok(result) => result,
            Err(error) if recovering_known_session && is_retryable_reconnect_error(&error) => {
                let session_id = known_session_id
                    .as_deref()
                    .expect("reconnect attempts require a known session");
                pause_attached_io(&mut attached_io);
                let delay = next_reconnect_delay(&mut reconnect_attempts);
                eprintln!(
                    "ssh-obi: reconnect attempt {} for session {session_id} failed: {error}; retrying in {}",
                    reconnect_attempts.saturating_sub(1),
                    format_duration(delay)
                );
                sleep_for_reconnect(delay);
                continue;
            }
            Err(error) => return Err(error),
        };
        let ClientAttemptResult::Attached(result) = result else {
            unreachable!("attach action returned a control result");
        };

        match result {
            Ok(success) => return Ok(success.exit_code),
            Err(failure)
                if reconnect_busy_session(
                    &failure.error,
                    known_session_id.as_deref(),
                    recovering_known_session,
                )
                .is_some() =>
            {
                let session_id =
                    reconnect_busy_session(&failure.error, known_session_id.as_deref(), true)
                        .expect("guard matched reconnect busy session");
                pause_attached_io(&mut attached_io);
                let delay = next_reconnect_delay(&mut reconnect_attempts);
                eprintln!(
                    "ssh-obi: session {session_id} still has an attached client; requesting detach before reconnect attempt {reconnect_attempts}"
                );
                if let Err(source) = detach_stale_client_for_reconnect(args, &session_id) {
                    if is_retryable_reconnect_error(&source) {
                        eprintln!(
                            "ssh-obi: detach request for session {session_id} failed: {source}; continuing reconnect attempts"
                        );
                    } else {
                        return Err(ClientRunError::ReconnectFailed {
                            session_id: session_id.clone(),
                            source: Box::new(source),
                        });
                    }
                }
                eprintln!(
                    "ssh-obi: retrying session {session_id} in {}",
                    format_duration(delay)
                );
                sleep_for_reconnect(delay);

                next_args = args.clone();
                next_args.action = ClientAction::Attach;
                next_args.session = Some(session_id);
            }
            Err(failure) if is_ambiguous_disconnect(&failure.error) => {
                let connection_was_established = failure.confirmed_session_id.is_some();
                let Some(session_id) = failure
                    .confirmed_session_id
                    .or_else(|| known_session_id.clone())
                else {
                    return Err(ClientRunError::ConnectionLostBeforeSessionKnown(Box::new(
                        failure.error,
                    )));
                };

                if connection_was_established {
                    reconnect_attempts = 0;
                }
                recovering_known_session = true;
                known_session_id = Some(session_id.clone());
                pause_attached_io(&mut attached_io);
                let delay = next_reconnect_delay(&mut reconnect_attempts);
                eprintln!(
                    "ssh-obi: connection lost; reconnecting session {session_id} in {} (attempt {reconnect_attempts})",
                    format_duration(delay)
                );
                sleep_for_reconnect(delay);

                next_args = args.clone();
                next_args.action = ClientAction::Attach;
                next_args.session = Some(session_id);
            }
            Err(failure) => return Err(failure.error),
        }
    }
}

fn next_reconnect_delay(attempts: &mut u32) -> Duration {
    *attempts = attempts.saturating_add(1);
    reconnect_delay_for_attempt(*attempts)
}

fn reconnect_delay_for_attempt(attempt: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    RECONNECT_INITIAL_DELAY
        .saturating_mul(multiplier)
        .min(RECONNECT_MAX_DELAY)
}

fn format_duration(duration: Duration) -> String {
    format!("{}s", duration.as_secs())
}

fn sleep_for_reconnect(delay: Duration) {
    let deadline = ReconnectDeadline::new(delay);
    loop {
        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(RECONNECT_SLEEP_QUANTUM));
    }
}

#[derive(Debug)]
struct ReconnectDeadline {
    suspend_aware: Option<Duration>,
    monotonic: Instant,
}

impl ReconnectDeadline {
    fn new(delay: Duration) -> Self {
        Self::with_now(delay, suspend_aware_machine_time(), Instant::now())
    }

    fn with_now(delay: Duration, suspend_aware: Option<Duration>, monotonic: Instant) -> Self {
        Self {
            suspend_aware: suspend_aware.map(|now| now.saturating_add(delay)),
            monotonic: monotonic + delay,
        }
    }

    fn remaining(&self) -> Duration {
        self.remaining_at(suspend_aware_machine_time(), Instant::now())
    }

    fn remaining_at(&self, suspend_aware: Option<Duration>, monotonic: Instant) -> Duration {
        if let (Some(deadline), Some(now)) = (self.suspend_aware, suspend_aware) {
            return deadline.saturating_sub(now);
        }

        self.monotonic.saturating_duration_since(monotonic)
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "openbsd"
))]
fn suspend_aware_machine_time() -> Option<Duration> {
    let mut time = std::mem::MaybeUninit::<nix::libc::timespec>::uninit();
    if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_BOOTTIME, time.as_mut_ptr()) } != 0 {
        return None;
    }

    let time = unsafe { time.assume_init() };
    let seconds = u64::try_from(time.tv_sec).ok()?;
    let nanos = u32::try_from(time.tv_nsec).ok()?;
    Some(Duration::new(seconds, nanos))
}

#[cfg(target_vendor = "apple")]
fn suspend_aware_machine_time() -> Option<Duration> {
    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }

    let mut info = std::mem::MaybeUninit::<MachTimebaseInfo>::uninit();
    if unsafe { mach_timebase_info(info.as_mut_ptr()) } != 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    if info.denom == 0 {
        return None;
    }

    let ticks = unsafe { mach_continuous_time() } as u128;
    let nanos = ticks
        .saturating_mul(u128::from(info.numer))
        .checked_div(u128::from(info.denom))?;
    Some(Duration::from_nanos(
        u64::try_from(nanos).unwrap_or(u64::MAX),
    ))
}

#[cfg(windows)]
fn suspend_aware_machine_time() -> Option<Duration> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;

    Some(Duration::from_millis(unsafe { GetTickCount64() }))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "openbsd",
    target_vendor = "apple",
    windows
)))]
fn suspend_aware_machine_time() -> Option<Duration> {
    None
}

fn detach_stale_client_for_reconnect(
    args: &ClientArgs,
    session_id: &str,
) -> Result<(), ClientRunError> {
    let detach_args = reconnect_detach_args(args, session_id);
    let mut attached_io = None;

    match run_client_attempt(&detach_args, &mut attached_io, 0)? {
        ClientAttemptResult::Control(code) if code == ExitCode::SUCCESS => Ok(()),
        ClientAttemptResult::Control(_) => Err(ClientRunError::ReconnectDetachFailed),
        ClientAttemptResult::DetachedForConnect { .. } => {
            unreachable!("detach action prepared detach-connect")
        }
        ClientAttemptResult::Attached(_) => unreachable!("detach action attached"),
    }
}

fn reconnect_detach_args(args: &ClientArgs, session_id: &str) -> ClientArgs {
    let mut detach_args = args.clone();
    detach_args.action = ClientAction::Detach;
    detach_args.session = Some(session_id.to_string());
    detach_args
}

fn reconnect_busy_session(
    error: &ClientRunError,
    known_session_id: Option<&str>,
    reconnecting: bool,
) -> Option<String> {
    let ClientRunError::SessionBusy(session_id) = error else {
        return None;
    };

    if reconnecting && known_session_id == Some(session_id.as_str()) {
        Some(session_id.clone())
    } else {
        None
    }
}

fn run_client_attempt(
    args: &ClientArgs,
    attached_io: &mut Option<AttachedIo>,
    attempt_id: u64,
) -> Result<ClientAttemptResult, ClientRunError> {
    let server_args = server_args_for_action(args);
    let term = terminal_type_from_env();
    let remote_command = remote_shell_command(&server_args, &term);
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
    let capabilities = match exchange_capabilities(&mut child_stdout, &mut child_stdin) {
        Ok(capabilities) => capabilities,
        Err(err) => {
            let _ = child.wait();
            return Err(err);
        }
    };
    let initial_window_size = if matches!(args.action, ClientAction::Attach | ClientAction::New)
        && capabilities
            .iter()
            .any(|capability| capability == CAP_INITIAL_WINDOW_SIZE_V1)
    {
        current_window_size().map_err(ClientRunError::WindowSize)?
    } else {
        None
    };

    let initial_action = match handle_initial_action(
        args,
        &mut child_stdout,
        &mut child_stdin,
        initial_window_size.as_ref(),
    ) {
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
    if let InitialActionResult::DetachedForConnect { session_id } = initial_action {
        let status = child.wait().map_err(ClientRunError::WaitSsh)?;
        return Ok(ClientAttemptResult::DetachedForConnect {
            session_id,
            exit_code: exit_code_with_ssh_status(ExitCode::SUCCESS, status),
        });
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
        capabilities
            .iter()
            .any(|capability| capability == CAP_KEEPALIVE_V1),
    );
    if matches!(
        &attached_result,
        Err(AttachedSessionFailure {
            error: ClientRunError::KeepaliveTimeout,
            ..
        })
    ) {
        let _ = child.kill();
    }
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
    DetachedForConnect {
        session_id: String,
        exit_code: ExitCode,
    },
    Attached(Result<AttachedSessionSuccess, AttachedSessionFailure>),
}

#[derive(Debug)]
struct AttachedSessionSuccess {
    exit_code: ExitCode,
}

#[derive(Debug)]
struct AttachedSessionFailure {
    confirmed_session_id: Option<String>,
    error: ClientRunError,
}

impl AttachedSessionFailure {
    fn new(confirmed_session_id: Option<String>, error: ClientRunError) -> Self {
        Self {
            confirmed_session_id,
            error,
        }
    }
}

#[derive(Debug)]
enum InitialActionResult {
    Control(ExitCode),
    DetachedForConnect { session_id: String },
    Attached { session_id: Option<String> },
}

fn handle_initial_action<R, W>(
    args: &ClientArgs,
    reader: &mut R,
    writer: &mut W,
    initial_window_size: Option<&WindowSize>,
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
            let frame = read_expected_broker_frame(&mut framed_reader, MessageType::SESSION_LIST)?;

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
        ClientAction::DetachConnect => {
            let session_id = match &args.session {
                Some(session_id) => session_id.clone(),
                None => select_detach_connect_session(reader, writer)?,
            };
            let mut framed_writer = FramedWriter::new(writer);
            framed_writer.write_body(
                MessageType::DETACH,
                0,
                &DetachSessionRequest {
                    session_id: session_id.clone(),
                },
            )?;
            framed_writer.flush()?;
            Ok(InitialActionResult::DetachedForConnect { session_id })
        }
        ClientAction::Attach => {
            let selection = if let Some(session_id) = &args.session {
                InitialClientSelection::Attach(session_id.clone())
            } else {
                select_session(reader, writer, initial_window_size)?
            };

            let InitialClientSelection::Attach(session_id) = selection else {
                return Ok(InitialActionResult::Attached { session_id: None });
            };
            let mut framed_writer = FramedWriter::new(writer);
            write_initial_window_size(&mut framed_writer, initial_window_size)?;
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
            write_initial_window_size(&mut framed_writer, initial_window_size)?;
            framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
            framed_writer.flush()?;
            Ok(InitialActionResult::Attached { session_id: None })
        }
        ClientAction::Version => unreachable!("version action does not start ssh"),
    }
}

fn select_detach_connect_session<R, W>(
    reader: &mut R,
    writer: &mut W,
) -> Result<String, ClientRunError>
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
    let frame = read_expected_broker_frame(&mut framed_reader, MessageType::SESSION_LIST)?;
    let list: SessionList = frame.decode_body()?;
    let sessions = list
        .sessions
        .into_iter()
        .map(SessionInfo::from_record)
        .collect::<Result<Vec<_>, _>>()?;

    match auto_select_existing(&sessions) {
        ExistingSelection::None => Err(ClientRunError::NoSessionsForDetachConnect),
        ExistingSelection::Attach(session_id) => Ok(session_id.to_string()),
        ExistingSelection::Prompt => prompt_detach_connect_selection(&sessions),
    }
}

fn prompt_detach_connect_selection(sessions: &[SessionInfo]) -> Result<String, ClientRunError> {
    eprintln!("Select a session to detach and connect:\n");
    eprint!("{}", render_detach_connect_table(sessions));
    eprint!("> ");
    io::stderr().flush().map_err(ClientRunError::Prompt)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(ClientRunError::Prompt)?;
    let answer = answer.trim();
    let selector = answer
        .parse::<usize>()
        .map_err(|_| ClientRunError::InvalidSessionSelection(answer.to_string()))?;
    let session = selector
        .checked_sub(1)
        .and_then(|index| sessions.get(index))
        .ok_or_else(|| ClientRunError::InvalidSessionSelection(answer.to_string()))?;
    Ok(session.id.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InitialClientSelection {
    Attach(String),
    NewAlreadySent,
}

fn select_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    initial_window_size: Option<&WindowSize>,
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
    let frame = read_expected_broker_frame(&mut framed_reader, MessageType::SESSION_LIST)?;

    let list: SessionList = frame.decode_body()?;
    let sessions = list
        .sessions
        .into_iter()
        .map(SessionInfo::from_record)
        .collect::<Result<Vec<_>, _>>()?;

    match auto_select(&sessions) {
        AutoSelection::NewSession => {
            let mut framed_writer = FramedWriter::new(writer);
            write_initial_window_size(&mut framed_writer, initial_window_size)?;
            framed_writer.write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)?;
            framed_writer.flush()?;
            Ok(InitialClientSelection::NewAlreadySent)
        }
        AutoSelection::Attach(session_id) => {
            Ok(InitialClientSelection::Attach(session_id.to_string()))
        }
        AutoSelection::Prompt => prompt_session_selection(&sessions, writer, initial_window_size),
    }
}

fn prompt_session_selection<W>(
    sessions: &[SessionInfo],
    writer: &mut W,
    initial_window_size: Option<&WindowSize>,
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
        write_initial_window_size(&mut framed_writer, initial_window_size)?;
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

fn write_initial_window_size<W>(
    writer: &mut FramedWriter<W>,
    initial_window_size: Option<&WindowSize>,
) -> Result<(), ClientRunError>
where
    W: Write,
{
    if let Some(window_size) = initial_window_size {
        writer.write_body(MessageType::INITIAL_WINDOW_SIZE, 0, window_size)?;
    }
    Ok(())
}

#[derive(Debug)]
struct AttachedIo {
    raw_mode: Option<ClientRawModeGuard>,
    input_paused: bool,
    receiver: Receiver<AttachedEvent>,
    sender: Sender<AttachedEvent>,
    stdin_control: Arc<StdinControl>,
    _stdin_thread: thread::JoinHandle<()>,
}

#[derive(Debug, Default)]
struct StdinControl {
    state: Mutex<StdinControlState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct StdinControlState {
    pause_requested: bool,
    pause_acknowledged: bool,
    reader_finished: bool,
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
        let stdin_control = Arc::new(StdinControl::default());
        let thread_control = Arc::clone(&stdin_control);
        let stdin_thread = thread::spawn(move || read_stdin_events(stdin_sender, thread_control));

        *attached_io = Some(AttachedIo {
            raw_mode,
            input_paused: false,
            receiver,
            sender,
            stdin_control,
            _stdin_thread: stdin_thread,
        });
    } else {
        attached_io
            .as_mut()
            .expect("attached io initialized")
            .resume()?;
    }

    Ok(attached_io.as_ref().expect("attached io initialized"))
}

fn pause_attached_io(attached_io: &mut Option<AttachedIo>) {
    if let Some(attached_io) = attached_io {
        attached_io.pause();
    }
}

impl AttachedIo {
    fn pause(&mut self) {
        if self.input_paused {
            return;
        }

        let mut state = self
            .stdin_control
            .state
            .lock()
            .expect("stdin control poisoned");
        state.pause_requested = true;
        self.stdin_control.changed.notify_all();
        while !state.pause_acknowledged && !state.reader_finished {
            state = self
                .stdin_control
                .changed
                .wait(state)
                .expect("stdin control poisoned");
        }
        drop(state);

        // The reader has stopped touching stdin, so OpenSSH can exclusively use /dev/tty (or
        // the Windows console) while the terminal is back in its normal line-oriented mode.
        self.raw_mode.take();
        self.discard_pending_events();
        self.input_paused = true;
    }

    fn resume(&mut self) -> Result<(), ClientRunError> {
        if !self.input_paused {
            return Ok(());
        }

        self.discard_pending_events();
        self.raw_mode = enable_stdin_raw_mode()?;

        let mut state = self
            .stdin_control
            .state
            .lock()
            .expect("stdin control poisoned");
        state.pause_requested = false;
        self.stdin_control.changed.notify_all();
        while state.pause_acknowledged && !state.reader_finished {
            state = self
                .stdin_control
                .changed
                .wait(state)
                .expect("stdin control poisoned");
        }
        self.input_paused = false;
        Ok(())
    }

    fn discard_pending_events(&self) {
        while self.receiver.try_recv().is_ok() {}
    }
}

fn read_stdin_events(sender: Sender<AttachedEvent>, control: Arc<StdinControl>) {
    let _finished = StdinReaderFinished(Arc::clone(&control));
    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 8192];

    loop {
        wait_while_stdin_paused(&control);
        match wait_for_stdin_ready(&stdin, STDIN_POLL_INTERVAL) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(err) => {
                let _ = sender.send(AttachedEvent::StdinError(err));
                return;
            }
        }

        match stdin.read(&mut buf) {
            Ok(0) => {
                let _ = sender.send(AttachedEvent::StdinEof);
                return;
            }
            Ok(n) => {
                let mut bytes = buf[..n].to_vec();
                if let Err(err) = append_pending_stdin_bytes(&mut stdin, &mut bytes) {
                    let _ = sender.send(AttachedEvent::StdinError(err));
                    return;
                }
                if sender.send(AttachedEvent::Stdin(bytes)).is_err() {
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

struct StdinReaderFinished(Arc<StdinControl>);

impl Drop for StdinReaderFinished {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("stdin control poisoned");
        state.reader_finished = true;
        self.0.changed.notify_all();
    }
}

fn wait_while_stdin_paused(control: &StdinControl) {
    let mut state = control.state.lock().expect("stdin control poisoned");
    if !state.pause_requested {
        return;
    }

    state.pause_acknowledged = true;
    control.changed.notify_all();
    while state.pause_requested {
        state = control.changed.wait(state).expect("stdin control poisoned");
    }
    state.pause_acknowledged = false;
    control.changed.notify_all();
}

#[cfg(unix)]
fn wait_for_stdin_ready<R>(stdin: &R, timeout: Duration) -> io::Result<bool>
where
    R: std::os::fd::AsRawFd,
{
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut pollfd = nix::libc::pollfd {
        fd: stdin.as_raw_fd(),
        events: nix::libc::POLLIN,
        revents: 0,
    };

    loop {
        let ready = unsafe { nix::libc::poll(&mut pollfd, 1, timeout_ms) };
        if ready >= 0 {
            return Ok(ready > 0);
        }

        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(windows)]
fn wait_for_stdin_ready<R>(_stdin: &R, timeout: Duration) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{
        INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Ok(true);
    }

    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    match unsafe { WaitForSingleObject(handle, timeout_ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Ok(false),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn wait_for_stdin_ready<R>(_stdin: &R, _timeout: Duration) -> io::Result<bool> {
    Ok(true)
}

#[cfg(unix)]
fn append_pending_stdin_bytes<R>(stdin: &mut R, bytes: &mut Vec<u8>) -> io::Result<()>
where
    R: Read + std::os::fd::AsRawFd,
{
    const COALESCE_TIMEOUT_MS: i32 = 2;
    const MAX_BATCH: usize = 64 * 1024;

    let fd = stdin.as_raw_fd();
    let mut buf = [0u8; 8192];

    while bytes.len() < MAX_BATCH {
        let mut pollfd = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { nix::libc::poll(&mut pollfd, 1, COALESCE_TIMEOUT_MS) };
        if ready == 0 {
            break;
        }
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if pollfd.revents & nix::libc::POLLIN == 0 {
            break;
        }

        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

#[cfg(not(unix))]
fn append_pending_stdin_bytes<R>(_stdin: &mut R, _bytes: &mut Vec<u8>) -> io::Result<()>
where
    R: Read,
{
    Ok(())
}

fn run_attached_session<R, W>(
    reader: R,
    writer: W,
    initial_session_id: Option<String>,
    attached_io: &AttachedIo,
    attempt_id: u64,
    keepalive_enabled: bool,
) -> Result<AttachedSessionSuccess, AttachedSessionFailure>
where
    R: Read + Send + 'static,
    W: Write,
{
    spawn_broker_reader(reader, attached_io.sender.clone(), attempt_id);
    let expected_session_id = initial_session_id;
    let mut confirmed_session_id = None;
    let mut writer = FramedWriter::new(writer);
    let mut stdout = io::stdout().lock();
    let mut last_window_size = None;
    let mut stdin_open = true;
    let mut next_keepalive = Instant::now() + KEEPALIVE_INTERVAL;
    let mut pending_keepalive = None;
    let mut keepalive_nonce = 0u64;

    send_window_size_if_changed(&mut writer, &mut last_window_size)
        .map_err(|err| AttachedSessionFailure::new(confirmed_session_id.clone(), err))?;

    loop {
        let now = Instant::now();
        if pending_keepalive.is_some_and(|(_, deadline)| now >= deadline) {
            return Err(AttachedSessionFailure::new(
                confirmed_session_id,
                ClientRunError::KeepaliveTimeout,
            ));
        }
        if keepalive_enabled && pending_keepalive.is_none() && now >= next_keepalive {
            keepalive_nonce = keepalive_nonce.wrapping_add(1);
            writer
                .write_body(
                    MessageType::PING,
                    0,
                    &Keepalive {
                        nonce: keepalive_nonce,
                    },
                )
                .map_err(|err| {
                    AttachedSessionFailure::new(
                        confirmed_session_id.clone(),
                        ClientRunError::from(err),
                    )
                })?;
            writer.flush().map_err(|err| {
                AttachedSessionFailure::new(confirmed_session_id.clone(), ClientRunError::from(err))
            })?;
            pending_keepalive = Some((keepalive_nonce, now + KEEPALIVE_TIMEOUT));
        }

        if resize_pending() {
            send_window_size_if_changed(&mut writer, &mut last_window_size)
                .map_err(|err| AttachedSessionFailure::new(confirmed_session_id.clone(), err))?;
        }

        let event = match attached_io.receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(AttachedSessionFailure::new(
                    confirmed_session_id,
                    ClientRunError::UnexpectedBrokerEof,
                ));
            }
        };

        match event {
            AttachedEvent::Stdin(bytes) if stdin_open => {
                writer
                    .write_body(MessageType::PTY_DATA, 0, &PtyData { bytes })
                    .map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                writer.flush().map_err(|err| {
                    AttachedSessionFailure::new(
                        confirmed_session_id.clone(),
                        ClientRunError::from(err),
                    )
                })?;
            }
            AttachedEvent::Stdin(_) => {}
            AttachedEvent::StdinEof => {
                stdin_open = false;
            }
            AttachedEvent::StdinError(err) => {
                return Err(AttachedSessionFailure::new(
                    confirmed_session_id,
                    ClientRunError::CopyStdin(err),
                ));
            }
            AttachedEvent::BrokerFrame {
                attempt_id: event_attempt_id,
                result,
            } if event_attempt_id == attempt_id => {
                let Some(frame) = result.map_err(|err| {
                    AttachedSessionFailure::new(
                        confirmed_session_id.clone(),
                        ClientRunError::from(err),
                    )
                })?
                else {
                    return Err(AttachedSessionFailure::new(
                        confirmed_session_id,
                        ClientRunError::UnexpectedBrokerEof,
                    ));
                };

                if frame.msg_type() == MessageType::ATTACHED_SESSION {
                    let attached: AttachedSession = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    if let Some(expected) = expected_session_id.as_ref().cloned()
                        && expected != attached.session_id
                    {
                        return Err(AttachedSessionFailure::new(
                            confirmed_session_id,
                            ClientRunError::SessionMismatch {
                                expected,
                                actual: attached.session_id,
                            },
                        ));
                    }
                    confirmed_session_id = Some(attached.session_id);
                    continue;
                }

                if frame.msg_type() == MessageType::PTY_DATA {
                    let data: PtyData = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    stdout.write_all(&data.bytes).map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::CopyStdout(err),
                        )
                    })?;
                    stdout.flush().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::CopyStdout(err),
                        )
                    })?;
                    continue;
                }

                if frame.msg_type() == MessageType::PONG {
                    let pong: Keepalive = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    if pending_keepalive.is_some_and(|(nonce, _)| nonce == pong.nonce) {
                        pending_keepalive = None;
                        next_keepalive = Instant::now() + KEEPALIVE_INTERVAL;
                    }
                    continue;
                }

                if frame.msg_type() == MessageType::CLIENT_SHOULD_EXIT {
                    return Ok(AttachedSessionSuccess {
                        exit_code: ExitCode::SUCCESS,
                    });
                }

                if frame.msg_type() == MessageType::EXIT_STATUS {
                    let status: ExitStatus = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    let exit_code = exit_code_from_status(status).map_err(|err| {
                        AttachedSessionFailure::new(confirmed_session_id.clone(), err)
                    })?;
                    return Ok(AttachedSessionSuccess { exit_code });
                }

                if frame.msg_type() == MessageType::SESSION_BUSY {
                    let busy: SessionBusy = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    return Err(AttachedSessionFailure::new(
                        confirmed_session_id,
                        ClientRunError::SessionBusy(busy.session_id),
                    ));
                }

                if frame.msg_type() == MessageType::ERROR {
                    let error: ErrorMessage = frame.decode_body().map_err(|err| {
                        AttachedSessionFailure::new(
                            confirmed_session_id.clone(),
                            ClientRunError::from(err),
                        )
                    })?;
                    return Err(AttachedSessionFailure::new(
                        confirmed_session_id,
                        ClientRunError::BrokerError(error.message),
                    ));
                }

                if frame.msg_type().name().is_none() {
                    continue;
                }

                return Err(AttachedSessionFailure::new(
                    confirmed_session_id,
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

fn read_expected_broker_frame<R>(
    reader: &mut FramedReader<R>,
    expected: MessageType,
) -> Result<Frame, ClientRunError>
where
    R: Read,
{
    loop {
        let frame = reader
            .read_frame()?
            .ok_or(ClientRunError::UnexpectedBrokerEof)?;

        if frame.msg_type() == expected {
            return Ok(frame);
        }

        if frame.msg_type().name().is_none() {
            continue;
        }

        return Err(ClientRunError::UnexpectedBrokerMessage(
            frame.msg_type().get(),
        ));
    }
}

fn exchange_capabilities<R, W>(
    reader: &mut R,
    writer: &mut W,
) -> Result<Vec<String>, ClientRunError>
where
    R: Read,
    W: Write,
{
    {
        let mut writer = FramedWriter::new(&mut *writer);
        writer.write_body(
            MessageType::CAPABILITIES,
            0,
            &Capabilities::default_supported(),
        )?;
        writer.flush()?;
    }

    let mut reader = FramedReader::new(reader);
    let frame = read_expected_broker_frame(&mut reader, MessageType::CAPABILITIES)?;
    let peer: Capabilities = frame.decode_body()?;
    let intersection = Capabilities::default_supported().intersection(&peer);
    if intersection.is_empty() {
        return Err(ClientRunError::NoSharedCapabilities);
    }

    Ok(intersection)
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

#[cfg(windows)]
fn current_window_size() -> io::Result<Option<WindowSize>> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Ok(None);
    }

    let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::uninit();
    if unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) } == 0 {
        return Ok(None);
    }

    let info = unsafe { info.assume_init() };
    let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
    let cols = info.srWindow.Right - info.srWindow.Left + 1;
    if rows <= 0 || cols <= 0 {
        return Ok(None);
    }

    Ok(Some(WindowSize {
        rows: rows as u16,
        cols: cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    }))
}

#[cfg(all(not(unix), not(windows)))]
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

#[cfg(windows)]
fn resize_pending() -> bool {
    true
}

#[cfg(all(not(unix), not(windows)))]
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
            | ClientRunError::KeepaliveTimeout
            | ClientRunError::Protocol(ProtocolError::Io(_))
            | ClientRunError::Protocol(ProtocolError::TruncatedHeader)
            | ClientRunError::Protocol(ProtocolError::TruncatedPayload { .. })
    )
}

fn is_retryable_reconnect_error(error: &ClientRunError) -> bool {
    is_ambiguous_disconnect(error)
        || matches!(
            error,
            ClientRunError::SpawnSsh(_)
                | ClientRunError::MissingChildStdin
                | ClientRunError::MissingChildStdout
                | ClientRunError::UnexpectedBootstrapEof
                | ClientRunError::ReadBootstrap(_)
                | ClientRunError::WriteBootstrap(_)
                | ClientRunError::WaitSsh(_)
                | ClientRunError::ReconnectDetachFailed
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
    KeepaliveTimeout,
    UnexpectedBrokerMessage(u8),
    BrokerError(String),
    SessionBusy(String),
    SessionMismatch {
        expected: String,
        actual: String,
    },
    NoSharedCapabilities,
    ConnectionLostBeforeSessionKnown(Box<ClientRunError>),
    ReconnectFailed {
        session_id: String,
        source: Box<ClientRunError>,
    },
    ReconnectDetachFailed,
    Protocol(ProtocolError),
    InvalidSession(SessionIdError),
    InvalidSessionSelection(String),
    NoSessionsForDetachConnect,
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
            Self::KeepaliveTimeout => write!(f, "broker keepalive timed out"),
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
            Self::NoSharedCapabilities => write!(f, "broker has no shared protocol capabilities"),
            Self::ConnectionLostBeforeSessionKnown(source) => {
                write!(
                    f,
                    "connection lost before the broker identified the session: {source}"
                )
            }
            Self::ReconnectFailed { session_id, source } => {
                write!(f, "failed to reconnect session {session_id}: {source}")
            }
            Self::ReconnectDetachFailed => write!(
                f,
                "failed to ask the stale attached client to detach during reconnect"
            ),
            Self::Protocol(err) => write!(f, "protocol error: {err}"),
            Self::InvalidSession(err) => write!(f, "invalid session from broker: {err}"),
            Self::InvalidSessionSelection(selection) => {
                write!(f, "invalid session selection: {selection}")
            }
            Self::NoSessionsForDetachConnect => {
                write!(f, "there are no existing sessions to detach and connect")
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
    use crate::protocol::{SessionRecord, UnixTimeMillis};
    use std::ffi::OsString;
    use std::io::Cursor;

    fn args(action: ClientAction) -> ClientArgs {
        ClientArgs {
            action,
            session: None,
            ssh_args: Vec::new(),
            destination: OsString::from("host"),
        }
    }

    fn attached_io_for_test() -> AttachedIo {
        let (sender, receiver) = mpsc::channel();
        AttachedIo {
            raw_mode: None,
            input_paused: false,
            receiver,
            sender,
            stdin_control: Arc::new(StdinControl::default()),
            _stdin_thread: thread::spawn(|| {}),
        }
    }

    struct BlockingReader {
        prefix: Cursor<Vec<u8>>,
        release: Receiver<()>,
    }

    impl Read for BlockingReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.prefix.position() < self.prefix.get_ref().len() as u64 {
                return self.prefix.read(bytes);
            }
            let _ = self.release.recv();
            Ok(0)
        }
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().expect("shared writer poisoned").clone()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("shared writer poisoned").write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_window_size() -> WindowSize {
        WindowSize {
            rows: 33,
            cols: 101,
            pixel_width: 1001,
            pixel_height: 501,
        }
    }

    fn session_record(session_id: &str, attached: bool) -> SessionRecord {
        SessionRecord {
            session_id: session_id.to_string(),
            init_time: UnixTimeMillis(1_000),
            last_detach_time: None,
            current_command: "sh".to_string(),
            attached,
        }
    }

    #[test]
    fn detach_action_uses_broker_default() {
        assert!(server_args_for_action(&args(ClientAction::Detach)).is_empty());
    }

    #[test]
    fn detach_connect_with_session_override_writes_detach_request() {
        let mut args = args(ClientAction::DetachConnect);
        args.session = Some("aaaaaaaa".to_string());
        let mut output = Vec::new();

        let action = handle_initial_action(&args, &mut &[][..], &mut output, None).unwrap();
        assert!(matches!(
            action,
            InitialActionResult::DetachedForConnect { session_id }
                if session_id == "aaaaaaaa"
        ));

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::DETACH);
        let request: DetachSessionRequest = frame.decode_body().unwrap();
        assert_eq!(request.session_id, "aaaaaaaa");
        assert!(reader.read_frame().unwrap().is_none());
    }

    #[test]
    fn detach_connect_auto_selects_the_only_session() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::SESSION_LIST,
                    0,
                    &SessionList {
                        sessions: vec![session_record("aaaaaaaa", true)],
                    },
                )
                .unwrap();
        }
        let mut output = Vec::new();

        let action = handle_initial_action(
            &args(ClientAction::DetachConnect),
            &mut input.as_slice(),
            &mut output,
            None,
        )
        .unwrap();
        assert!(matches!(
            action,
            InitialActionResult::DetachedForConnect { session_id }
                if session_id == "aaaaaaaa"
        ));

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::SESSION_LIST_REQUEST);
        let request: SessionListRequest = frame.decode_body().unwrap();
        assert!(request.continue_after_response);
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::DETACH);
        let request: DetachSessionRequest = frame.decode_body().unwrap();
        assert_eq!(request.session_id, "aaaaaaaa");
    }

    #[test]
    fn detach_connect_rejects_an_empty_session_list() {
        let mut input = Vec::new();
        FramedWriter::new(&mut input)
            .write_body(
                MessageType::SESSION_LIST,
                0,
                &SessionList {
                    sessions: Vec::new(),
                },
            )
            .unwrap();

        let error = handle_initial_action(
            &args(ClientAction::DetachConnect),
            &mut input.as_slice(),
            &mut Vec::new(),
            None,
        )
        .unwrap_err();
        assert!(matches!(error, ClientRunError::NoSessionsForDetachConnect));
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
            None,
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

        let action =
            handle_initial_action(&args, &mut input.as_slice(), &mut output, None).unwrap();
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
    fn attach_action_writes_initial_window_size_before_attach_request() {
        let size = test_window_size();
        let mut args = args(ClientAction::Attach);
        args.session = Some("aaaaaaaa".to_string());
        let input = Vec::new();
        let mut output = Vec::new();

        handle_initial_action(&args, &mut input.as_slice(), &mut output, Some(&size)).unwrap();

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::INITIAL_WINDOW_SIZE);
        let request_size: WindowSize = frame.decode_body().unwrap();
        assert_eq!(request_size, size);

        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::ATTACH_SESSION);
        let request: AttachSessionRequest = frame.decode_body().unwrap();
        assert_eq!(request.session_id, "aaaaaaaa");
    }

    #[test]
    fn reconnect_detach_args_preserve_destination_and_target_session() {
        let mut args = args(ClientAction::Attach);
        args.session = Some("oldoldold".to_string());
        args.ssh_args = vec![OsString::from("-p"), OsString::from("2222")];

        let detach_args = reconnect_detach_args(&args, "aaaaaaaa");

        assert!(matches!(detach_args.action, ClientAction::Detach));
        assert_eq!(detach_args.session.as_deref(), Some("aaaaaaaa"));
        assert_eq!(detach_args.destination, OsString::from("host"));
        assert_eq!(
            detach_args.ssh_args,
            vec![OsString::from("-p"), OsString::from("2222")]
        );
    }

    #[test]
    fn reconnect_busy_session_only_matches_known_reconnect_target() {
        let busy = ClientRunError::SessionBusy("aaaaaaaa".to_string());

        assert_eq!(
            reconnect_busy_session(&busy, Some("aaaaaaaa"), true).as_deref(),
            Some("aaaaaaaa")
        );
        assert!(reconnect_busy_session(&busy, Some("aaaaaaaa"), false).is_none());
        assert!(reconnect_busy_session(&busy, Some("bbbbbbbb"), true).is_none());
        assert!(reconnect_busy_session(&busy, None, true).is_none());
        assert!(
            reconnect_busy_session(&ClientRunError::UnexpectedBrokerEof, Some("aaaaaaaa"), true)
                .is_none()
        );
    }

    #[test]
    fn reconnect_delay_uses_sparse_capped_exponential_backoff() {
        assert_eq!(reconnect_delay_for_attempt(1), Duration::from_secs(1));
        assert_eq!(reconnect_delay_for_attempt(2), Duration::from_secs(2));
        assert_eq!(reconnect_delay_for_attempt(3), Duration::from_secs(4));
        assert_eq!(reconnect_delay_for_attempt(4), Duration::from_secs(8));
        assert_eq!(reconnect_delay_for_attempt(5), Duration::from_secs(16));
        assert_eq!(reconnect_delay_for_attempt(6), Duration::from_secs(32));
        assert_eq!(reconnect_delay_for_attempt(7), Duration::from_secs(64));
        assert_eq!(reconnect_delay_for_attempt(8), Duration::from_secs(120));
        assert_eq!(reconnect_delay_for_attempt(10), Duration::from_secs(120));
    }

    #[test]
    fn next_reconnect_delay_does_not_expire() {
        let mut attempts = 0;

        for expected_attempt in 1..=100 {
            let delay = next_reconnect_delay(&mut attempts);
            assert_eq!(attempts, expected_attempt);
            assert!(delay <= RECONNECT_MAX_DELAY);
        }
    }

    #[test]
    fn reconnect_deadline_counts_suspend_aware_machine_time() {
        let monotonic = Instant::now();
        let deadline = ReconnectDeadline::with_now(
            Duration::from_secs(120),
            Some(Duration::from_secs(1_000)),
            monotonic,
        );

        assert_eq!(
            deadline.remaining_at(Some(Duration::from_secs(1_060)), monotonic),
            Duration::from_secs(60)
        );
        assert_eq!(
            deadline.remaining_at(Some(Duration::from_secs(1_120)), monotonic),
            Duration::ZERO
        );
    }

    #[test]
    fn reconnect_deadline_falls_back_to_monotonic_time() {
        let monotonic = Instant::now();
        let deadline = ReconnectDeadline::with_now(Duration::from_secs(5), None, monotonic);

        assert_eq!(
            deadline.remaining_at(None, monotonic + Duration::from_secs(2)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn ssh_transport_failures_remain_retryable_during_reconnect() {
        assert!(is_retryable_reconnect_error(
            &ClientRunError::UnexpectedBootstrapEof
        ));
        assert!(is_retryable_reconnect_error(
            &ClientRunError::ReadBootstrap(io::Error::from(io::ErrorKind::HostUnreachable))
        ));
        assert!(is_retryable_reconnect_error(
            &ClientRunError::ReadBootstrap(io::Error::from(io::ErrorKind::NotFound))
        ));
        assert!(is_retryable_reconnect_error(
            &ClientRunError::UnexpectedBrokerEof
        ));
        assert!(is_retryable_reconnect_error(
            &ClientRunError::ReconnectDetachFailed
        ));
        assert!(!is_retryable_reconnect_error(
            &ClientRunError::RemoteBootstrap("incompatible server".to_string())
        ));
    }

    #[test]
    fn stdin_pause_control_acknowledges_and_resumes() {
        let control = Arc::new(StdinControl::default());
        control.state.lock().unwrap().pause_requested = true;
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || wait_while_stdin_paused(&worker_control));

        let mut state = control.state.lock().unwrap();
        while !state.pause_acknowledged {
            state = control.changed.wait(state).unwrap();
        }
        state.pause_requested = false;
        control.changed.notify_all();
        while state.pause_acknowledged {
            state = control.changed.wait(state).unwrap();
        }
        drop(state);

        worker.join().unwrap();
    }

    #[test]
    fn new_action_writes_new_session_request() {
        let input = Vec::new();
        let mut output = Vec::new();

        let action = handle_initial_action(
            &args(ClientAction::New),
            &mut input.as_slice(),
            &mut output,
            None,
        )
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
    fn new_action_writes_initial_window_size_before_new_session_request() {
        let size = test_window_size();
        let input = Vec::new();
        let mut output = Vec::new();

        handle_initial_action(
            &args(ClientAction::New),
            &mut input.as_slice(),
            &mut output,
            Some(&size),
        )
        .unwrap();

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::INITIAL_WINDOW_SIZE);
        let request_size: WindowSize = frame.decode_body().unwrap();
        assert_eq!(request_size, size);

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
            None,
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

    #[test]
    fn list_action_skips_unknown_message_before_response() {
        let mut input = Vec::new();
        {
            let unknown = Frame::new(MessageType::new(250), 0, Vec::new()).unwrap();
            unknown.write_to(&mut input).unwrap();

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
            None,
        )
        .unwrap();
        assert!(matches!(
            action,
            InitialActionResult::Control(code) if code == ExitCode::SUCCESS
        ));
    }

    #[test]
    fn capability_exchange_writes_and_reads_capabilities() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::CAPABILITIES,
                    0,
                    &Capabilities::default_supported(),
                )
                .unwrap();
        }
        let mut output = Vec::new();

        exchange_capabilities(&mut input.as_slice(), &mut output).unwrap();

        let mut reader = FramedReader::new(output.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::CAPABILITIES);
        let _: Capabilities = frame.decode_body().unwrap();
    }

    #[test]
    fn attached_session_does_not_confirm_requested_id_before_attached_message() {
        let attached_io = attached_io_for_test();
        let result = run_attached_session(
            Cursor::new(Vec::<u8>::new()),
            Vec::<u8>::new(),
            Some("aaaaaaaa".to_string()),
            &attached_io,
            1,
            false,
        )
        .unwrap_err();

        assert!(result.confirmed_session_id.is_none());
        assert!(matches!(result.error, ClientRunError::UnexpectedBrokerEof));
    }

    #[test]
    fn attached_session_records_authoritative_id_after_attached_message() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::ATTACHED_SESSION,
                    0,
                    &AttachedSession {
                        session_id: "bbbbbbbb".to_string(),
                    },
                )
                .unwrap();
        }

        let attached_io = attached_io_for_test();
        let result = run_attached_session(
            Cursor::new(input),
            Vec::<u8>::new(),
            None,
            &attached_io,
            1,
            false,
        )
        .unwrap_err();

        assert_eq!(result.confirmed_session_id.as_deref(), Some("bbbbbbbb"));
        assert!(matches!(result.error, ClientRunError::UnexpectedBrokerEof));
    }

    #[test]
    fn attached_session_reports_session_busy() {
        let mut input = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut input);
            writer
                .write_body(
                    MessageType::SESSION_BUSY,
                    0,
                    &SessionBusy {
                        session_id: "aaaaaaaa".to_string(),
                    },
                )
                .unwrap();
        }

        let attached_io = attached_io_for_test();
        let result = run_attached_session(
            Cursor::new(input),
            Vec::<u8>::new(),
            None,
            &attached_io,
            1,
            false,
        )
        .unwrap_err();

        assert!(result.confirmed_session_id.is_none());
        assert!(matches!(
            result.error,
            ClientRunError::SessionBusy(session_id) if session_id == "aaaaaaaa"
        ));
    }

    #[test]
    fn attached_session_sends_negotiated_keepalive_and_times_out_without_pong() {
        let mut input = Vec::new();
        let mut input_writer = FramedWriter::new(&mut input);
        input_writer
            .write_body(
                MessageType::ATTACHED_SESSION,
                0,
                &AttachedSession {
                    session_id: "kaaaaaaa".to_string(),
                },
            )
            .unwrap();
        let (release_sender, release_receiver) = mpsc::channel();
        let reader = BlockingReader {
            prefix: Cursor::new(input),
            release: release_receiver,
        };
        let output = SharedWriter::default();

        let attached_io = attached_io_for_test();
        let result =
            run_attached_session(reader, output.clone(), None, &attached_io, 1, true).unwrap_err();

        assert_eq!(result.confirmed_session_id.as_deref(), Some("kaaaaaaa"));
        assert!(matches!(result.error, ClientRunError::KeepaliveTimeout));

        let bytes = output.bytes();
        let mut reader = FramedReader::new(bytes.as_slice());
        let frame = loop {
            let frame = reader.read_frame().unwrap().unwrap();
            if frame.msg_type() == MessageType::PING {
                break frame;
            }
        };
        assert_eq!(frame.msg_type(), MessageType::PING);
        assert_eq!(frame.decode_body::<Keepalive>().unwrap().nonce, 1);
        drop(release_sender);
    }
}
