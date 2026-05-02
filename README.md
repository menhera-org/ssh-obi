# ssh-obi

> Lightweight per-user terminal multiplexer with auto-reconnecting sessions tunneled over plain SSH — preserves the local terminal's native scrollback (unlike mosh) and requires no system-wide daemon (unlike Eternal Terminal).

The name is from obi (帯), the Japanese sash that holds a kimono together — fitting for a tool whose job is to keep a remote shell tied to your terminal across disconnects.

**Status:** early development. CLI, wire protocol, and library APIs are unstable until `0.1.0`.

## What it is

`ssh-obi` keeps remote shells alive across SSH disconnects, suspend/resume, and IP changes.

- **Transport is plain SSH.** No separate port, no UDP, no custom auth — if you can `ssh user@host`, you can `ssh-obi user@host`.
- **The persistent server is per-user.** No setuid binary, no system service, no shared state between users.
- **The data path is a byte stream**, so the local terminal emulator's scrollback, search, and copy-paste keep working exactly as with vanilla SSH.
- **Multiple concurrent sessions per user** on a remote, each surviving disconnects independently. On connect the client picks the right one: auto-attach if exactly one is free, prompt if many.

**Scope boundaries** (commitments, not "not yet"):

- **No window or pane management.** Pure 1:1 forwarding between local TTY and remote PTY. Combine with tmux on the remote if you want multiplexing.
- **No in-band escape sequences.** Bytes typed locally are bytes the remote shell sees. Detach is initiated by running `ssh-obi-server --detach` *inside* the remote session.
- **Not a screen-state replicator.** Only a bounded recent-output buffer is replayed on reconnect — older history lives in the local terminal's scrollback.

## Design clarifications

These clarify implementation choices that are easy to get subtly wrong:

- **SSH transport uses the system `ssh` binary.** We rely on the file descriptors exposed by the locally executed `ssh` process for the single SSH connection. During bootstrap, the client does not send framed protocol bytes until the script has printed `OBI-SERVER-READY` and then `exec`'d `ssh-obi-server`; this avoids protocol bytes being consumed as shell-script input.
- **No systemd assumption.** systemd-based systems are supported, but the daemon design is not systemd-based and does not assume `user@UID.service`, `systemd-run`, or linger setup. The server daemonizes itself and should be treated as a plain per-user daemon.
- **Daemon socket requests are role-specific.** A session may have at most one attached broker, but the daemon must still accept non-broker control requests while busy. Listing/info probes and detach requests are valid even when a broker is attached.
- **Replay may duplicate recent bytes.** Reattach replays the daemon's current bounded ring buffer, and duplicates are acceptable. The implementation does not track a per-client replay cursor.
- **Ambiguous disconnects reconnect first.** If the client loses the SSH/broker connection without a graceful `ClientShouldExit` or shell-exit report, it reconnects. If the session is then gone or cannot provide an exit status, the client fails rather than guessing success.
- **Foreground command detection is best-effort only.** The daemon must not interfere with the spawned shell to improve the `WHAT` column. POSIX-ish process-group/proc-table guessing is acceptable, and failure falls back to `(unknown)`.
- **Windows is client-only.** The Windows build can run the local `ssh-obi` client and use the system `ssh` binary, but Windows is not a supported remote server platform.

## Architecture

Three process roles on the remote:

1. **Daemon** (`ssh-obi-server --daemon`) — owns one PTY master and one user shell. One daemon per session. Long-lived; daemonizes itself and is not managed through any required system service. Listens on a UNIX-domain socket at `/tmp/ssh-obi-<uid>/<session-id>.sock`. Tracks: init time, last-detach time, attached state, and the current foreground command on its PTY.
2. **Broker** (`ssh-obi-server`, no flag) — short-lived, spawned by SSH inside the login session. Bridges SSH stdio ↔ a chosen daemon's UNIX socket. Dies when SSH disconnects; the daemon does not.
3. **Detach helper** (`ssh-obi-server --detach`) — short-lived, run inside a remote session by the user. Reads `$SSH_OBI_SOCKET` from its environment, connects to the daemon as a control requester, sends `Detach`, exits.

### Session selection on connect

