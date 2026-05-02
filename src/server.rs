use std::env;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

use crate::protocol::{
    DaemonInfo, DaemonInfoRequest, DetachRequest, DetachSessionRequest, ErrorMessage, MessageType,
    ProtocolError, SessionList,
};
use crate::session::{
    SocketPathError, list_session_socket_ids, remove_stale_socket, socket_dir_for_uid, socket_path,
    socket_path_for_uid,
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
    handle_broker_request(stdin.lock(), stdout.lock(), current_uid())
}

pub fn handle_broker_request<R, W>(reader: R, writer: W, uid: u32) -> Result<(), ServerError>
where
    R: Read,
    W: Write,
{
    let mut reader = FramedReader::new(reader);
    let mut writer = FramedWriter::new(writer);
    let frame = reader.read_frame()?.ok_or(ServerError::NoBrokerRequest)?;

    if frame.msg_type() == MessageType::SESSION_LIST_REQUEST {
        let socket_dir = socket_dir_for_uid(uid);
        let sessions = enumerate_daemons(socket_dir)?
            .into_iter()
            .map(|info| info.session)
            .collect();
        writer.write_body(MessageType::SESSION_LIST, 0, &SessionList { sessions })?;
        writer.flush()?;
        return Ok(());
    }

    if frame.msg_type() == MessageType::DETACH {
        let request: DetachSessionRequest = frame.decode_body()?;
        let path = socket_path_for_uid(uid, &request.session_id)?;
        detach_via_socket(path)?;
        return Ok(());
    }

    let message = format!("unsupported broker request type {}", frame.msg_type().get());
    writer.write_body(MessageType::ERROR, 0, &ErrorMessage { message })?;
    writer.flush()?;
    Err(ServerError::UnexpectedMessage(frame.msg_type().get()))
}

fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
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
    UnsupportedPlatform(&'static str),
    SocketPath(SocketPathError),
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
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
            Self::SocketPath(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Protocol(err) => Some(err),
            Self::SocketPath(err) => Some(err),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::{DetachRequest, SessionListRequest, SessionRecord, UnixTimeMillis};
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
                .write_body(MessageType::SESSION_LIST_REQUEST, 0, &SessionListRequest)
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
