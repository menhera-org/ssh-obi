use std::fmt;
use std::os::fd::OwnedFd;

use crate::protocol::WindowSize;

#[derive(Debug)]
pub struct PtyPair {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

#[cfg(all(unix, not(target_os = "aix")))]
#[derive(Debug)]
pub struct PtyChild {
    pub master: OwnedFd,
    pub child: nix::unistd::Pid,
}

#[cfg(all(unix, not(target_os = "aix")))]
pub fn open_pty(size: Option<WindowSize>) -> Result<PtyPair, PtyError> {
    let winsize = size.map(to_nix_winsize);
    let result = nix::pty::openpty(winsize.as_ref(), None).map_err(PtyError::Open)?;

    Ok(PtyPair {
        master: result.master,
        slave: result.slave,
    })
}

#[cfg(all(unix, not(target_os = "aix")))]
pub fn spawn_pty_command(
    program: &str,
    args: &[&str],
    env_overrides: &[(&str, &str)],
    size: Option<WindowSize>,
) -> Result<PtyChild, PtyError> {
    use std::ffi::CString;

    use nix::pty::ForkptyResult;
    use nix::unistd::execvp;

    let program = CString::new(program).map_err(|_| PtyError::NulByte("program"))?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.clone());
    for arg in args {
        argv.push(CString::new(*arg).map_err(|_| PtyError::NulByte("argument"))?);
    }

    let mut env = Vec::with_capacity(env_overrides.len());
    for (name, value) in env_overrides {
        if name.as_bytes().contains(&b'=') {
            return Err(PtyError::InvalidEnvironmentName((*name).to_string()));
        }

        env.push((
            CString::new(*name).map_err(|_| PtyError::NulByte("environment"))?,
            CString::new(*value).map_err(|_| PtyError::NulByte("environment"))?,
        ));
    }

    let winsize = size.map(to_nix_winsize);
    // SAFETY: forkpty is called before this function starts any threads. The child branch only
    // applies prebuilt environment overrides, invokes execvp with prebuilt C strings, then _exit
    // if setup or exec fails.
    match unsafe { nix::pty::forkpty(winsize.as_ref(), None) }.map_err(PtyError::Fork)? {
        ForkptyResult::Parent { child, master } => Ok(PtyChild { master, child }),
        ForkptyResult::Child => {
            for (name, value) in &env {
                if unsafe { nix::libc::setenv(name.as_ptr(), value.as_ptr(), 1) } != 0 {
                    // SAFETY: We are in the post-fork child and cannot recover from environment
                    // setup failure before exec.
                    unsafe { nix::libc::_exit(127) };
                }
            }
            let _ = execvp(&program, &argv);
            // SAFETY: We are in the post-fork child and exec failed; _exit avoids running parent
            // Rust destructors in the child process.
            unsafe { nix::libc::_exit(127) };
        }
    }
}

#[cfg(all(unix, not(target_os = "aix")))]
pub fn set_window_size<Fd>(fd: Fd, size: WindowSize) -> Result<(), PtyError>
where
    Fd: std::os::fd::AsFd,
{
    use std::os::fd::AsRawFd;

    let winsize = to_libc_winsize(size);
    let result = unsafe {
        nix::libc::ioctl(
            fd.as_fd().as_raw_fd(),
            nix::libc::TIOCSWINSZ,
            &winsize as *const nix::libc::winsize,
        )
    };

    nix::errno::Errno::result(result)
        .map(drop)
        .map_err(PtyError::SetWindowSize)
}

#[cfg(not(all(unix, not(target_os = "aix"))))]
pub fn open_pty(_size: Option<WindowSize>) -> Result<PtyPair, PtyError> {
    Err(PtyError::UnsupportedPlatform(
        "PTY allocation requires a Unix platform supported by nix::pty::openpty",
    ))
}

