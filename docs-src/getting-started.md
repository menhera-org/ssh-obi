# Getting Started

This page assumes you already have a local `ssh-obi` client. If you do not, see
[Installation](./installation.md).

## First Connect

Run:

```sh
ssh-obi user@example.com
```

The client starts the system `ssh` binary and prepares the remote side over the
same SSH connection. If a compatible server component is already installed, the
session starts immediately.

If a compatible, sufficiently recent server is already installed at
`~/.ssh-obi/bin`, at `~/.cargo/bin`, or on the remote `PATH`, `ssh-obi` uses it.
The bootstrap checks protocol compatibility and compares the server's
`--obi-version` output with the local client's compiled version. A missing
version option or an older server enters the install path. If a prebuilt
tarball exists for the remote platform, `ssh-obi` asks before installing it
into `~/.ssh-obi/bin` on the remote account. No root access is needed.

After installation, `ssh-obi` attaches to a new or existing session.

When a new session is created, the remote host MOTD is printed before the shell
starts, unless the remote account has `~/.hushlogin`.

## Session Selection

When you connect, `ssh-obi` looks for sessions owned by the same remote user.

If no free session exists, a new session is created.

If exactly one free session exists, the client attaches to it automatically.

If multiple free sessions exist, the client prompts locally:

```text
Select a session to attach:

  #   INIT                 DETACH               WHAT
  1   2026-05-01 09:14     2026-05-02 11:02     bash
  2   2026-05-01 14:30     2026-05-02 09:55     vim notes.md
  3   2026-05-02 10:11     2026-05-02 10:48     cargo watch
  n   (new session)

>
```

Busy sessions are shown by `--list`, but they are not selectable in the
interactive picker.

To take over an existing session, use `ssh-obi --detach-connect user@host`.
The only session is selected automatically; multiple sessions produce a picker
that includes both free and busy sessions. `--session ID` overrides selection.

## Detach Without Killing The Shell

From inside the remote shell:

```sh
ssh-obi-server --detach
```

This detaches the client. The shell keeps running. The local client exits with
status 0 and does not reconnect.

Closing the laptop, losing Wi-Fi, or killing the local SSH connection is
different: the client treats that as ambiguous and attempts to reconnect.

## Reconnect Behavior

After the first successful attach, the client knows the session id. If the SSH
connection disappears without a graceful detach or shell exit report, the
client reconnects and asks for the same session.

If the old broker is still attached when reconnect starts, the reconnecting
client asks that stale client to detach and then retries the attach.

Reconnect retries continue indefinitely through temporary SSH, DNS, and routing
failures. Delays are 1s, 2s, 4s, 8s, 16s, 32s, 64s, then at most 120s. The
deadline uses suspend-aware monotonic machine time, so wall-clock changes do not
affect it and an overdue attempt continues promptly after laptop resume.

While recovering, the client restores normal terminal mode and pauses its stdin
reader. OpenSSH can therefore ask for key passphrases, passwords, host-key
confirmation, and proxy authentication on subsequent connections.

On reattach, recent output is replayed first, then live forwarding resumes. The
replay buffer is bounded, so old history belongs in your local terminal
scrollback.
