use std::env;
use std::fmt;

use crate::protocol::{DetachRequest, MessageType, ProtocolError};
use crate::transport::FramedWriter;

pub const ENV_SESSION: &str = "SSH_OBI_SESSION";
pub const ENV_SOCKET: &str = "SSH_OBI_SOCKET";

pub fn detach_from_env() -> Result<(), ServerError> {
    let socket = env::var(ENV_SOCKET).map_err(|_| ServerError::MissingEnvironment(ENV_SOCKET))?;
    detach_via_socket(socket)
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

#[cfg(not(unix))]
pub fn detach_via_socket(_socket: impl AsRef<std::path::Path>) -> Result<(), ServerError> {
    Err(ServerError::UnsupportedPlatform(
        "server control sockets require Unix-domain sockets",
    ))
}

#[derive(Debug)]
pub enum ServerError {
    MissingEnvironment(&'static str),
    Connect(std::io::Error),
    Protocol(ProtocolError),
    UnsupportedPlatform(&'static str),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment(name) => write!(f, "{name} is not set"),
            Self::Connect(err) => write!(f, "failed to connect to daemon socket: {err}"),
            Self::Protocol(err) => write!(f, "failed to send control request: {err}"),
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProtocolError> for ServerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::DetachRequest;
    use crate::transport::FramedReader;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detach_via_socket_sends_detach_request() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::current_dir().unwrap().join("target");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "ssh-obi-detach-test-{}-{unique}.sock",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping Unix socket test because bind is not permitted here");
                return;
            }
            Err(err) => panic!("failed to bind test Unix socket: {err}"),
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
}
