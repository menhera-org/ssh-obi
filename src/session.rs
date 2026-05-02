use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};

use crate::protocol::{SessionRecord, UnixTimeMillis};

pub const SOCKET_PATH_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionIdError> {
        let value = value.into();
        let len = value.len();

        if !(8..=10).contains(&len) {
            return Err(SessionIdError::InvalidLength { len });
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
        {
            return Err(SessionIdError::InvalidCharacter);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionIdError {
    InvalidLength { len: usize },
    InvalidCharacter,
}

impl fmt::Display for SessionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { len } => {
                write!(f, "session id must be 8-10 base32 characters, got {len}")
            }
            Self::InvalidCharacter => {
                write!(
                    f,
                    "session id must contain only lowercase base32 characters"
                )
            }
        }
    }
}

impl std::error::Error for SessionIdError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketPathError {
    InvalidSessionId(SessionIdError),
    TooLong { len: usize, max: usize },
}

impl fmt::Display for SocketPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSessionId(err) => write!(f, "{err}"),
            Self::TooLong { len, max } => {
                write!(f, "socket path is too long: {len} bytes exceeds {max}")
            }
        }
    }
}

impl std::error::Error for SocketPathError {}

impl From<SessionIdError> for SocketPathError {
    fn from(value: SessionIdError) -> Self {
        Self::InvalidSessionId(value)
    }
}

pub fn socket_dir_for_uid(uid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("ssh-obi-{uid}"))
}

pub fn socket_path_for_uid(uid: u32, session_id: &str) -> Result<PathBuf, SocketPathError> {
    socket_path(socket_dir_for_uid(uid), session_id)
}

pub fn socket_path(
    socket_dir: impl AsRef<Path>,
    session_id: &str,
) -> Result<PathBuf, SocketPathError> {
    let session_id = SessionId::new(session_id)?;
    let path = socket_dir
        .as_ref()
        .join(format!("{}.sock", session_id.as_str()));
    let len = path.as_os_str().as_encoded_bytes().len();

    if len > SOCKET_PATH_LIMIT {
        return Err(SocketPathError::TooLong {
            len,
            max: SOCKET_PATH_LIMIT,
        });
    }

    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Free,
    Busy,
}

impl SessionState {
    pub fn is_free(self) -> bool {
        matches!(self, Self::Free)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub init_time: SystemTime,
    pub last_detach_time: Option<SystemTime>,
    pub current_command: String,
    pub state: SessionState,
}

impl SessionInfo {
    pub fn is_selectable(&self) -> bool {
        self.state.is_free()
    }

    pub fn from_record(record: SessionRecord) -> Result<Self, SessionIdError> {
        Ok(Self {
            id: SessionId::new(record.session_id)?,
            init_time: system_time_from_millis(record.init_time),
            last_detach_time: record.last_detach_time.map(system_time_from_millis),
            current_command: record.current_command,
            state: if record.attached {
                SessionState::Busy
            } else {
                SessionState::Free
            },
        })
    }
}

fn system_time_from_millis(millis: UnixTimeMillis) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSelection {
    NewSession,
    Attach(SessionId),
    Prompt,
}

pub fn auto_select(sessions: &[SessionInfo]) -> AutoSelection {
    let mut free = sessions.iter().filter(|session| session.is_selectable());
    let Some(first) = free.next() else {
        return AutoSelection::NewSession;
    };

    if free.next().is_some() {
        AutoSelection::Prompt
    } else {
        AutoSelection::Attach(first.id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub selector: Option<usize>,
    pub session_id: SessionId,
    pub init_time: String,
    pub detach_time: String,
    pub state: SessionState,
    pub current_command: String,
}

pub fn picker_rows(sessions: &[SessionInfo]) -> Vec<PickerRow> {
    let mut next_selector = 1;

    sessions
        .iter()
        .map(|session| {
            let selector = if session.is_selectable() {
                let selector = next_selector;
                next_selector += 1;
                Some(selector)
            } else {
                None
            };

            PickerRow {
                selector,
                session_id: session.id.clone(),
                init_time: display_time(session.init_time),
                detach_time: session
                    .last_detach_time
                    .map(display_time)
                    .unwrap_or_else(|| "-".to_string()),
                state: session.state,
                current_command: session.current_command.clone(),
            }
        })
        .collect()
}

pub fn render_session_table(sessions: &[SessionInfo], include_new: bool) -> String {
    let rows = picker_rows(sessions);
    let mut output = String::new();
    output.push_str("  #   STATE  INIT              DETACH            WHAT\n");

    for row in rows {
        let selector = row
            .selector
            .map(|selector| selector.to_string())
            .unwrap_or_else(|| "-".to_string());
        let state = match row.state {
            SessionState::Free => "free",
            SessionState::Busy => "busy",
        };

        output.push_str(&format!(
            "{selector:>3}   {state:<5}  {:<16}  {:<16}  {}\n",
            row.init_time, row.detach_time, row.current_command
        ));
    }

    if include_new {
        output.push_str("  n   new\n");
    }

    output
}

fn display_time(time: SystemTime) -> String {
    let local: DateTime<Local> = time.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, state: SessionState) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(id).unwrap(),
            init_time: UNIX_EPOCH + Duration::from_secs(1),
            last_detach_time: None,
            current_command: "bash".to_string(),
            state,
        }
    }

