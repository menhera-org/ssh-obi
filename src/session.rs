use std::fmt;
use std::fs;
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
    TooLong {
        len: usize,
        max: usize,
    },
    Io(String),
    NotDirectory(PathBuf),
    WrongOwner {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    WrongMode {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    UnsupportedPlatform(&'static str),
}

impl fmt::Display for SocketPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSessionId(err) => write!(f, "{err}"),
            Self::TooLong { len, max } => {
                write!(f, "socket path is too long: {len} bytes exceeds {max}")
            }
            Self::Io(message) => write!(f, "{message}"),
            Self::NotDirectory(path) => {
                write!(f, "socket path is not a directory: {}", path.display())
            }
            Self::WrongOwner {
                path,
                expected,
                actual,
            } => write!(
                f,
                "socket directory {} is owned by uid {actual}, expected {expected}",
                path.display()
            ),
            Self::WrongMode {
                path,
                expected,
                actual,
            } => write!(
                f,
                "socket directory {} has mode {actual:o}, expected {expected:o}",
                path.display()
            ),
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
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

pub fn session_id_from_socket_path(path: impl AsRef<Path>) -> Result<SessionId, SessionIdError> {
    let Some(file_name) = path.as_ref().file_name().and_then(|value| value.to_str()) else {
        return Err(SessionIdError::InvalidCharacter);
    };

    let Some(session_id) = file_name.strip_suffix(".sock") else {
        return Err(SessionIdError::InvalidCharacter);
    };

    SessionId::new(session_id)
}

pub fn list_session_socket_ids(
    socket_dir: impl AsRef<Path>,
) -> Result<Vec<SessionId>, SocketPathError> {
    let socket_dir = socket_dir.as_ref();
    let mut sessions = Vec::new();

    match fs::read_dir(socket_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|err| {
                    SocketPathError::Io(format!(
                        "failed to read socket directory {}: {err}",
                        socket_dir.display()
                    ))
                })?;

                if let Ok(session_id) = session_id_from_socket_path(entry.path()) {
                    sessions.push(session_id);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(SocketPathError::Io(format!(
                "failed to read socket directory {}: {err}",
                socket_dir.display()
            )));
        }
    }

    sessions.sort();
    Ok(sessions)
}

#[cfg(unix)]
pub fn prepare_socket_dir(
    socket_dir: impl AsRef<Path>,
    expected_uid: u32,
) -> Result<(), SocketPathError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let socket_dir = socket_dir.as_ref();
    if !socket_dir.exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(socket_dir)
            .map_err(|err| {
                SocketPathError::Io(format!(
                    "failed to create socket directory {}: {err}",
                    socket_dir.display()
                ))
            })?;
    }

    let metadata = fs::metadata(socket_dir).map_err(|err| {
        SocketPathError::Io(format!(
            "failed to stat socket directory {}: {err}",
            socket_dir.display()
        ))
    })?;

    if !metadata.is_dir() {
        return Err(SocketPathError::NotDirectory(socket_dir.to_path_buf()));
    }

    let actual_uid = metadata.uid();
    if actual_uid != expected_uid {
        return Err(SocketPathError::WrongOwner {
            path: socket_dir.to_path_buf(),
            expected: expected_uid,
            actual: actual_uid,
        });
    }

    let actual_mode = metadata.permissions().mode() & 0o777;
    if actual_mode != 0o700 {
        return Err(SocketPathError::WrongMode {
            path: socket_dir.to_path_buf(),
            expected: 0o700,
            actual: actual_mode,
        });
    }

    Ok(())
}

#[cfg(not(unix))]
pub fn prepare_socket_dir(
    _socket_dir: impl AsRef<Path>,
    _expected_uid: u32,
) -> Result<(), SocketPathError> {
    Err(SocketPathError::UnsupportedPlatform(
        "server socket directories require Unix permissions",
    ))
}

#[cfg(unix)]
pub fn remove_stale_socket(path: impl AsRef<Path>) -> Result<bool, SocketPathError> {
    use std::os::unix::net::UnixStream;

    let path = path.as_ref();
    match UnixStream::connect(path) {
        Ok(_stream) => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path).map_err(|err| {
                SocketPathError::Io(format!(
                    "failed to remove stale socket {}: {err}",
                    path.display()
                ))
            })?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(SocketPathError::Io(format!(
            "failed to probe socket {}: {err}",
            path.display()
        ))),
    }
}

#[cfg(not(unix))]
pub fn remove_stale_socket(_path: impl AsRef<Path>) -> Result<bool, SocketPathError> {
    Err(SocketPathError::UnsupportedPlatform(
        "server sockets require Unix-domain sockets",
    ))
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

    #[test]
    fn session_id_from_socket_path_requires_sock_suffix() {
        assert_eq!(
            session_id_from_socket_path("/tmp/ssh-obi-1/aaaaaaaa.sock").unwrap(),
            SessionId::new("aaaaaaaa").unwrap()
        );
        assert!(session_id_from_socket_path("/tmp/ssh-obi-1/aaaaaaaa.txt").is_err());
    }

    #[test]
    fn list_session_socket_ids_filters_and_sorts() {
        let dir = test_dir("list-session-socket-ids");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bbbbbbbb.sock"), "").unwrap();
        fs::write(dir.join("not-a-session.sock"), "").unwrap();
        fs::write(dir.join("aaaaaaaa.sock"), "").unwrap();
        fs::write(dir.join("cccccccc.txt"), "").unwrap();

        let sessions = list_session_socket_ids(&dir).unwrap();
        assert_eq!(
            sessions,
            vec![
                SessionId::new("aaaaaaaa").unwrap(),
                SessionId::new("bbbbbbbb").unwrap()
            ]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prepare_socket_dir_creates_private_directory() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = test_dir("prepare-socket-dir");
        let uid = current_uid();
        prepare_socket_dir(&dir, uid).unwrap();

        let metadata = fs::metadata(&dir).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.uid(), uid);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prepare_socket_dir_rejects_wrong_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("prepare-socket-dir-wrong-mode");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let err = prepare_socket_dir(&dir, current_uid()).unwrap_err();
        assert!(matches!(
            err,
            SocketPathError::WrongMode { actual: 0o755, .. }
        ));

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    fn current_uid() -> u32 {
        use std::os::unix::fs::MetadataExt;

        fs::metadata(".").unwrap().uid()
    }

    fn test_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("ssh-obi-{name}-{}-{unique}", std::process::id()))
    }
}