#[cfg(all(unix, not(target_os = "aix")))]
fn to_nix_winsize(size: WindowSize) -> nix::pty::Winsize {
    nix::pty::Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

#[cfg(all(unix, not(target_os = "aix")))]
fn to_libc_winsize(size: WindowSize) -> nix::libc::winsize {
    nix::libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

#[derive(Debug)]
pub enum PtyError {
    Open(nix::errno::Errno),
    Fork(nix::errno::Errno),
    SetWindowSize(nix::errno::Errno),
    NulByte(&'static str),
    InvalidEnvironmentName(String),
    UnsupportedPlatform(&'static str),
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(err) => write!(f, "failed to open PTY: {err}"),
            Self::Fork(err) => write!(f, "failed to fork PTY child: {err}"),
            Self::SetWindowSize(err) => write!(f, "failed to set PTY window size: {err}"),
            Self::NulByte(field) => write!(f, "PTY {field} contains a NUL byte"),
            Self::InvalidEnvironmentName(name) => {
                write!(f, "environment variable name contains '=': {name}")
            }
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for PtyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "aix")))]
    #[test]
    fn window_size_maps_to_nix_winsize() {
        let winsize = to_nix_winsize(WindowSize {
            rows: 24,
            cols: 80,
            pixel_width: 640,
            pixel_height: 480,
        });

        assert_eq!(winsize.ws_row, 24);
        assert_eq!(winsize.ws_col, 80);
        assert_eq!(winsize.ws_xpixel, 640);
        assert_eq!(winsize.ws_ypixel, 480);
    }

    #[cfg(all(unix, not(target_os = "aix")))]
    #[test]
    fn open_pty_returns_two_file_descriptors() {
        let pair = open_pty(Some(WindowSize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }))
        .unwrap();

        drop(pair);
    }

    #[cfg(all(unix, not(target_os = "aix")))]
    #[test]
    fn set_window_size_applies_to_pty() {
        use std::os::fd::{AsFd, AsRawFd};

        let pair = open_pty(None).unwrap();
        set_window_size(
            pair.master.as_fd(),
            WindowSize {
                rows: 33,
                cols: 101,
                pixel_width: 640,
                pixel_height: 480,
            },
        )
        .unwrap();

        let mut winsize = std::mem::MaybeUninit::<nix::libc::winsize>::uninit();
        let result = unsafe {
            nix::libc::ioctl(
                pair.master.as_fd().as_raw_fd(),
                nix::libc::TIOCGWINSZ,
                winsize.as_mut_ptr(),
            )
        };
        nix::errno::Errno::result(result).unwrap();
        let winsize = unsafe { winsize.assume_init() };

        assert_eq!(winsize.ws_row, 33);
        assert_eq!(winsize.ws_col, 101);
        assert_eq!(winsize.ws_xpixel, 640);
        assert_eq!(winsize.ws_ypixel, 480);
    }

    #[cfg(all(unix, not(target_os = "aix")))]
    #[test]
    fn spawn_pty_command_execs_child_with_environment() {
        use std::io::Read;
        use std::time::{Duration, Instant};

        use nix::sys::wait::{WaitStatus, waitpid};

        let child = spawn_pty_command(
            "/bin/sh",
            &["-c", "printf '%s' \"$SSH_OBI_TEST_VALUE\""],
            &[("SSH_OBI_TEST_VALUE", "pty-ok")],
            Some(WindowSize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            }),
        )
        .unwrap();
        let mut master = std::fs::File::from(child.master);
        let start = Instant::now();
        let mut output = Vec::new();
        let mut buf = [0u8; 64];

        while start.elapsed() < Duration::from_secs(2) && !output.windows(6).any(|w| w == b"pty-ok")
        {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }

        assert!(
            output.windows(6).any(|w| w == b"pty-ok"),
            "pty output was {:?}",
            String::from_utf8_lossy(&output)
        );

        let status = waitpid(child.child, None).unwrap();
        assert!(matches!(status, WaitStatus::Exited(_, 0)));
    }
}
