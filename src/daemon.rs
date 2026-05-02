use std::collections::VecDeque;
use std::fmt;

pub const DEFAULT_REPLAY_CAPACITY: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBuffer {
    capacity: usize,
    bytes: VecDeque<u8>,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Result<Self, ReplayBufferError> {
        if capacity == 0 {
            return Err(ReplayBufferError::ZeroCapacity);
        }

        Ok(Self {
            capacity,
            bytes: VecDeque::with_capacity(capacity),
        })
    }

    pub fn default_capacity() -> Self {
        Self::new(DEFAULT_REPLAY_CAPACITY).expect("default replay capacity is nonzero")
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            self.bytes.clear();
            self.bytes
                .extend(bytes[bytes.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(bytes.iter().copied());
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self::default_capacity()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayBufferError {
    ZeroCapacity,
}

impl fmt::Display for ReplayBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => write!(f, "replay buffer capacity must be greater than zero"),
        }
    }
}

impl std::error::Error for ReplayBufferError {}

#[cfg(unix)]
mod runtime {
    use std::fmt;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use nix::sys::wait::{WaitStatus, waitpid};

    use crate::daemon::ReplayBuffer;
    use crate::protocol::{
        BrokerAttachRequest, DaemonInfo, DaemonInfoRequest, DetachRequest, ExitStatus, MessageType,
        ProtocolError, PtyData, SessionBusy, SessionRecord, UnixTimeMillis, WindowSize,
    };
    use crate::pty::{PtyError, set_window_size, spawn_pty_command};
    use crate::server::{ENV_SESSION, ENV_SOCKET};
    use crate::session::{
        SessionId, SessionIdError, SocketPathError, prepare_socket_dir, remove_stale_socket,
        socket_dir_for_uid, socket_path,
    };
    use crate::transport::{FramedReader, FramedWriter};

    const ACCEPT_POLL: Duration = Duration::from_millis(50);

    pub fn run_daemon(session_id: SessionId) -> Result<(), DaemonError> {
        let uid = nix::unistd::Uid::current().as_raw();
        let socket_dir = socket_dir_for_uid(uid);
        prepare_socket_dir(&socket_dir, uid)?;
        let socket_path = socket_path(&socket_dir, session_id.as_str())?;

        if socket_path.exists() && !remove_stale_socket(&socket_path)? {
            return Err(DaemonError::SocketAlreadyExists(socket_path));
        }

        let listener = UnixListener::bind(&socket_path).map_err(DaemonError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(DaemonError::Listen)?;

        daemonize_after_bind()?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let pty = spawn_pty_command(
            &shell,
            &[],
            &[
                (ENV_SESSION, session_id.as_str()),
                (ENV_SOCKET, socket_path.to_string_lossy().as_ref()),
            ],
            None,
        )?;
        let master_write = nix::unistd::dup(pty.master.as_fd()).map_err(DaemonError::DupPty)?;
        let master_write = Arc::new(Mutex::new(fs::File::from(master_write)));
        let master_read = fs::File::from(pty.master);

        let state = Arc::new(Mutex::new(DaemonState::new(
            session_id,
            SystemTime::now(),
            shell_command_name(&shell),
        )));

        spawn_pty_reader(master_read, Arc::clone(&state));
        spawn_child_waiter(pty.child, socket_path.clone(), Arc::clone(&state));

        while !state.lock().expect("daemon state poisoned").shutdown {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    let master_write = Arc::clone(&master_write);
                    thread::spawn(move || {
                        let _ = handle_daemon_stream(stream, state, master_write);
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                }
                Err(err) => return Err(DaemonError::Accept(err)),
            }
        }

        let _ = fs::remove_file(&socket_path);
        Ok(())
    }

    fn daemonize_after_bind() -> Result<(), DaemonError> {
        use nix::sys::stat::{Mode, umask};
        use nix::unistd::{ForkResult, chdir, dup2_stderr, dup2_stdin, dup2_stdout, fork, setsid};

        match unsafe { fork() }.map_err(DaemonError::Fork)? {
            ForkResult::Parent { .. } => unsafe { nix::libc::_exit(0) },
            ForkResult::Child => {}
        }

        setsid().map_err(DaemonError::SetSid)?;

        match unsafe { fork() }.map_err(DaemonError::Fork)? {
            ForkResult::Parent { .. } => unsafe { nix::libc::_exit(0) },
            ForkResult::Child => {}
        }

        chdir("/").map_err(DaemonError::Chdir)?;
        umask(Mode::empty());

        let dev_null = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .map_err(DaemonError::OpenDevNull)?;
        dup2_stdin(&dev_null).map_err(DaemonError::RedirectStdio)?;
        dup2_stdout(&dev_null).map_err(DaemonError::RedirectStdio)?;
        dup2_stderr(&dev_null).map_err(DaemonError::RedirectStdio)?;

        Ok(())
    }

    fn spawn_pty_reader(mut master_read: fs::File, state: Arc<Mutex<DaemonState>>) {
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match master_read.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let bytes = &buf[..n];
                        let broker = {
                            let mut state = state.lock().expect("daemon state poisoned");
                            state.replay.append(bytes);
                            state.broker.as_ref().map(|broker| broker.writer.clone())
                        };

                        if let Some(writer) = broker
                            && write_broker_pty_data(writer, bytes).is_err()
                        {
                            let mut state = state.lock().expect("daemon state poisoned");
                            state.detach_current(SystemTime::now());
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
    }

    fn spawn_child_waiter(
        child: nix::unistd::Pid,
        socket_path: PathBuf,
        state: Arc<Mutex<DaemonState>>,
    ) {
        thread::spawn(move || {
            let status = waitpid(child, None)
                .map(exit_status_from_wait)
                .unwrap_or(ExitStatus {
                    code: None,
                    signal: None,
                });
            let broker = {
                let mut state = state.lock().expect("daemon state poisoned");
                state.shutdown = true;
                state.broker.as_ref().map(|broker| broker.writer.clone())
            };

            if let Some(writer) = broker {
                let _ = write_broker_exit_status(writer, &status);
            }
            let _ = fs::remove_file(socket_path);
        });
    }

    fn exit_status_from_wait(status: WaitStatus) -> ExitStatus {
        match status {
            WaitStatus::Exited(_, code) => ExitStatus {
                code: Some(code),
                signal: None,
            },
            WaitStatus::Signaled(_, signal, _) => ExitStatus {
                code: None,
                signal: Some(signal as i32),
            },
            _ => ExitStatus {
                code: None,
                signal: None,
            },
        }
    }

    fn handle_daemon_stream(
        stream: UnixStream,
        state: Arc<Mutex<DaemonState>>,
        master_write: Arc<Mutex<fs::File>>,
    ) -> Result<(), DaemonError> {
        let mut reader = FramedReader::new(stream.try_clone().map_err(DaemonError::StreamClone)?);
        let frame = reader.read_frame()?.ok_or(DaemonError::NoRequest)?;

        if frame.msg_type() == MessageType::DAEMON_INFO_REQUEST {
            let _: DaemonInfoRequest = frame.decode_body()?;
            let info = {
                let state = state.lock().expect("daemon state poisoned");
                DaemonInfo {
                    session: state.record(),
                }
            };
            let mut writer = FramedWriter::new(stream);
            writer.write_body(MessageType::DAEMON_INFO, 0, &info)?;
            writer.flush()?;
            return Ok(());
        }

        if frame.msg_type() == MessageType::DETACH {
            let _: DetachRequest = frame.decode_body()?;
            let broker = {
                let mut state = state.lock().expect("daemon state poisoned");
                state.detach_current(SystemTime::now())
            };
            if let Some(writer) = broker {
                write_client_should_exit(writer)?;
            }
            return Ok(());
        }

        if frame.msg_type() == MessageType::BROKER_ATTACH
            || frame.msg_type() == MessageType::ATTACH_SESSION
        {
            if frame.msg_type() == MessageType::BROKER_ATTACH {
                let _: BrokerAttachRequest = frame.decode_body()?;
            }

            return attach_broker(stream, reader, state, master_write);
        }

        Err(DaemonError::UnexpectedMessage(frame.msg_type().get()))
    }

    fn attach_broker(
        mut stream: UnixStream,
        mut reader: FramedReader<UnixStream>,
        state: Arc<Mutex<DaemonState>>,
        master_write: Arc<Mutex<fs::File>>,
    ) -> Result<(), DaemonError> {
        let writer = Arc::new(Mutex::new(
            stream.try_clone().map_err(DaemonError::StreamClone)?,
        ));
        let (token, replay) = {
            let mut state = state.lock().expect("daemon state poisoned");
            let session_id = state.session_id.to_string();
            if state.broker.is_some() {
                let mut writer = FramedWriter::new(&mut stream);
                writer.write_body(MessageType::SESSION_BUSY, 0, &SessionBusy { session_id })?;
                writer.flush()?;
                return Ok(());
            }

            let token = state.next_attach_token();
            let replay = state.replay.snapshot();
            state.broker = Some(AttachedBroker {
                token,
                writer: writer.clone(),
            });
            (token, replay)
        };

        if !replay.is_empty() {
            write_broker_pty_data(writer.clone(), &replay)?;
        }

        while let Some(frame) = reader.read_frame()? {
            if frame.msg_type() == MessageType::PTY_DATA {
                let data: PtyData = frame.decode_body()?;
                let mut master = master_write.lock().expect("pty writer poisoned");
                master
                    .write_all(&data.bytes)
                    .map_err(DaemonError::PtyWrite)?;
                master.flush().map_err(DaemonError::PtyWrite)?;
                continue;
            }

            if frame.msg_type() == MessageType::WINDOW_SIZE {
                let size: WindowSize = frame.decode_body()?;
                let master = master_write.lock().expect("pty writer poisoned");
                set_window_size(master.as_fd(), size).map_err(DaemonError::PtyWindowSize)?;
                continue;
            }

            if frame.msg_type() == MessageType::DETACH {
                let _: DetachRequest = frame.decode_body()?;
                break;
            }

            return Err(DaemonError::UnexpectedMessage(frame.msg_type().get()));
        }

        let mut state = state.lock().expect("daemon state poisoned");
        if state
            .broker
            .as_ref()
            .is_some_and(|broker| broker.token == token)
        {
            state.detach_current(SystemTime::now());
        }

        Ok(())
    }

    fn write_broker_pty_data(
        writer: Arc<Mutex<UnixStream>>,
        bytes: &[u8],
    ) -> Result<(), DaemonError> {
        let mut stream = writer.lock().expect("broker writer poisoned");
        let mut writer = FramedWriter::new(&mut *stream);
        writer.write_body(
            MessageType::PTY_DATA,
            0,
            &PtyData {
                bytes: bytes.to_vec(),
            },
        )?;
        writer.flush()?;
        Ok(())
    }

    fn write_broker_exit_status(
        writer: Arc<Mutex<UnixStream>>,
        status: &ExitStatus,
    ) -> Result<(), DaemonError> {
        let mut stream = writer.lock().expect("broker writer poisoned");
        let mut writer = FramedWriter::new(&mut *stream);
        writer.write_body(MessageType::EXIT_STATUS, 0, status)?;
        writer.flush()?;
        Ok(())
    }

    fn write_client_should_exit(writer: Arc<Mutex<UnixStream>>) -> Result<(), DaemonError> {
        let mut stream = writer.lock().expect("broker writer poisoned");
        let mut writer = FramedWriter::new(&mut *stream);
        writer.write_body(MessageType::CLIENT_SHOULD_EXIT, 0, &())?;
        writer.flush()?;
        Ok(())
    }

    fn shell_command_name(shell: &str) -> String {
        PathBuf::from(shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("(unknown)")
            .to_string()
    }

    fn unix_time_millis(time: SystemTime) -> UnixTimeMillis {
        UnixTimeMillis(
            time.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        )
    }

    #[derive(Debug)]
    struct AttachedBroker {
        token: u64,
        writer: Arc<Mutex<UnixStream>>,
    }

    #[derive(Debug)]
    struct DaemonState {
        session_id: SessionId,
        init_time: SystemTime,
        last_detach_time: Option<SystemTime>,
        current_command: String,
        replay: ReplayBuffer,
        broker: Option<AttachedBroker>,
        attach_token: u64,
        shutdown: bool,
    }

    impl DaemonState {
        fn new(session_id: SessionId, init_time: SystemTime, current_command: String) -> Self {
            Self {
                session_id,
                init_time,
                last_detach_time: None,
                current_command,
                replay: ReplayBuffer::default_capacity(),
                broker: None,
                attach_token: 0,
                shutdown: false,
            }
        }

        fn record(&self) -> SessionRecord {
            SessionRecord {
                session_id: self.session_id.to_string(),
                init_time: unix_time_millis(self.init_time),
                last_detach_time: self.last_detach_time.map(unix_time_millis),
                current_command: self.current_command.clone(),
                attached: self.broker.is_some(),
            }
        }

        fn next_attach_token(&mut self) -> u64 {
            self.attach_token = self.attach_token.wrapping_add(1);
            self.attach_token
        }

        fn detach_current(&mut self, detach_time: SystemTime) -> Option<Arc<Mutex<UnixStream>>> {
            self.last_detach_time = Some(detach_time);
            self.broker.take().map(|broker| broker.writer)
        }
    }

    #[derive(Debug)]
    pub enum DaemonError {
        SessionId(SessionIdError),
        SocketPath(SocketPathError),
        SocketAlreadyExists(PathBuf),
        Fork(nix::errno::Errno),
        SetSid(nix::errno::Errno),
        Chdir(nix::errno::Errno),
        OpenDevNull(io::Error),
        RedirectStdio(nix::errno::Errno),
        Bind(io::Error),
        Listen(io::Error),
        Accept(io::Error),
        StreamClone(io::Error),
        Pty(PtyError),
        DupPty(nix::errno::Errno),
        PtyWindowSize(PtyError),
        PtyWrite(io::Error),
        Protocol(ProtocolError),
        NoRequest,
        UnexpectedMessage(u8),
    }

    impl fmt::Display for DaemonError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::SessionId(err) => write!(f, "{err}"),
                Self::SocketPath(err) => write!(f, "{err}"),
                Self::SocketAlreadyExists(path) => {
                    write!(f, "daemon socket already exists: {}", path.display())
                }
                Self::Fork(err) => write!(f, "failed to fork daemon: {err}"),
                Self::SetSid(err) => write!(f, "failed to create daemon session: {err}"),
                Self::Chdir(err) => write!(f, "failed to chdir daemon to /: {err}"),
                Self::OpenDevNull(err) => write!(f, "failed to open /dev/null: {err}"),
                Self::RedirectStdio(err) => write!(f, "failed to redirect daemon stdio: {err}"),
                Self::Bind(err) => write!(f, "failed to bind daemon socket: {err}"),
                Self::Listen(err) => write!(f, "failed to configure daemon listener: {err}"),
                Self::Accept(err) => write!(f, "failed to accept daemon connection: {err}"),
                Self::StreamClone(err) => write!(f, "failed to clone daemon socket stream: {err}"),
                Self::Pty(err) => write!(f, "{err}"),
                Self::DupPty(err) => write!(f, "failed to duplicate PTY master: {err}"),
                Self::PtyWindowSize(err) => write!(f, "{err}"),
                Self::PtyWrite(err) => write!(f, "failed to write to PTY: {err}"),
                Self::Protocol(err) => write!(f, "daemon protocol error: {err}"),
                Self::NoRequest => write!(f, "daemon connection closed before sending a request"),
                Self::UnexpectedMessage(msg_type) => {
                    write!(f, "daemon received unexpected message type {msg_type}")
                }
            }
        }
    }

    impl std::error::Error for DaemonError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                Self::SessionId(err) => Some(err),
                Self::SocketPath(err) => Some(err),
                Self::Bind(err)
                | Self::Listen(err)
                | Self::Accept(err)
                | Self::StreamClone(err)
                | Self::OpenDevNull(err)
                | Self::PtyWrite(err) => Some(err),
                Self::Pty(err) | Self::PtyWindowSize(err) => Some(err),
                Self::Protocol(err) => Some(err),
                _ => None,
            }
        }
    }

    impl From<SessionIdError> for DaemonError {
        fn from(value: SessionIdError) -> Self {
            Self::SessionId(value)
        }
    }

    impl From<SocketPathError> for DaemonError {
        fn from(value: SocketPathError) -> Self {
            Self::SocketPath(value)
        }
    }

    impl From<PtyError> for DaemonError {
        fn from(value: PtyError) -> Self {
            Self::Pty(value)
        }
    }

    impl From<ProtocolError> for DaemonError {
        fn from(value: ProtocolError) -> Self {
            Self::Protocol(value)
        }
    }
}

