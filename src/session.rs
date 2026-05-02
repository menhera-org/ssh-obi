use std::fmt;
use std::time::SystemTime;

use chrono::{DateTime, Local};

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

fn display_time(time: SystemTime) -> String {
    let local: DateTime<Local> = time.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

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
}
