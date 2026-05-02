use std::env;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::protocol::{
    AttachSessionRequest, BrokerAttachRequest, DaemonInfo, DaemonInfoRequest, DetachRequest,
    DetachSessionRequest, ErrorMessage, MessageType, NewSessionRequest, ProtocolError, SessionList,
    SessionListRequest,
};
use crate::session::{
    SessionIdError, SocketPathError, generate_session_id, list_session_socket_ids,
    remove_stale_socket, socket_dir_for_uid, socket_path, socket_path_for_uid,
};
use crate::transport::{FramedReader, FramedWriter};

pub const ENV_SESSION: &str = "SSH_OBI_SESSION";
pub const ENV_SOCKET: &str = "SSH_OBI_SOCKET";

pub fn detach_from_env() -> Result<(), ServerError> {
    let socket = env::var(ENV_SOCKET).map_err(|_| ServerError::MissingEnvironment(ENV_SOCKET))?;
    detach_via_socket(socket)
}

pub fn run_broker_stdio() -> Result<(), ServerError> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    handle_broker_request(stdin, stdout, current_uid())
}

pub fn handle_broker_request<R, W>(reader: R, writer: W, uid: u32) -> Result<(), ServerError>
where
    R: Read + Send,
    W: Write + Send,
{
    let mut reader = FramedReader::new(reader);
    let mut writer = FramedWriter::new(writer);
    let mut saw_request = false;

    loop {
        let Some(frame) = reader.read_frame()? else {
            return if saw_request {
                Ok(())
            } else {
                Err(ServerError::NoBrokerRequest)
            };
        };
        saw_request = true;

        if frame.msg_type() == MessageType::SESSION_LIST_REQUEST {
            let request: SessionListRequest = frame.decode_body()?;
            let socket_dir = socket_dir_for_uid(uid);
            let sessions = enumerate_daemons(socket_dir)?
                .into_iter()
                .map(|info| info.session)
                .collect();
            writer.write_body(MessageType::SESSION_LIST, 0, &SessionList { sessions })?;
            writer.flush()?;

            if request.continue_after_response {
                continue;
            }
            return Ok(());
        }

        if frame.msg_type() == MessageType::DETACH {
            let request: DetachSessionRequest = frame.decode_body()?;
            let path = socket_path_for_uid(uid, &request.session_id)?;
            detach_via_socket(path)?;
            return Ok(());
        }

        if frame.msg_type() == MessageType::ATTACH_SESSION {
            #[cfg(not(unix))]
            {
                return Err(ServerError::UnsupportedPlatform(
                    "broker attach requires Unix-domain sockets",
                ));
            }

            #[cfg(unix)]
            {
                let request: AttachSessionRequest = frame.decode_body()?;
                let path = socket_path_for_uid(uid, &request.session_id)?;
                let daemon = connect_daemon(path)?;
                let mut daemon_writer =
                    FramedWriter::new(daemon.try_clone().map_err(ServerError::Connect)?);
                daemon_writer.write_body(MessageType::BROKER_ATTACH, 0, &BrokerAttachRequest)?;
                daemon_writer.flush()?;
                return proxy_attached_session(reader, writer.into_inner(), daemon);
            }
        }

        if frame.msg_type() == MessageType::NEW_SESSION {
            #[cfg(not(unix))]
            {
                return Err(ServerError::UnsupportedPlatform(
                    "new session daemon launch requires Unix",
                ));
            }

            #[cfg(unix)]
            {
                let _: NewSessionRequest = frame.decode_body()?;
                let session_id = launch_daemon(uid)?;
                let path = socket_path_for_uid(uid, session_id.as_str())?;
                let daemon = connect_daemon(path)?;
                let mut daemon_writer =
                    FramedWriter::new(daemon.try_clone().map_err(ServerError::Connect)?);
                daemon_writer.write_body(MessageType::BROKER_ATTACH, 0, &BrokerAttachRequest)?;
                daemon_writer.flush()?;
                return proxy_attached_session(reader, writer.into_inner(), daemon);
            }
        }

        let message = format!("unsupported broker request type {}", frame.msg_type().get());
        writer.write_body(MessageType::ERROR, 0, &ErrorMessage { message })?;
        writer.flush()?;
        return Err(ServerError::UnexpectedMessage(frame.msg_type().get()));
    }
}

fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

