use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const START_TRANSIENT_UNIT_SIGNATURE: &str = "ssa(sv)a(sa(sv))";
const SCOPE_START_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) fn move_pid_to_new_scope(pid: nix::libc::pid_t) -> Result<(), SystemdScopeError> {
    let Some(runtime_dir) = user_systemd_runtime_dir() else {
        return Err(SystemdScopeError::Unavailable);
    };

    let unit = transient_scope_unit_name(pid);
    let deadline = Instant::now() + SCOPE_START_TIMEOUT;
    let command = transient_scope_command(pid, &unit, &runtime_dir);
    wait_for_command_until(command, deadline)?;
    wait_for_scope_membership(pid, &unit, deadline)
}

#[cfg(test)]
fn wait_for_command(command: Command, timeout: Duration) -> Result<(), SystemdScopeError> {
    wait_for_command_until(command, Instant::now() + timeout)
}

fn wait_for_command_until(
    mut command: Command,
    deadline: Instant,
) -> Result<(), SystemdScopeError> {
    let mut child = command.spawn().map_err(SystemdScopeError::Spawn)?;

    loop {
        match child.try_wait().map_err(SystemdScopeError::Wait)? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(SystemdScopeError::Failed(status.code())),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SystemdScopeError::Timeout);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn wait_for_scope_membership(
    pid: nix::libc::pid_t,
    unit: &str,
    deadline: Instant,
) -> Result<(), SystemdScopeError> {
    let cgroup_path = format!("/proc/{pid}/cgroup");

    loop {
        let cgroup =
            std::fs::read_to_string(&cgroup_path).map_err(SystemdScopeError::ReadCgroup)?;
        if cgroup.contains(unit) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(SystemdScopeError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn transient_scope_command(pid: nix::libc::pid_t, unit: &str, runtime_dir: &Path) -> Command {
    let description = format!("ssh-obi PTY child {pid} launched by process {}", unsafe {
        nix::libc::getppid()
    });

    let mut command = Command::new("busctl");
    command
        .arg("--user")
        .arg("call")
        .arg("org.freedesktop.systemd1")
        .arg("/org/freedesktop/systemd1")
        .arg("org.freedesktop.systemd1.Manager")
        .arg("StartTransientUnit")
        .arg(START_TRANSIENT_UNIT_SIGNATURE)
        .arg(unit)
        .arg("fail")
        .arg("5")
        .arg("Description")
        .arg("s")
        .arg(description)
        .arg("SendSIGHUP")
        .arg("b")
        .arg("true")
        .arg("Slice")
        .arg("s")
        .arg("app-ssh_obi.slice")
        .arg("PIDs")
        .arg("au")
        .arg("1")
        .arg(pid.to_string())
        .arg("CollectMode")
        .arg("s")
        .arg("inactive-or-failed")
        .arg("0")
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn user_systemd_runtime_dir() -> Option<PathBuf> {
    if !systemd_booted_at(Path::new("/run/systemd/system")) {
        return None;
    }

    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && runtime_dir.join("bus").exists()
    {
        return Some(runtime_dir);
    }

    let runtime_dir = PathBuf::from(format!(
        "/run/user/{}",
        nix::unistd::Uid::current().as_raw()
    ));
    if runtime_dir.join("bus").exists() {
        Some(runtime_dir)
    } else {
        None
    }
}

fn systemd_booted_at(path: &Path) -> bool {
    path.is_dir()
}

fn transient_scope_unit_name(pid: nix::libc::pid_t) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("ssh-obi-spawn-{pid}-{now}.scope")
}

#[derive(Debug)]
pub(crate) enum SystemdScopeError {
    Unavailable,
    Spawn(std::io::Error),
    Wait(std::io::Error),
    ReadCgroup(std::io::Error),
    Failed(Option<i32>),
    Timeout,
}

impl fmt::Display for SystemdScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "systemd user bus is unavailable"),
            Self::Spawn(err) => write!(f, "failed to launch busctl: {err}"),
            Self::Wait(err) => write!(f, "failed to wait for busctl: {err}"),
            Self::ReadCgroup(err) => write!(f, "failed to read process cgroup: {err}"),
            Self::Failed(code) => {
                write!(f, "systemd transient scope creation failed")?;
                if let Some(code) = code {
                    write!(f, " with status {code}")?;
                }
                Ok(())
            }
            Self::Timeout => write!(f, "timed out creating systemd transient scope"),
        }
    }
}

impl std::error::Error for SystemdScopeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(err) | Self::Wait(err) | Self::ReadCgroup(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_scope_command_uses_start_transient_unit_with_pid() {
        let command = transient_scope_command(
            1234,
            "ssh-obi-spawn-1234-test.scope",
            Path::new("/run/user/1000"),
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(command.get_program(), "busctl");
        assert_eq!(args[0], "--user");
        assert_eq!(args[1], "call");
        assert!(args.iter().any(|arg| arg == "StartTransientUnit"));
        assert!(args.iter().any(|arg| arg == START_TRANSIENT_UNIT_SIGNATURE));
        assert!(
            args.iter()
                .any(|arg| arg == "ssh-obi-spawn-1234-test.scope")
        );
        assert!(args.windows(3).any(|args| args == ["PIDs", "au", "1"]));
        assert!(args.iter().any(|arg| arg == "1234"));
        assert!(
            args.windows(3)
                .any(|args| args == ["Slice", "s", "app-ssh_obi.slice"])
        );
        assert!(
            args.windows(3)
                .any(|args| args == ["CollectMode", "s", "inactive-or-failed"])
        );
    }

    #[test]
    fn systemd_booted_detection_requires_runtime_dir() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("ssh-obi-systemd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);

        assert!(!systemd_booted_at(&path));
        std::fs::create_dir_all(&path).unwrap();
        assert!(systemd_booted_at(&path));

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn transient_scope_unit_name_is_scope_unit() {
        let name = transient_scope_unit_name(1234);
        assert!(name.starts_with("ssh-obi-spawn-1234-"));
        assert!(name.ends_with(".scope"));
    }

    #[test]
    fn command_wait_times_out() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let err = wait_for_command(command, Duration::from_millis(10)).unwrap_err();
        assert!(matches!(err, SystemdScopeError::Timeout));
    }

    #[test]
    fn command_wait_reports_failed_status() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("exit 7")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let err = wait_for_command(command, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, SystemdScopeError::Failed(Some(7))));
    }
}