Each `ssh-obi user@host` invocation goes through this dance over a single SSH connection:

1. Client SSH-execs the bootstrap (see "Auto-install"); bootstrap exec's `ssh-obi-server`.
2. Broker enumerates sessions for the current user — sockets under `/tmp/ssh-obi-<uid>/`, probed for liveness and queried with a daemon info request — and reports the *free* ones (no attached broker) to the client as `(session_id, init_time, last_detach_time, current_command)`.
3. Client decides:
   - **Zero free sessions** → request "new session"; daemon spawns; broker attaches.
   - **Exactly one free session** → attach to it automatically.
   - **Multiple free sessions** → prompt the user locally:
     ```
     Select a session to attach:

       #   INIT                 DETACH               WHAT
       1   2026-05-01 09:14     2026-05-02 11:02     bash
       2   2026-05-01 14:30     2026-05-02 09:55     vim notes.md
       3   2026-05-02 10:11     2026-05-02 10:48     cargo watch
       n   (new session)

     >
     ```
4. Client sends the choice (`1`, `2`, …, or `n`); broker connects to the chosen daemon (or spawns a new one), sends an `AttachedSession` message containing the authoritative session id, and then real PTY forwarding takes over on the same stdio channel.

A session is "free" when no broker is currently attached. Attached sessions are excluded from the picker — one client per session. The picker filter is UX; the **authoritative arbiter is the daemon itself**, which refuses any second broker attach request with a `SessionBusy` control message and closes that requester. Info/listing and detach requests remain valid while a broker is attached. This handles the race between "list sessions" and "attach to chosen one," and the case where two clients both pass `--session ID` for the same active session. The losing client surfaces `session is currently in use` to the user. Override the auto/prompt logic with:

- `ssh-obi --new user@host` — always create a new session.
- `ssh-obi --session ID user@host` — attach to a specific session ID.
- `ssh-obi --list user@host` — print all sessions and exit without attaching. Busy sessions are marked busy.
- `ssh-obi --detach --session ID user@host` — detach the attached client for a specific session and exit without attaching.

When prompting interactively, free sessions are numbered and selectable. Busy sessions are shown for awareness but are unnumbered and unselectable. Sessions that have never detached display `-` in the `DETACH` column. Picker timestamps are rendered in the client's local time.

The client invokes the system `ssh` binary with `-T` because `ssh-obi` owns the remote command and the broker protocol is binary over stdio. It passes through common OpenSSH client options before the destination, including `-p`, `-i`, `-J`, `-F`, `-o`, `-l`, `-4`, `-6`, and `-v`; host aliases are left to `ssh` configuration. Remote command arguments are rejected.

### Detach

When the broker exits (network drop, or `ssh-obi-server --detach`), the daemon's local socket sees EOF. The daemon updates `last_detach_time` and waits for the next broker. The shell is *not* sent SIGHUP — surviving disconnects is the whole point.

The daemon **continues draining the PTY master fd at all times**, attached or not. This is not optional: if the daemon ever stops reading, the kernel's PTY buffer fills, and the shell blocks on its next `write()` — which would effectively pause every running process whose output reaches the terminal. So during detach, output is read continuously and appended to the same bounded ring buffer (default 64 KiB) used for replay. When the ring fills, oldest bytes are evicted. On reattach, the ring's current contents are replayed to the new broker, then live forwarding resumes.

`ssh-obi-server --detach` differs from a network drop by sending a `Detach` control request to the daemon even though the broker is still attached. The daemon then sends `ClientShouldExit` to the broker before closing the broker connection. The client treats `ClientShouldExit` as graceful (status 0, no reconnect). All other disconnections hit the reconnect loop. Getting this asymmetry right is load-bearing.

### Shell exit

When the user's shell exits normally, the daemon catches SIGCHLD, forwards the exit code to the broker (so the client can mirror it as its own exit code), unlinks its socket, and exits. If the client loses the broker connection before receiving that report, it reconnects first; if the session is gone or no exit status can be recovered, the client reports failure rather than guessing the shell's status.

### Reconnect resumption

