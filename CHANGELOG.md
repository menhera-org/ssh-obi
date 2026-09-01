# Changelog

## 0.3.0

The wire-protocol baseline remains `0.1`.

### Added

- Negotiated `keepalive.v1` heartbeats now work automatically for attached
  sessions. The client detects an unresponsive broker and reconnects, while the
  broker detaches clients that stop sending heartbeats so stale sessions become
  available without user-supplied SSH options.

### Notes

- Keepalive is enabled only when both client and broker negotiate
  `keepalive.v1`; connections to older peers retain their previous behavior.
- The client sends a ping every 15 seconds. A missing pong or missing incoming
  ping is considered dead after 45 seconds.

## 0.2.1

### Added

- `--detach-connect` takes over the only existing session, or prompts when
  several exist. `--session ID` selects the takeover target explicitly.
- `--obi-version` reports the package version from both the client and server.
  Remote bootstrap now uses the local client's compiled version to replace
  servers that lack this option or report an older version.

### Changed

- Automatic reconnect now retries SSH transport failures indefinitely, including
  temporary DNS and routing failures, with a slower 1s-to-120s capped exponential
  backoff. Deadlines use suspend-aware monotonic machine time, so wall-clock
  changes do not affect retries and overdue attempts continue promptly after
  laptop resume.
- During reconnect and stale-client cleanup, the session stdin reader pauses and
  the local terminal returns to its normal mode so OpenSSH authentication and
  host-key prompts can interact with the user safely.
- The crate version is now `0.2.1`. The wire-protocol baseline remains `0.1`.
- The `server-bin` feature is enabled by default so `cargo install ssh-obi`
  installs `ssh-obi-server`, including on OpenBSD where no prebuilt archive is
  available.

### Fixed

- Remote login shells now inherit the launching process's umask instead of being
  forced to umask `0000` during daemonization. Daemon sockets are explicitly
  owner-only (`0600`) inside their validated `0700` per-user directory, so they
  do not depend on the old process-wide umask behavior.

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

- The wire protocol baseline remains `0.1`; `v0.1.3` remains compatible with
  the `0.1.x` framing and capability model.

## v0.1.2

Released for the `0.1.x` protocol line.

### Added

- On Linux systems booted with systemd and a user bus, newly spawned PTY
  children are moved into a transient user scope before exec. This mirrors the
  tmux >= 3.2 cgroup-detach behavior while remaining best-effort and
  Linux-only.
- During automatic reconnect, if the target session still reports
  `SessionBusy`, the client now asks that stale attached client to detach and
  then retries the reconnect.
- The Unix bootstrap has explicit OpenBSD handling. OpenBSD has no prebuilt
  release tarballs; when no compatible server is already installed, the
  bootstrap tells the user to install a Rust toolchain and run
  `cargo install --features server-bin ssh-obi`.
- `ssh-obi-server --list` lists currently alive sessions for the remote user on
  the server host, including busy sessions and a marker for the current session
  when run inside an `ssh-obi` session.

### Fixed

- OpenBSD install-only bootstrap runs now complete cleanly when a compatible
  `ssh-obi-server` already exists at `~/.cargo/bin/ssh-obi-server` or on
  `PATH`.

### Notes

- The wire protocol baseline remains `0.1`; `v0.1.2` remains compatible with
  the `0.1.x` framing and capability model.
- The systemd scope allocation is not required. Non-systemd Linux systems,
  non-Linux Unix systems, and systems without `busctl` or a user systemd bus
  continue through the existing PTY spawn path.

## v0.1.1

### Added

- Send the initial terminal window size before attach and new-session requests
  using the `initial-window-size.v1` capability.
- Create new remote PTYs with the client's current terminal size when available.
- Apply the client's terminal size on reattach before replaying buffered output.
- Detect compatible remote `ssh-obi-server` binaries at
  `~/.cargo/bin/ssh-obi-server` and on `PATH`, in addition to
  `~/.ssh-obi/bin/ssh-obi-server`.

### Notes

- The wire protocol baseline remains `0.1`; the new startup-size behavior is
  capability-gated.
- Remote platforms without a prebuilt tarball can be supported by installing the
  server with Cargo or through a distro package.

## v0.1.0

Initial public release.

### Added

- Local client and Unix remote server.
- Per-user sessions, detach, reconnect, and replay.
- Session listing and picker behavior.
- Exit status forwarding.
- Unix and Windows bootstrap installers.
