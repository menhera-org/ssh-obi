use crate::protocol::CURRENT_PROTOCOL_BASELINE;

pub const SCRIPT: &str = include_str!("../bootstrap.sh");
pub const READY_MARKER: &str = "OBI-SERVER-READY";
pub const INSTALL_REQUIRED_MARKER: &str = "OBI-INSTALL-REQUIRED";
pub const INSTALL_OK_MARKER: &str = "OBI-INSTALL-OK";
pub const DEFAULT_TERM: &str = "xterm-256color";

pub fn remote_shell_command(server_args: &[&str], term: &str) -> String {
    let mut command = format!(
        "sh -c {} sh {} --term {}",
        shell_quote(SCRIPT),
        shell_quote(CURRENT_PROTOCOL_BASELINE),
        shell_quote(term)
    );
    for arg in server_args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
}

pub fn terminal_type_from_env() -> String {
    match std::env::var("TERM") {
        Ok(term) if is_useful_term(&term) => term,
        _ => DEFAULT_TERM.to_string(),
    }
}

fn is_useful_term(term: &str) -> bool {
    !term.is_empty() && term != "dumb"
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b'_' | b'-'))
    {
        return value.to_string();
    }

    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_script_contains_sync_markers() {
        assert!(SCRIPT.contains(READY_MARKER));
        assert!(SCRIPT.contains(INSTALL_REQUIRED_MARKER));
        assert!(SCRIPT.contains(INSTALL_OK_MARKER));
    }

    #[test]
    fn embedded_script_checks_fixed_cargo_server_path() {
        assert!(SCRIPT.contains("${HOME}/.cargo/bin/ssh-obi-server"));
    }

    #[test]
    fn embedded_script_checks_path_server() {
        assert!(SCRIPT.contains("command -v ssh-obi-server"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_execs_cargo_server_without_path_lookup() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "ssh-obi-bootstrap-cargo-test-{}-{unique}",
            std::process::id()
        ));
        let cargo_bin = home.join(".cargo").join("bin");
        fs::create_dir_all(&cargo_bin).unwrap();
        let server = cargo_bin.join("ssh-obi-server");
        fs::write(
            &server,
            "#!/bin/sh\nif [ \"$1\" = \"--protocol-check\" ]; then exit 0; fi\nprintf 'FAKE-SERVER %s\\n' \"$*\"\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&server).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&server, perms).unwrap();

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("0.1")
            .arg("--term")
            .arg("vt100")
            .arg("--probe")
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();

        let _ = fs::remove_dir_all(&home);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-SERVER-READY\n"));
        assert!(stdout.contains("FAKE-SERVER --probe\n"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_execs_server_found_on_path() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ssh-obi-bootstrap-path-test-{}-{unique}",
            std::process::id()
        ));
        let home = root.join("home");
        let bin = root.join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let server = bin.join("ssh-obi-server");
        fs::write(
            &server,
            "#!/bin/sh\nif [ \"$1\" = \"--protocol-check\" ]; then exit 0; fi\nprintf 'PATH-SERVER %s\\n' \"$*\"\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&server).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&server, perms).unwrap();

        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("0.1")
            .arg("--term")
            .arg("vt100")
            .arg("--probe")
            .env("HOME", &home)
            .env("PATH", path)
            .output()
            .unwrap();

        let _ = fs::remove_dir_all(&root);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-SERVER-READY\n"));
        assert!(stdout.contains("PATH-SERVER --probe\n"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_warns_openbsd_during_install_only() {
        let root = openbsd_uname_test_root("install-only");
        let home = root.join("home");
        let bin = root.join("bin");
        make_fake_openbsd_uname(&bin);
        std::fs::create_dir_all(&home).unwrap();

        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("--install")
            .env("HOME", &home)
            .env("PATH", path)
            .output()
            .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-ERROR OpenBSD has no prebuilt ssh-obi binaries."));
        assert!(stdout.contains("cargo install --features server-bin ssh-obi"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_openbsd_install_only_accepts_cargo_server() {
        let root = openbsd_uname_test_root("install-only-cargo");
        let home = root.join("home");
        let bin = root.join("bin");
        let cargo_bin = home.join(".cargo").join("bin");
        make_fake_openbsd_uname(&bin);
        make_fake_protocol_server(&cargo_bin.join("ssh-obi-server"), "CARGO-SERVER");

        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("--install")
            .env("HOME", &home)
            .env("PATH", path)
            .output()
            .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-INSTALL-COMPLETE\n"));
        assert!(!stdout.contains("OBI-ERROR OpenBSD"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_openbsd_install_only_accepts_path_server() {
        let root = openbsd_uname_test_root("install-only-path");
        let home = root.join("home");
        let bin = root.join("bin");
        make_fake_openbsd_uname(&bin);
        make_fake_protocol_server(&bin.join("ssh-obi-server"), "PATH-SERVER");
        std::fs::create_dir_all(&home).unwrap();

        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("--install")
            .env("HOME", &home)
            .env("PATH", path)
            .output()
            .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-INSTALL-COMPLETE\n"));
        assert!(!stdout.contains("OBI-ERROR OpenBSD"));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_script_warns_openbsd_after_auto_install_confirmation() {
        let root = openbsd_uname_test_root("auto-install");
        let home = root.join("home");
        let bin = root.join("bin");
        make_fake_openbsd_uname(&bin);
        std::fs::create_dir_all(&home).unwrap();

        let path = format!("{}:/usr/bin:/bin", bin.display());
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("sh")
            .arg("0.1")
            .arg("--probe")
            .env("HOME", &home)
            .env("PATH", path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;

            let stdin = child.stdin.as_mut().unwrap();
            writeln!(stdin, "OBI-INSTALL-OK").unwrap();
        }
        let output = child.wait_with_output().unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OBI-INSTALL-REQUIRED\n"));
        assert!(stdout.contains("OBI-ERROR OpenBSD has no prebuilt ssh-obi binaries."));
        assert!(stdout.contains("cargo install --features server-bin ssh-obi"));
    }

    #[test]
    fn remote_command_runs_stdin_script_with_baseline() {
        let command = remote_shell_command(&[], "xterm-256color");
        assert!(command.starts_with("sh -c '"));
        assert!(command.ends_with(" sh 0.1 --term xterm-256color"));
    }

    #[test]
    fn remote_command_passes_server_args() {
        assert!(
            remote_shell_command(&["--detach"], "xterm").ends_with(" sh 0.1 --term xterm --detach")
        );
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn terminal_type_uses_default_for_unhelpful_values() {
        assert!(!is_useful_term(""));
        assert!(!is_useful_term("dumb"));
        assert!(is_useful_term("xterm-256color"));
    }

    #[cfg(unix)]
    fn openbsd_uname_test_root(name: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ssh-obi-bootstrap-openbsd-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn make_fake_openbsd_uname(bin: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(bin).unwrap();
        let uname = bin.join("uname");
        std::fs::write(
            &uname,
            "#!/bin/sh\ncase \"$1\" in\n  -s) printf 'OpenBSD\\n' ;;\n  -m) printf 'amd64\\n' ;;\n  *) printf 'OpenBSD\\n' ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&uname).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&uname, perms).unwrap();
    }

    #[cfg(unix)]
    fn make_fake_protocol_server(path: &std::path::Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--protocol-check\" ]; then exit 0; fi\nprintf '{name} %s\\n' \"$*\"\n"
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}
