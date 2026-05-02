use crate::protocol::CURRENT_PROTOCOL_BASELINE;

pub const SCRIPT: &str = include_str!("../bootstrap.sh");
pub const READY_MARKER: &str = "OBI-SERVER-READY";
pub const INSTALL_REQUIRED_MARKER: &str = "OBI-INSTALL-REQUIRED";
pub const INSTALL_OK_MARKER: &str = "OBI-INSTALL-OK";

pub fn remote_shell_command(server_args: &[&str]) -> String {
    let mut command = format!(
        "sh -c {} sh {}",
        shell_quote(SCRIPT),
        shell_quote(CURRENT_PROTOCOL_BASELINE)
    );
    for arg in server_args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
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
    fn remote_command_runs_stdin_script_with_baseline() {
        let command = remote_shell_command(&[]);
        assert!(command.starts_with("sh -c '"));
        assert!(command.ends_with(" sh 0.1"));
    }

    #[test]
    fn remote_command_passes_server_args() {
        assert!(remote_shell_command(&["--detach"]).ends_with(" sh 0.1 --detach"));
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