Reconnect is **byte-stream-based**, not screen-state-based. After the first attach, the client knows the authoritative session id from `AttachedSession`; on an ambiguous disconnect it starts a fresh SSH/broker connection and requests that same session id. The new broker reattaches, the daemon replays the current ring contents and then resumes live forwarding. This can duplicate output the client already displayed before the disconnect; duplicates are acceptable. Anything older than the ring is gone — the client's terminal scrollback already has it. We do not maintain any states about terminal screens or per-client replay cursors.

## Project layout

```
ssh-obi/
├── Cargo.toml
├── LICENSE-APACHE
├── LICENSE-MPL
├── README.md
├── bootstrap.sh                    # embedded into the client via include_str!
├── bootstrap.bat                   # Windows cmd.exe client installer
├── bootstrap.ps1                   # Windows PowerShell client installer
├── build-docs.sh                   # builds mdBook and re-copies published artifacts into docs/
├── build-release.sh                # builds release tarballs with cross-rs, cargo-zigbuild, cargo-xwin, and tar
└── src/
    ├── lib.rs                      # re-exports the modules below as the `ssh_obi` library crate
    ├── protocol.rs                 # wire protocol: framing, capability handshake, control messages
    ├── session.rs                  # session id, listing, picker logic, on-remote enumeration
    ├── foreground.rs               # best-effort WHAT lookup from PTY foreground process group
    ├── pty.rs                      # PTY open, child spawn, winsize
    ├── terminal.rs                 # local raw-mode guard
    ├── transport.rs                # framed I/O over std::io::Read + std::io::Write
    ├── daemon.rs                   # double-fork + setsid + chdir + umask + stdio redirect; replay ring
    └── bin/
        ├── ssh-obi/
        │   └── main.rs             # client: arg parsing, SSH spawn, raw-mode TTY, picker UI, reconnect loop
        └── ssh-obi-server/
            └── main.rs             # broker (default), daemon (--daemon), detach helper (--detach)
```

## Wire protocol

Stable across all major versions. Forward-compatible by design:

- **Fixed framing.** `msg_type: u8`, `flags: u8`, `length: u32 (BE)`, then `length` bytes of CBOR-encoded payload. Receivers must skip unknown `msg_type` silently.
- **Conservative frame limits.** Maximum payload length is 1 MiB. Oversized lengths, malformed CBOR, invalid required fields, or other protocol violations close the connection with an error. Unknown message types at or below the size limit are skipped silently.
- **Capability handshake, not version negotiation.** Each side sends a list of capability strings (`pty.v1`, `replay.v1`, `detach.v1`, `session-list.v1`, `exit-code.v1`). The intersection is the working set. New features ship as new capabilities; existing capabilities are frozen on first release. There is no "v2 of `pty.v1`" — that would be `pty.v2`, a separate capability.
- **Message bodies are CBOR** with stable field tags. Adding a field requires a new capability, never an in-place edit to an existing one.
- **`ssh-obi-server --protocol-check <baseline>`** is the binary-compatibility probe used by the bootstrap. Returns 0 if the server can speak the framing/handshake of any protocol the given baseline supports. This is the only place a version *number* appears on the wire — and it's about binary compatibility, not feature compatibility.

## Auto-install over SSH

A single SSH invocation handles "install if needed, then attach" with one auth round-trip. The embedded bootstrap script is passed as the remote command body, not streamed over SSH stdin, because stdin must remain available for install confirmation and then the broker protocol after `ssh-obi-server` is exec'd:

```sh
ssh -T host 'sh -c <embedded-bootstrap> sh <args>'
```

Bootstrap behavior on the remote:

1. If `~/.ssh-obi/bin/ssh-obi-server --protocol-check $WANT` succeeds, write `OBI-SERVER-READY` to stdout and `exec` it.
2. Otherwise write `OBI-INSTALL-REQUIRED` to stdout and read a line from stdin.
3. The client prompts the user locally: `installing ssh-obi on host, continue? [Y/n]`. It writes `OBI-INSTALL-OK` or aborts.
4. The bootstrap downloads the release archive (`curl -fsSL` || `wget -qO-` || `fetch -qo -` || `ftp -o - -M`), trusting HTTPS for the MVP, installs to `~/.ssh-obi/bin/`, updates shell rc files (`~/.bashrc`, `~/.zshenv`, `~/.config/fish/conf.d/ssh-obi.fish` — *not* `~/.bash_profile`, because non-interactive SSH execution is the target), writes `OBI-SERVER-READY`, and `exec`s the server.
5. Real handshake takes over on the same stdio channel.