#[cfg(unix)]
fn launch_daemon(uid: u32) -> Result<crate::session::SessionId, ServerError> {
    if uid != current_uid() {
        return Err(ServerError::UidMismatch {
            expected: current_uid(),
            actual: uid,
        });
    }

    let socket_dir = socket_dir_for_uid(uid);
    let existing = list_session_socket_ids(&socket_dir)?;
    let session_id = generate_session_id(existing.iter())?;
    let socket_path = socket_path(&socket_dir, session_id.as_str())?;
    let current_exe = std::env::current_exe().map_err(ServerError::CurrentExe)?;
    let mut command = Command::new(current_exe);
    command
        .arg("--daemon")
        .arg("--session")
        .arg(session_id.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // SAFETY: pre_exec runs after fork and before exec in the child. The closure only calls
        // setsid and constructs an io::Error from errno on failure.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(|err| std::io::Error::from_raw_os_error(err as i32))
            });
        }
    }

    command.spawn().map_err(ServerError::SpawnDaemon)?;
    wait_for_daemon_ready(&socket_path)?;
    Ok(session_id)
}

#[cfg(unix)]
fn wait_for_daemon_ready(socket_path: impl AsRef<std::path::Path>) -> Result<(), ServerError> {
    let socket_path = socket_path.as_ref();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_error = None;

    while Instant::now() < deadline {
        match query_daemon_info(socket_path) {
            Ok(_) => return Ok(()),
            Err(ServerError::Connect(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                last_error = Some(err);
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(err),
        }
    }

    Err(ServerError::DaemonNotReady {
        path: socket_path.to_path_buf(),
        source: last_error,
    })
}

#[cfg(unix)]
fn connect_daemon(
    socket: impl AsRef<std::path::Path>,
) -> Result<std::os::unix::net::UnixStream, ServerError> {
    std::os::unix::net::UnixStream::connect(socket).map_err(ServerError::Connect)
}

#[cfg(unix)]
fn proxy_attached_session<R, W>(
    mut client_reader: FramedReader<R>,
    client_writer: W,
    daemon: std::os::unix::net::UnixStream,
) -> Result<(), ServerError>
where
    R: Read + Send,
    W: Write + Send,
{
    use std::net::Shutdown;

    let daemon_reader = daemon.try_clone().map_err(ServerError::Connect)?;
    std::thread::scope(|scope| {
        let daemon_to_client = scope.spawn(move || -> Result<(), ServerError> {
            let mut daemon_reader = FramedReader::new(daemon_reader);
            let mut client_writer = FramedWriter::new(client_writer);

            while let Some(frame) = daemon_reader.read_frame()? {
                let should_close = matches!(
                    frame.msg_type(),
                    msg if msg == MessageType::CLIENT_SHOULD_EXIT
                        || msg == MessageType::EXIT_STATUS
                        || msg == MessageType::SESSION_BUSY
                        || msg == MessageType::ERROR
                );
                client_writer.write_frame(&frame)?;
                client_writer.flush()?;

                if should_close {
                    break;
                }
            }

            Ok(())
        });

        let client_to_daemon = (|| -> Result<(), ServerError> {
            let mut daemon_writer = FramedWriter::new(daemon);
            while let Some(frame) = client_reader.read_frame()? {
                let should_close = frame.msg_type() == MessageType::DETACH;
                daemon_writer.write_frame(&frame)?;
                daemon_writer.flush()?;

                if should_close {
                    break;
                }
            }

            let daemon = daemon_writer.into_inner();
            let _ = daemon.shutdown(Shutdown::Write);
            Ok(())
        })();

        let daemon_to_client = daemon_to_client
            .join()
            .map_err(|_| ServerError::ProxyThreadPanicked)?;
        client_to_daemon?;
        daemon_to_client
    })
}

#[cfg(unix)]
pub fn detach_via_socket(socket: impl AsRef<std::path::Path>) -> Result<(), ServerError> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket).map_err(ServerError::Connect)?;
    let mut writer = FramedWriter::new(stream);
    writer.write_body(MessageType::DETACH, 0, &DetachRequest)?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
pub fn query_daemon_info(socket: impl AsRef<std::path::Path>) -> Result<DaemonInfo, ServerError> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket).map_err(ServerError::Connect)?;
    let reader_stream = stream.try_clone().map_err(ServerError::Connect)?;
    let mut writer = FramedWriter::new(stream);
    writer.write_body(MessageType::DAEMON_INFO_REQUEST, 0, &DaemonInfoRequest)?;
    writer.flush()?;

    let mut reader = FramedReader::new(reader_stream);
    let frame = reader
        .read_frame()?
        .ok_or(ServerError::UnexpectedDaemonEof)?;

    if frame.msg_type() != MessageType::DAEMON_INFO {
        return Err(ServerError::UnexpectedMessage(frame.msg_type().get()));
    }

    Ok(frame.decode_body()?)
}

