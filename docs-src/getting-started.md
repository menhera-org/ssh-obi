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

If a compatible server is already installed at `~/.ssh-obi/bin`, at
`~/.cargo/bin`, or on the remote `PATH`, `ssh-obi` uses it. If the server
component is missing or incompatible and a prebuilt tarball exists for the
remote platform, `ssh-obi` asks before installing it into `~/.ssh-obi/bin` on
the remote account. No root access is needed for the built-in install path.

After installation, `ssh-obi` attaches to a new or existing session.

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

On reattach, recent output is replayed first, then live forwarding resumes. The
replay buffer is bounded, so old history belongs in your local terminal
scrollback.