Bootstrap output uses the `OBI-` line prefix so motd/rc-file noise can't be mistaken for protocol. Strict line discipline: only `OBI-` marker lines may be written to stdout before the bootstrap `exec`s the server. The client must not write framed protocol bytes until `OBI-SERVER-READY` is observed and the server has taken over stdio. `bootstrap.sh` is `include_str!`'d into the client at build time — single source of truth.

The bootstrap also supports install-only mode for users who want to prepare or update a remote manually without starting a server:

```sh
wget -O - https://obi.menhera.org/bootstrap.sh | sh -s -- --install
```

The `sh -s -- --install` form is intentional: `-s` tells a POSIX shell to read the script from stdin, and `--` ends shell option parsing before passing `--install` to the script. Do not use `sh - -- --install`; portable `/bin/sh` implementations treat the first `--` as a script filename, not as "read from stdin." In install-only mode, the script skips the interactive `OBI-INSTALL-REQUIRED` confirmation, installs or updates `~/.ssh-obi/bin/ssh-obi-server`, prints `OBI-INSTALL-COMPLETE`, and exits without execing the server. It can be run repeatedly without adding duplicate shell startup entries.

Windows has separate client-only bootstraps because Windows is not a supported server platform. PowerShell is preferred:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://obi.menhera.org/bootstrap.ps1 | iex"
```

`cmd.exe` cannot execute a batch file directly from an HTTPS URL, and batch files do not have a portable `sh -s` equivalent with arguments. Download the batch file to a temporary path, then run it:

```bat
curl.exe -fsSL -o "%TEMP%\ssh-obi-bootstrap.bat" https://obi.menhera.org/bootstrap.bat && "%TEMP%\ssh-obi-bootstrap.bat" --install
```

Both Windows bootstraps install the client-only `release-x86_64-pc-windows-msvc.tar.gz`, copy `ssh-obi.exe` to `%USERPROFILE%\.ssh-obi\bin`, add that directory to the user's PATH, print `OBI-INSTALL-COMPLETE`, and exit without starting a server.

## Cargo.toml (skeleton)

```toml
[package]
autobins = false

[features]
default = []
server-bin = []

[[bin]]
name = "ssh-obi"
path = "src/bin/ssh-obi/main.rs"

[[bin]]
name = "ssh-obi-server"
path = "src/bin/ssh-obi-server/main.rs"
required-features = ["server-bin"]

[dependencies]
ciborium  = "0.2"      # CBOR encoding for protocol bodies
clap      = { version = "4", features = ["derive"] }
blake3    = "1"        # session id generation
chrono    = { version = "0.4", default-features = false, features = ["clock"] }
serde     = { version = "1", features = ["derive"] }