#[cfg(unix)]
pub fn enumerate_daemons(socket_dir: impl AsRef<Path>) -> Result<Vec<DaemonInfo>, ServerError> {
    let socket_dir = socket_dir.as_ref();
    let session_ids = list_session_socket_ids(socket_dir)?;
    let mut sessions = Vec::new();

    for session_id in session_ids {
        let path = socket_path(socket_dir, session_id.as_str())?;
        match query_daemon_info(&path) {
            Ok(info) => sessions.push(info),
            Err(ServerError::Connect(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                let _ = remove_stale_socket(&path)?;
            }
            Err(err) => return Err(err),
        }
    }

    sessions.sort_by(|left, right| left.session.session_id.cmp(&right.session.session_id));
    Ok(sessions)
}

#[cfg(not(unix))]
pub fn enumerate_daemons(_socket_dir: impl AsRef<Path>) -> Result<Vec<DaemonInfo>, ServerError> {
    Err(ServerError::UnsupportedPlatform(
        "server sockets require Unix-domain sockets",
    ))
}

#[cfg(not(unix))]
pub fn detach_via_socket(_socket: impl AsRef<std::path::Path>) -> Result<(), ServerError> {
    Err(ServerError::UnsupportedPlatform(
        "server control sockets require Unix-domain sockets",
    ))
}

#[cfg(not(unix))]
pub fn query_daemon_info(_socket: impl AsRef<std::path::Path>) -> Result<DaemonInfo, ServerError> {
    Err(ServerError::UnsupportedPlatform(
        "server control sockets require Unix-domain sockets",
    ))
}

#[derive(Debug)]
pub enum ServerError {
    MissingEnvironment(&'static str),
    Connect(std::io::Error),
    Protocol(ProtocolError),
    NoBrokerRequest,
    UnexpectedDaemonEof,
    UnexpectedMessage(u8),
    CurrentExe(std::io::Error),
    SpawnDaemon(std::io::Error),
    DaemonNotReady {
        path: std::path::PathBuf,
        source: Option<std::io::Error>,
    },
    UidMismatch {
        expected: u32,
        actual: u32,
    },
    ProxyThreadPanicked,
    UnsupportedPlatform(&'static str),
    SocketPath(SocketPathError),
    SessionId(SessionIdError),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment(name) => write!(f, "{name} is not set"),
            Self::Connect(err) => write!(f, "failed to connect to daemon socket: {err}"),
            Self::Protocol(err) => write!(f, "failed to send control request: {err}"),
            Self::NoBrokerRequest => write!(f, "broker received no request"),
            Self::UnexpectedDaemonEof => write!(f, "daemon closed connection without a response"),
            Self::UnexpectedMessage(msg_type) => {
                write!(f, "daemon returned unexpected message type {msg_type}")
            }
            Self::CurrentExe(err) => write!(f, "failed to resolve server executable: {err}"),
            Self::SpawnDaemon(err) => write!(f, "failed to launch daemon: {err}"),
            Self::DaemonNotReady { path, source } => {
                write!(f, "daemon did not become ready at {}", path.display())?;
                if let Some(source) = source {
                    write!(f, ": {source}")?;
                }
                Ok(())
            }
            Self::UidMismatch { expected, actual } => {
                write!(
                    f,
                    "broker uid mismatch: request used uid {actual}, expected {expected}"
                )
            }
            Self::ProxyThreadPanicked => write!(f, "broker proxy thread panicked"),
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
            Self::SocketPath(err) => write!(f, "{err}"),
            Self::SessionId(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(err) | Self::CurrentExe(err) | Self::SpawnDaemon(err) => Some(err),
            Self::Protocol(err) => Some(err),
            Self::SocketPath(err) => Some(err),
            Self::SessionId(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProtocolError> for ServerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<SocketPathError> for ServerError {
    fn from(value: SocketPathError) -> Self {
        Self::SocketPath(value)
    }
}

impl From<SessionIdError> for ServerError {
    fn from(value: SessionIdError) -> Self {
        Self::SessionId(value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::{
        AttachSessionRequest, BrokerAttachRequest, DetachRequest, ExitStatus, NewSessionRequest,
        PtyData, SessionListRequest, SessionRecord, UnixTimeMillis,
    };
    use crate::session::prepare_socket_dir;
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detach_via_socket_sends_detach_request() {
        let Some((listener, path)) = test_listener("detach") else {
            return;
        };

        let thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = FramedReader::new(stream);
            let frame = reader.read_frame().unwrap().unwrap();
            assert_eq!(frame.msg_type(), MessageType::DETACH);
            let _: DetachRequest = frame.decode_body().unwrap();
        });

        detach_via_socket(&path).unwrap();
        thread.join().unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn query_daemon_info_sends_request_and_decodes_response() {
        let Some((listener, path)) = test_listener("info") else {
            return;
        };

        let thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let reader_stream = stream.try_clone().unwrap();
            let mut reader = FramedReader::new(reader_stream);
            let frame = reader.read_frame().unwrap().unwrap();
            assert_eq!(frame.msg_type(), MessageType::DAEMON_INFO_REQUEST);
            let _: DaemonInfoRequest = frame.decode_body().unwrap();

            let response = DaemonInfo {
                session: SessionRecord {
                    session_id: "aaaaaaaa".to_string(),
                    init_time: UnixTimeMillis(1),
                    last_detach_time: None,
                    current_command: "bash".to_string(),
                    attached: false,
                },
            };
            let mut writer = FramedWriter::new(stream);
            writer
                .write_body(MessageType::DAEMON_INFO, 0, &response)
                .unwrap();
            writer.flush().unwrap();
        });

        let info = query_daemon_info(&path).unwrap();
        assert_eq!(info.session.session_id, "aaaaaaaa");
        thread.join().unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn enumerate_daemons_on_missing_dir_is_empty() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("ssh-obi-missing-enumerate-dir");
        let _ = fs::remove_dir_all(&path);

        assert!(enumerate_daemons(&path).unwrap().is_empty());
    }

    #[test]
    fn broker_list_request_returns_session_list() {
        let mut request = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut request);
            writer
                .write_body(
                    MessageType::SESSION_LIST_REQUEST,
                    0,
                    &SessionListRequest {
                        continue_after_response: false,
                    },
                )
                .unwrap();
        }

        let mut response = Vec::new();
        handle_broker_request(Cursor::new(request), &mut response, 4_294_967_295).unwrap();

        let mut reader = FramedReader::new(response.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::SESSION_LIST);
        let list: SessionList = frame.decode_body().unwrap();
        assert!(list.sessions.is_empty());
    }

    #[test]
    fn broker_new_session_rejects_wrong_uid() {
        let mut request = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut request);
            writer
                .write_body(MessageType::NEW_SESSION, 0, &NewSessionRequest)
                .unwrap();
        }

        let mut response = Vec::new();
        let err =
            handle_broker_request(Cursor::new(request), &mut response, 4_294_967_295).unwrap_err();
        assert!(matches!(
            err,
            ServerError::UidMismatch {
                actual: 4_294_967_295,
                ..
            }
        ));
        assert!(response.is_empty());
    }

    #[test]
    fn broker_attach_proxies_daemon_frames() {
        let uid = current_uid();
        let socket_dir = socket_dir_for_uid(uid);
        prepare_socket_dir(&socket_dir, uid).unwrap();
        let path = socket_path(&socket_dir, "aaaaaaaa").unwrap();
        let _ = fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping Unix socket test because bind is not permitted here");
                return;
            }
            Err(err) => panic!("failed to bind test Unix socket: {err}"),
        };

        let daemon = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let reader_stream = stream.try_clone().unwrap();
            let mut reader = FramedReader::new(reader_stream);
            let frame = reader.read_frame().unwrap().unwrap();
            assert_eq!(frame.msg_type(), MessageType::BROKER_ATTACH);
            let _: BrokerAttachRequest = frame.decode_body().unwrap();

            let mut writer = FramedWriter::new(stream);
            writer
                .write_body(
                    MessageType::PTY_DATA,
                    0,
                    &PtyData {
                        bytes: b"hello".to_vec(),
                    },
                )
                .unwrap();
            writer
                .write_body(
                    MessageType::EXIT_STATUS,
                    0,
                    &ExitStatus {
                        code: Some(0),
                        signal: None,
                    },
                )
                .unwrap();
            writer.flush().unwrap();
        });

        let mut request = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut request);
            writer
                .write_body(
                    MessageType::ATTACH_SESSION,
                    0,
                    &AttachSessionRequest {
                        session_id: "aaaaaaaa".to_string(),
                    },
                )
                .unwrap();
        }

        let mut response = Vec::new();
        handle_broker_request(Cursor::new(request), &mut response, uid).unwrap();

        let mut reader = FramedReader::new(response.as_slice());
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::PTY_DATA);
        let data: PtyData = frame.decode_body().unwrap();
        assert_eq!(data.bytes, b"hello");

        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type(), MessageType::EXIT_STATUS);
        let status: ExitStatus = frame.decode_body().unwrap();
        assert_eq!(status.code, Some(0));
        assert_eq!(status.signal, None);

        daemon.join().unwrap();
        let _ = fs::remove_file(&path);
    }

    fn test_listener(name: &str) -> Option<(UnixListener, std::path::PathBuf)> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::current_dir().unwrap().join("target");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "ssh-obi-{name}-test-{}-{unique}.sock",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => Some((listener, path)),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping Unix socket test because bind is not permitted here");
                None
            }
            Err(err) => panic!("failed to bind test Unix socket: {err}"),
        }
    }
}