#[cfg(unix)]
pub use runtime::{DaemonError, run_daemon};

#[cfg(not(unix))]
#[derive(Debug)]
pub enum DaemonError {
    UnsupportedPlatform(&'static str),
}

#[cfg(not(unix))]
impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
        }
    }
}

#[cfg(not(unix))]
impl std::error::Error for DaemonError {}

#[cfg(not(unix))]
pub fn run_daemon(_session_id: crate::session::SessionId) -> Result<(), DaemonError> {
    Err(DaemonError::UnsupportedPlatform(
        "ssh-obi-server daemon mode requires Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_buffer_keeps_recent_bytes() {
        let mut buffer = ReplayBuffer::new(5).unwrap();
        buffer.append(b"abc");
        buffer.append(b"def");

        assert_eq!(buffer.snapshot(), b"bcdef");
    }

    #[test]
    fn append_larger_than_capacity_keeps_tail() {
        let mut buffer = ReplayBuffer::new(4).unwrap();
        buffer.append(b"abcdefghijkl");

        assert_eq!(buffer.snapshot(), b"ijkl");
    }

    #[test]
    fn zero_capacity_is_rejected() {
        assert_eq!(
            ReplayBuffer::new(0).unwrap_err(),
            ReplayBufferError::ZeroCapacity
        );
    }
}
