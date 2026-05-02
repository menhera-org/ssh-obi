use std::fmt;

#[cfg(all(target_os = "linux", unix))]
pub fn foreground_command_for_pty<Fd>(fd: Fd) -> Result<Option<String>, ForegroundError>
where
    Fd: std::os::fd::AsFd,
{
    use std::os::fd::AsRawFd;

    let mut pgrp: nix::libc::pid_t = 0;
    let result = unsafe {
        nix::libc::ioctl(
            fd.as_fd().as_raw_fd(),
            nix::libc::TIOCGPGRP,
            &mut pgrp as *mut nix::libc::pid_t,
        )
    };

    if result < 0 {
        let err = std::io::Error::last_os_error();
        if matches!(
            err.raw_os_error(),
            Some(code) if code == nix::libc::ENOTTY || code == nix::libc::EINVAL
        ) {
            return Ok(None);
        }
        return Err(ForegroundError::Ioctl(err));
    }

    if pgrp <= 0 {
        return Ok(None);
    }

    linux::command_for_process_group(pgrp as i32)
}

#[cfg(not(all(target_os = "linux", unix)))]
pub fn foreground_command_for_pty<Fd>(_fd: Fd) -> Result<Option<String>, ForegroundError> {
    Ok(None)
}

#[derive(Debug)]
pub enum ForegroundError {
    Ioctl(std::io::Error),
    ReadProc(std::io::Error),
}

impl fmt::Display for ForegroundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ioctl(err) => write!(f, "failed to query PTY foreground process group: {err}"),
            Self::ReadProc(err) => write!(f, "failed to read process table: {err}"),
        }
    }
}

impl std::error::Error for ForegroundError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ioctl(err) | Self::ReadProc(err) => Some(err),
        }
    }
}

#[cfg(all(target_os = "linux", unix))]
mod linux {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    use super::ForegroundError;

    pub(super) fn command_for_process_group(pgrp: i32) -> Result<Option<String>, ForegroundError> {
        command_for_process_group_in(pgrp, Path::new("/proc"))
    }

    fn command_for_process_group_in(
        pgrp: i32,
        proc_root: &Path,
    ) -> Result<Option<String>, ForegroundError> {
        let mut fallback = None;

        for entry in fs::read_dir(proc_root).map_err(ForegroundError::ReadProc)? {
            let entry = entry.map_err(ForegroundError::ReadProc)?;
            let file_name = entry.file_name();
            let Some(pid) = file_name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
                continue;
            };

            let stat_path = entry.path().join("stat");
            let stat = match fs::read_to_string(&stat_path) {
                Ok(stat) => stat,
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue;
                }
                Err(err) => return Err(ForegroundError::ReadProc(err)),
            };

            let Some(stat) = parse_stat(&stat) else {
                continue;
            };
            if stat.pgrp != pgrp {
                continue;
            }

            let command = read_command(entry.path(), &stat.comm)?;
            if pid == pgrp {
                return Ok(Some(command));
            }
            fallback.get_or_insert(command);
        }

        Ok(fallback)
    }

    fn parse_stat(stat: &str) -> Option<ProcStat> {
        let left = stat.find('(')?;
        let right = stat.rfind(')')?;
        if right <= left {
            return None;
        }

        let comm = stat[left + 1..right].to_string();
        let fields = stat[right + 1..].split_whitespace().collect::<Vec<_>>();
        let pgrp = fields.get(2)?.parse().ok()?;

        Some(ProcStat { comm, pgrp })
    }

    fn read_command(proc_entry: PathBuf, comm: &str) -> Result<String, ForegroundError> {
        let cmdline_path = proc_entry.join("cmdline");
        let cmdline = match fs::read(&cmdline_path) {
            Ok(cmdline) => cmdline,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(comm.to_string());
            }
            Err(err) => return Err(ForegroundError::ReadProc(err)),
        };

        let args = cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| {
                OsString::from_vec(arg.to_vec())
                    .to_string_lossy()
                    .into_owned()
            })
            .map(|arg| arg.replace(['\r', '\n', '\t'], " "))
            .collect::<Vec<_>>();

        if args.is_empty() {
            return Ok(comm.to_string());
        }

        Ok(args.join(" "))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ProcStat {
        comm: String,
        pgrp: i32,
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;

        #[test]
        fn parse_stat_handles_spaces_in_comm() {
            let stat = parse_stat("123 (vim notes.md) S 1 456 456 0 -1 4194304").unwrap();
            assert_eq!(
                stat,
                ProcStat {
                    comm: "vim notes.md".to_string(),
                    pgrp: 456,
                }
            );
        }

        #[test]
        fn command_for_process_group_prefers_group_leader_cmdline() {
            let dir = temp_proc_dir("foreground-command");
            let leader = dir.join("456");
            let peer = dir.join("789");
            fs::create_dir_all(&leader).unwrap();
            fs::create_dir_all(&peer).unwrap();
            fs::write(leader.join("stat"), "456 (sh) S 1 456 456 0").unwrap();
            fs::write(peer.join("stat"), "789 (cat) S 1 456 456 0").unwrap();
            fs::write(leader.join("cmdline"), b"vim\0notes.md\0").unwrap();
            fs::write(peer.join("cmdline"), b"cat\0").unwrap();

            let command = command_for_process_group_in(456, &dir).unwrap();
            assert_eq!(command, Some("vim notes.md".to_string()));

            let _ = fs::remove_dir_all(dir);
        }

        #[test]
        fn command_for_process_group_falls_back_to_comm() {
            let dir = temp_proc_dir("foreground-comm");
            let proc = dir.join("789");
            fs::create_dir_all(&proc).unwrap();
            fs::write(proc.join("stat"), "789 (cat) S 1 123 123 0").unwrap();
            fs::File::create(proc.join("cmdline"))
                .unwrap()
                .flush()
                .unwrap();

            let command = command_for_process_group_in(123, &dir).unwrap();
            assert_eq!(command, Some("cat".to_string()));

            let _ = fs::remove_dir_all(dir);
        }

        fn temp_proc_dir(name: &str) -> PathBuf {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            dir
        }
    }
}
