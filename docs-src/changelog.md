# Changelog

## v0.1.3

Released for the `0.1.x` protocol line.

### Added

- New sessions print the remote host MOTD before the shell starts, using
  `/run/motd.dynamic`, `/etc/motd`, and readable non-empty files under
  `/etc/motd.d/`. A user `~/.hushlogin` suppresses this MOTD output.

### Changed

- Automatic reconnect retries now use a small capped exponential backoff:
  125ms, 250ms, 500ms, 1s, then 2s, with a maximum of 10 attempts.

### Notes

- The protocol baseline remains `0.1`.

## v0.1.2

Released for the `0.1.x` protocol line.

### Added

- On Linux systems booted with systemd and a user bus, newly spawned PTY
  children are moved into a transient user scope before exec. This follows the
  same systemd `StartTransientUnit` shape used by tmux >= 3.2 for cgroup
  detaching, and falls back cleanly when systemd is unavailable.
- During automatic reconnect, if the target session still reports
  `SessionBusy`, the client asks the stale attached client to detach and then
  retries the reconnect.
- The Unix bootstrap reports an explicit OpenBSD message when no compatible
  server is already installed. OpenBSD has no prebuilt release tarballs; install
  a Rust toolchain and run `cargo install --features server-bin ssh-obi`.
- `ssh-obi-server --list` lists currently alive sessions for the remote user on
  the server host, including busy sessions and a marker for the current session
  when run inside an `ssh-obi` session.

### Fixed

- OpenBSD install-only bootstrap runs now complete successfully when a
  compatible `ssh-obi-server` already exists at
  `~/.cargo/bin/ssh-obi-server` or on `PATH`.

### Notes

- The protocol baseline remains `0.1`.
- Non-systemd Linux systems and non-Linux Unix systems do not require any
  systemd tooling.

## v0.1.1

Released for the `0.1.x` protocol line.

### Added

- The client sends an initial terminal window size before attach and new-session
  requests when the remote server supports `initial-window-size.v1`.
- New sessions create the remote PTY with the local terminal size when that size
  is available.
- Reattaches apply the local terminal size before replaying buffered output.
- The Unix bootstrap detects compatible `ssh-obi-server` binaries in three
  places before trying a tarball install:
  `~/.ssh-obi/bin/ssh-obi-server`, `~/.cargo/bin/ssh-obi-server`, and
  `ssh-obi-server` found on `PATH`.

### Notes

- The protocol baseline remains `0.1`; `initial-window-size.v1` is negotiated as
  a capability.
- Platforms without a published release tarball can still be used as remote
  servers when a compatible `ssh-obi-server` is installed by Cargo or a distro
  package.

## v0.1.0

Initial public release.

### Added

- Local `ssh-obi` client and Unix remote `ssh-obi-server`.
- Per-user long-lived remote sessions.
- Attach, new-session, list, and detach commands.
- Reconnect to a known session after ambiguous disconnects.
- Bounded replay of recent output on reattach.
- Shell exit status forwarding.
- Unix bootstrap install flow and Windows client-only bootstrap flow.
