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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
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
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            Self::GetAttr(err) => write!(f, "failed to read terminal attributes: {err}"),
            #[cfg(unix)]
            Self::SetAttr(err) => write!(f, "failed to set terminal attributes: {err}"),
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
