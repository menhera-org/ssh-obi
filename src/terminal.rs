use std::fmt;

#[cfg(unix)]
#[derive(Debug)]
pub struct RawModeGuard<Fd>
where
    Fd: std::os::fd::AsFd,
{
    fd: Fd,
    original: nix::sys::termios::Termios,
}

#[cfg(windows)]
#[derive(Debug)]
pub struct RawModeGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
    original: u32,
}

#[cfg(all(not(unix), not(windows)))]
#[derive(Debug)]
pub struct RawModeGuard;

#[cfg(unix)]
impl<Fd> RawModeGuard<Fd>
where
    Fd: std::os::fd::AsFd,
{
    pub fn enable(fd: Fd) -> Result<Option<Self>, TerminalError> {
        use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};

        let original = match tcgetattr(fd.as_fd()) {
            Ok(original) => original,
            Err(err) if err == nix::errno::Errno::ENOTTY || err == nix::errno::Errno::EINVAL => {
                return Ok(None);
            }
            Err(err) => return Err(TerminalError::GetAttr(err)),
        };

        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr(fd.as_fd(), SetArg::TCSANOW, &raw).map_err(TerminalError::SetAttr)?;

        Ok(Some(Self { fd, original }))
    }
}

#[cfg(unix)]
impl<Fd> Drop for RawModeGuard<Fd>
where
    Fd: std::os::fd::AsFd,
{
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcsetattr(
            self.fd.as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original,
        );
    }
}

#[cfg(windows)]
impl RawModeGuard {
    pub fn enable() -> Result<Option<Self>, TerminalError> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode,
            GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };

        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Ok(None);
        }

        let mut original = 0;
        if unsafe { GetConsoleMode(handle, &mut original) } == 0 {
            return Ok(None);
        }

        let raw = original & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
        if unsafe { SetConsoleMode(handle, raw) } == 0 {
            return Err(TerminalError::SetConsoleMode(
                std::io::Error::last_os_error(),
            ));
        }

        Ok(Some(Self { handle, original }))
    }
}

#[cfg(windows)]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = unsafe {
            windows_sys::Win32::System::Console::SetConsoleMode(self.handle, self.original)
        };
    }
}

#[cfg(all(not(unix), not(windows)))]
impl RawModeGuard {
    pub fn enable() -> Result<Option<Self>, TerminalError> {
        Ok(None)
    }
}

#[derive(Debug)]
pub enum TerminalError {
    #[cfg(unix)]
    GetAttr(nix::errno::Errno),
    #[cfg(unix)]
    SetAttr(nix::errno::Errno),
    #[cfg(windows)]
    SetConsoleMode(std::io::Error),
    #[cfg(all(not(unix), not(windows)))]
    UnsupportedPlatform,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            Self::GetAttr(err) => write!(f, "failed to read terminal attributes: {err}"),
            #[cfg(unix)]
            Self::SetAttr(err) => write!(f, "failed to set terminal attributes: {err}"),
            #[cfg(windows)]
            Self::SetConsoleMode(err) => write!(f, "failed to set console mode: {err}"),
            #[cfg(all(not(unix), not(windows)))]
            Self::UnsupportedPlatform => write!(f, "raw terminal mode is unsupported"),
        }
    }
}

impl std::error::Error for TerminalError {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    use nix::sys::termios::{InputFlags, LocalFlags, OutputFlags, SetArg, tcgetattr, tcsetattr};

    use crate::pty::open_pty;

    #[test]
    fn raw_mode_is_restored_on_drop() {
        let pair = open_pty(None).unwrap();
        let mut original = tcgetattr(pair.slave.as_fd()).unwrap();
        original.local_flags.insert(LocalFlags::ECHO);
        original.local_flags.insert(LocalFlags::ICANON);
        original.input_flags.insert(InputFlags::ICRNL);
        original.output_flags.insert(OutputFlags::OPOST);
        tcsetattr(pair.slave.as_fd(), SetArg::TCSANOW, &original).unwrap();

        {
            let _guard = RawModeGuard::enable(pair.slave.as_fd()).unwrap().unwrap();
            let raw = tcgetattr(pair.slave.as_fd()).unwrap();
            assert!(!raw.local_flags.contains(LocalFlags::ECHO));
            assert!(!raw.local_flags.contains(LocalFlags::ICANON));
            assert!(!raw.input_flags.contains(InputFlags::ICRNL));
            assert!(!raw.output_flags.contains(OutputFlags::OPOST));
        }

        let restored = tcgetattr(pair.slave.as_fd()).unwrap();
        assert_eq!(restored.local_flags, original.local_flags);
        assert_eq!(restored.input_flags, original.input_flags);
        assert_eq!(restored.output_flags, original.output_flags);
    }
}