[target.'cfg(unix)'.dependencies]
nix       = { version = "0.31", features = ["term", "process", "signal", "fs", "user"] }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Console"] }
```

`nix` features rationale: `term` covers PTY + termios; `process` covers `fork`/`setsid`; `signal` covers SIGHUP/SIGCHLD/SIGWINCH; `fs` covers `umask`/`chdir` during daemonization; `user` covers uid lookup for per-user socket paths. The broker ↔ daemon UNIX-domain channel uses `std::os::unix::net::UnixStream`.

Cargo binary auto-discovery is disabled. `ssh-obi` is always declared, while `ssh-obi-server` requires the explicit `server-bin` feature so Windows `--bins` builds only the client. Unix release/dev commands that need the server binary must pass `--features server-bin`.

## Platform support

- **Linux:** primary target. systemd-based and non-systemd distributions are supported, but systemd is not part of the required design. The daemon uses ordinary daemonization and should not require a system-wide service, setuid helper, or user service manager.
- **macOS, FreeBSD, OpenBSD, NetBSD, illumos:** supported. Plain double-fork suffices.
- **Windows:** client-only. Running the server on Windows is out of scope.
- **Android:** out of scope.

## Build & develop

```sh
cargo build --release
cargo build --release --features server-bin --bins
cargo test
cargo clippy --all-targets --features server-bin -- -D warnings
cargo fmt --all
```

Integration tests pipe a client process directly to a server process (no real SSH) to exercise the wire protocol, picker logic, reconnect, and detach.

## Key design decisions

These are settled — please don't relitigate without discussion:

- **`nix` for syscalls, not `libc` directly.** Cleaner errors, fewer `unsafe` blocks, smooths BSD/Linux/macOS differences. Drop to `libc` only for the rare call `nix` doesn't expose.
- **Synchronous std I/O for the MVP.** The current implementation keeps the runtime small: `nix` handles PTY/process/termios setup, and std threads handle SSH stdio and UNIX-socket forwarding.
- **Hand-rolled daemonization, not the `daemonize` crate.** We need to detach *after* the daemon's UNIX socket has bound, so bind failures can be reported to the user before going silent.
- **Bounded replay buffer, not screen reconstruction.** No mosh-style framebuffer ownership; the local terminal owns the screen and scrollback.
- **Session IDs are server-generated**, short (8–10 chars of base32-encoded blake3 over high-resolution monotonic time + random nonce), and unique within a user's session set on a given remote. *Not* derived from client identity — that would be incompatible with multi-machine selection.
- **Sessions are user-namespaced on the remote.** Any client authenticating as the same remote user can list and attach to that user's free sessions, regardless of which workstation it's coming from.
- **"WHAT" column** in the picker is a best-effort guess at the current foreground process for the session's PTY, using POSIX-ish process-group and per-OS process-info lookup where available. Do not instrument, wrap, or otherwise interfere with the spawned shell to improve this value. Falls back to `(unknown)` on platforms where lookup fails.
- **Three modes for the server binary, dispatched in `main()`** by `--daemon` / `--detach` / no flag (broker default).
- **Detach is out-of-band** via `$SSH_OBI_SESSION` and `$SSH_OBI_SOCKET` exported into the shell's environment. `ClientShouldExit` is the only signal the client treats as graceful (status 0, no reconnect); every other disconnect hits the reconnect loop.
- **`SessionBusy` is the daemon-level broker-attach race arbiter.** Any second broker attach request whose first broker hasn't yet seen EOF is refused with `SessionBusy` and that requester is closed. Info/listing and detach control requests are allowed even while the session is busy. The picker's exclusion of attached sessions is UX layered on top; correctness comes from the daemon, not from coordinated clients. This means a brief window where two pickers see the same session as "free" cannot result in two clients sharing one PTY — at most one wins, the other gets a clear error.
- **Daemon never stops reading the PTY master.** Whether attached or detached, the read loop runs continuously and feeds the bounded ring buffer. Stopping reads would let the kernel's PTY buffer fill and block the shell's writes — which would silently freeze every process that prints to the terminal. This is non-negotiable; do not introduce conditional reading "to save CPU on long detaches."
- **Shell exit terminates the session.** When the daemon's child shell exits (any cause: normal `exit`, `kill`, OOM, SIGSEGV), the daemon catches SIGCHLD, forwards the exit code and signal info through to any attached broker so the client can mirror them, unlinks its socket, and exits. The session is gone — next `ssh-obi user@host` will not see it in the picker.
- **Socket location: `/tmp/ssh-obi-<uid>/<session-id>.sock`.** Per-user subdir created with mode 0700, ownership-checked on reuse. Stale sockets handled via `connect()`-then-`unlink()`-on-`ECONNREFUSED`. Refuses to start on NFS/CIFS/SMB. Validates path length under 100 bytes (macOS/BSD `sun_path` cap is 104).
- **Wire protocol is forward-compatible by capability negotiation,** not version bumping. Once a capability ships, its message format is frozen forever. The only on-wire version number is `--protocol-check`, which gates binary compatibility of the framing/handshake layer.
- **Auto-install over a single SSH invocation** via embedded `bootstrap.sh` passed as the remote command body. One auth round-trip total, even on first-ever connect to a remote. SSH stdin stays available for install confirmation and then the broker protocol. The client waits for the bootstrap marker before sending protocol bytes. Bootstrap writes shell rc files reachable from non-interactive SSH (`~/.bashrc`, `~/.zshenv`, fish `conf.d`), not login-only files.

## mdBook-based obi.menhera.org site

`docs/` hosts a mdBook-generated documentation website and it is not `.gitignore`-d. It is to be served by GitHub pages. It is to contain release tarballs built with cross-rs, `cargo-zigbuild`, or `cargo-xwin`, named like `release-<cargo target>.tar.gz` ('release' is literal, not a version). The release archive is unpacked using a maximally-portable set of commands. No GNUism, no BSDism, etc. Signature verification is skipped for MVP, trusting HTTPS. It will be located at `https://obi.menhera.org/`.