    #[test]
    fn busy_sessions_are_not_auto_selected() {
        let sessions = vec![
            session("aaaaaaaa", SessionState::Busy),
            session("bbbbbbbb", SessionState::Free),
        ];

        assert_eq!(
            auto_select(&sessions),
            AutoSelection::Attach(SessionId::new("bbbbbbbb").unwrap())
        );
    }

    #[test]
    fn multiple_free_sessions_prompt() {
        let sessions = vec![
            session("aaaaaaaa", SessionState::Free),
            session("bbbbbbbb", SessionState::Free),
        ];

        assert_eq!(auto_select(&sessions), AutoSelection::Prompt);
    }

    #[test]
    fn picker_numbers_only_free_sessions() {
        let rows = picker_rows(&[
            session("aaaaaaaa", SessionState::Busy),
            session("bbbbbbbb", SessionState::Free),
            session("cccccccc", SessionState::Free),
        ]);

        assert_eq!(rows[0].selector, None);
        assert_eq!(rows[1].selector, Some(1));
        assert_eq!(rows[2].selector, Some(2));
        assert_eq!(rows[0].detach_time, "-");
    }

    #[test]
    fn session_table_marks_busy_rows_unselectable() {
        let table = render_session_table(
            &[
                session("aaaaaaaa", SessionState::Busy),
                session("bbbbbbbb", SessionState::Free),
            ],
            true,
        );

        assert!(table.contains("  -   busy"));
        assert!(table.contains("  1   free"));
        assert!(table.contains("  n   new"));
    }

    #[test]
    fn session_info_converts_from_protocol_record() {
        let info = SessionInfo::from_record(SessionRecord {
            session_id: "aaaaaaaa".to_string(),
            init_time: UnixTimeMillis(1_000),
            last_detach_time: Some(UnixTimeMillis(2_000)),
            current_command: "vim notes.md".to_string(),
            attached: true,
        })
        .unwrap();

        assert_eq!(info.id.as_str(), "aaaaaaaa");
        assert_eq!(info.init_time, UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(
            info.last_detach_time,
            Some(UNIX_EPOCH + Duration::from_secs(2))
        );
        assert_eq!(info.current_command, "vim notes.md");
        assert_eq!(info.state, SessionState::Busy);
    }

    #[test]
    fn socket_path_uses_uid_namespace() {
        let path = socket_path_for_uid(1234, "aaaaaaaa").unwrap();
        assert!(path.ends_with("ssh-obi-1234/aaaaaaaa.sock"));
    }

    #[test]
    fn socket_path_rejects_overlong_paths() {
        let dir = "/tmp/".to_string() + &"x".repeat(SOCKET_PATH_LIMIT);
        let err = socket_path(dir, "aaaaaaaa").unwrap_err();
        assert!(matches!(err, SocketPathError::TooLong { .. }));
    }
}
