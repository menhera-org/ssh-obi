# Troubleshooting

## `ssh-obi` cannot find `ssh`

Install OpenSSH client tools and make sure `ssh` is on PATH. `ssh-obi` uses the
system `ssh` binary and does not include its own SSH implementation.

## Remote install reports unsupported target

The Unix bootstrap chooses a tarball from `uname -s` and `uname -m`. If it
prints an unsupported target error, no release tarball is currently published
for that OS/CPU pair.

Use [Platform Support](./platform-support.md) to check the published target
list.

If the platform can build Rust code, install the server on the remote account
instead:

```sh
cargo install --features server-bin ssh-obi
```

The bootstrap probes `~/.cargo/bin/ssh-obi-server` directly and also accepts a
compatible `ssh-obi-server` on `PATH`, so Cargo-installed and distro-packaged
servers can be used without a prebuilt tarball.

## Windows install succeeds but `ssh-obi.exe` is not found

The Windows bootstrap updates the user's PATH. Existing terminals may not see
that change.

Open a new terminal and try:

```powershell
ssh-obi.exe --help
```

## Arrow keys do not work from Windows

Use Windows Terminal or another console with Windows virtual terminal input
support. While attached, `ssh-obi.exe` enables that mode so special keys are
sent to the remote PTY as escape sequences.

To check whether key bytes are reaching the remote shell, run:

```sh
cat -v
```

Then press Up. A working Windows client should print something like `^[[A`.
Press `Ctrl-C` to leave `cat`.

## The picker shows `(unknown)` in `WHAT`

The `WHAT` column is best-effort. It depends on the remote OS and process table
details. Failure to detect the foreground command does not affect session
correctness.

`ssh-obi` deliberately avoids wrapping or instrumenting your shell to improve
this field.

## Output repeats after reconnect

This is expected. Reattach replays the session's current bounded buffer before
live forwarding resumes. `ssh-obi` does not maintain a per-client replay
cursor.

## Old output is missing after reconnect

The session replay buffer is bounded. Old output belongs in the local terminal
scrollback.

## Reconnect eventually gives up

Automatic reconnect uses capped exponential backoff: 125ms, 250ms, 500ms, 1s,
then 2s for later attempts. After 10 failed reconnect attempts, `ssh-obi`
reports failure instead of retrying forever.

## A session is busy

A session can have only one attached client. If another client is already
attached, a second attach attempt gets `SessionBusy`.

During automatic reconnect, `ssh-obi` treats `SessionBusy` specially only for
the exact session it is trying to recover: it asks the stale attached client to
detach and then retries. Manual attaches still leave the busy session alone.

Use:

```sh
ssh-obi --list user@example.com
```

to inspect sessions, or:

```sh
ssh-obi --detach --session ID user@example.com
```

to ask the session to detach the attached client for a known session.

## MOTD is shown when a session starts

New sessions print the remote host MOTD before the shell starts. This includes
readable non-empty `/run/motd.dynamic` and `/etc/motd` files, plus readable
non-empty files in `/etc/motd.d/`.

To suppress this output for the remote account, create:

```sh
touch ~/.hushlogin
```

## A deliberate detach reconnects unexpectedly

Use the in-session helper:

```sh
ssh-obi-server --detach
```

This asks the remote session to detach and causes the client to exit gracefully.
Simply closing the terminal, killing SSH, or losing the network is ambiguous,
so the client reconnects.

## PowerShell bootstrap cannot run

Use a command that bypasses the current process policy:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://obi.menhera.org/bootstrap.ps1 | iex"
```

If that is blocked by local policy, download the script and inspect it before
running it in an allowed shell.