Build `docs/` with `./build-docs.sh`, not by running `mdbook build` directly. mdBook overwrites `docs/` on every build, so `build-docs.sh` must copy every non-mdBook artifact needed by the published site after each build, including `bootstrap.sh`, `bootstrap.bat`, `bootstrap.ps1`, `.nojekyll`, and any `release-<cargo target>.tar.gz` archives.

Each tarball contains only `LICENSE-APACHE`, `LICENSE-MPL`, `ssh-obi`, and `ssh-obi-server`, except client-only targets where `ssh-obi-server` is omitted. Platform executable suffixes are kept, so the Windows client-only tarball contains `ssh-obi.exe`.

Build release archives with `./build-release.sh`. It builds x86_64/aarch64 Linux, FreeBSD, illumos, and NetBSD targets with cross-rs. NetBSD uses a tiny target-only `libexecinfo` fallback during the build because the cross-rs NetBSD image currently lacks that target library. It builds Darwin, riscv64 Linux musl, powerpc64le Linux musl, and s390x Linux musl targets with `cargo-zigbuild` plus `zig`: Darwin avoids custom cross images, and riscv64/powerpc64le Linux musl avoid targets with no published cross-rs Docker image. `s390x-unknown-linux-musl` has no prebuilt Rust std component on stable, so the script builds it with `cargo +nightly zigbuild -Z build-std=std,panic_abort`; override that toolchain with `ZIGBUILD_BUILD_STD_TOOLCHAIN`. It builds the Windows client-only target as `x86_64-pc-windows-msvc` with `cargo-xwin`, producing a slimmer MSVC binary that avoids the cross-rs MinGW image's missing `GetHostNameW` import. The script installs the prebuilt Rust std components needed by regular zigbuild and xwin targets with `rustup target add`, installs `rust-src` for build-std targets, stages only the license files plus the target binaries, and creates the corresponding `release-<cargo target>.tar.gz` files with `tar` piped through `gzip`. Each target is built with its own Cargo target directory under `target/release-build/` so host-side build-script executables are not shared across build environments with different libc baselines. When `SDKROOT` is set to an absolute path for Darwin builds, the script passes it to `cargo-zigbuild` as a path relative to the repository root; this avoids Zig 0.14 treating `-L$SDKROOT/usr/lib` as relative to `--sysroot` and looking under `$SDKROOT/$SDKROOT/usr/lib`. `build-docs.sh` then copies those tarballs into `docs/` on every mdBook build.

- `release-x86_64-unknown-linux-musl.tar.gz`
- `release-x86_64-unknown-freebsd.tar.gz`
- `release-x86_64-unknown-illumos.tar.gz`
- `release-x86_64-apple-darwin.tar.gz`
- `release-x86_64-unknown-netbsd.tar.gz`
- `release-riscv64gc-unknown-linux-musl.tar.gz`
- `release-aarch64-unknown-linux-musl.tar.gz`
- `release-aarch64-apple-darwin.tar.gz`
- `release-powerpc64le-unknown-linux-musl.tar.gz`
- `release-s390x-unknown-linux-musl.tar.gz`

- `release-x86_64-pc-windows-msvc.tar.gz` (client only)

Rust/toolchain limitations. No other targets can be added at this stage.

## License

Dual-licensed at the user's option under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- Mozilla Public License, Version 2.0 ([LICENSE-MPL](LICENSE-MPL) or <https://www.mozilla.org/en-US/MPL/2.0/>)
