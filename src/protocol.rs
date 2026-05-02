use std::fmt;
use std::io::{self, Read, Write};

pub const HEADER_LEN: usize = 6;
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;
pub const CURRENT_PROTOCOL_BASELINE: &str = "0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageType(u8);

impl MessageType {
    pub const CAPABILITIES: Self = Self(1);
    pub const SESSION_LIST_REQUEST: Self = Self(2);
    pub const SESSION_LIST: Self = Self(3);
    pub const NEW_SESSION: Self = Self(4);
    pub const ATTACH_SESSION: Self = Self(5);
    pub const BROKER_ATTACH: Self = Self(6);
    pub const DAEMON_INFO_REQUEST: Self = Self(7);
    pub const DAEMON_INFO: Self = Self(8);
    pub const DETACH: Self = Self(9);
    pub const CLIENT_SHOULD_EXIT: Self = Self(10);
    pub const SESSION_BUSY: Self = Self(11);
    pub const PTY_DATA: Self = Self(12);
    pub const WINDOW_SIZE: Self = Self(13);
    pub const EXIT_STATUS: Self = Self(14);
    pub const ERROR: Self = Self(15);

    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::CAPABILITIES => "Capabilities",
            Self::SESSION_LIST_REQUEST => "SessionListRequest",
            Self::SESSION_LIST => "SessionList",
            Self::NEW_SESSION => "NewSession",
            Self::ATTACH_SESSION => "AttachSession",
            Self::BROKER_ATTACH => "BrokerAttach",
            Self::DAEMON_INFO_REQUEST => "DaemonInfoRequest",
            Self::DAEMON_INFO => "DaemonInfo",
            Self::DETACH => "Detach",
            Self::CLIENT_SHOULD_EXIT => "ClientShouldExit",
            Self::SESSION_BUSY => "SessionBusy",
            Self::PTY_DATA => "PtyData",
            Self::WINDOW_SIZE => "WindowSize",
            Self::EXIT_STATUS => "ExitStatus",
            Self::ERROR => "Error",
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    msg_type: MessageType,
    flags: u8,
    payload: Vec<u8>,
}

impl Frame {
    pub fn new(msg_type: MessageType, flags: u8, payload: Vec<u8>) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                len: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        Ok(Self {
            msg_type,
            flags,
            payload,
        })
    }

    pub fn msg_type(&self) -> MessageType {
        self.msg_type
    }

    pub fn flags(&self) -> u8 {
        self.flags
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), ProtocolError> {
        let len =
            u32::try_from(self.payload.len()).map_err(|_| ProtocolError::PayloadTooLarge {
                len: self.payload.len(),
                max: MAX_PAYLOAD_LEN,
            })?;

        if self.payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                len: self.payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        writer.write_all(&[self.msg_type.get(), self.flags])?;
        writer.write_all(&len.to_be_bytes())?;
        writer.write_all(&self.payload)?;
        Ok(())
    }
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Frame>, ProtocolError> {
    let mut header = [0u8; HEADER_LEN];

    match reader.read(&mut header[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(_) => unreachable!("one-byte read returned more than one byte"),
        Err(err) => return Err(err.into()),
    }

    reader
        .read_exact(&mut header[1..])
        .map_err(|err| match err.kind() {
            io::ErrorKind::UnexpectedEof => ProtocolError::TruncatedHeader,
            _ => ProtocolError::Io(err),
        })?;

    let len = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    if len > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::PayloadTooLarge {
            len,
            max: MAX_PAYLOAD_LEN,
        });
    }

    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(|err| match err.kind() {
            io::ErrorKind::UnexpectedEof => ProtocolError::TruncatedPayload {
                expected: len,
                actual: 0,
            },
            _ => ProtocolError::Io(err),
        })?;

    Frame::new(MessageType::new(header[0]), header[1], payload).map(Some)
}

pub fn supports_protocol_baseline(baseline: &str) -> bool {
    matches!(baseline, "0.1" | "0.1.0")
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    TruncatedHeader,
    TruncatedPayload { expected: usize, actual: usize },
    PayloadTooLarge { len: usize, max: usize },
    InvalidMessage(&'static str),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::TruncatedHeader => write!(f, "truncated protocol frame header"),
            Self::TruncatedPayload { expected, actual } => {
                write!(
                    f,
                    "truncated protocol frame payload: expected {expected} bytes, got {actual}"
                )
            }
            Self::PayloadTooLarge { len, max } => {
                write!(f, "protocol frame payload is too large: {len} > {max}")
            }
            Self::InvalidMessage(reason) => write!(f, "invalid protocol message: {reason}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_round_trips() {
        let frame = Frame::new(MessageType::DETACH, 7, b"payload".to_vec()).unwrap();
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes).unwrap();

        let decoded = read_frame(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn clean_eof_before_header_is_none() {
        assert!(
            read_frame(&mut Cursor::new(Vec::<u8>::new()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn oversized_payload_is_rejected_before_allocation() {
        let mut bytes = Vec::new();
        bytes.push(MessageType::PTY_DATA.get());
        bytes.push(0);
        bytes.extend_from_slice(&((MAX_PAYLOAD_LEN as u32) + 1).to_be_bytes());

        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, ProtocolError::PayloadTooLarge { .. }));
    }
}
