# Changelog

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
