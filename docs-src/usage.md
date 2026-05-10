# Connecting

## Basic Commands

Attach to an existing free session or create one if none exists:

```sh
ssh-obi user@example.com
```

Always create a new session:

```sh
ssh-obi --new user@example.com
```

Attach to a specific session:

```sh
ssh-obi --session abc123 user@example.com
```

List sessions and exit:

```sh
ssh-obi --list user@example.com
```

List sessions directly on the server host:

```sh
ssh-obi-server --list
```

Detach the attached client for a specific session and exit:

```sh
ssh-obi --detach --session abc123 user@example.com
```

Detach from inside the remote shell:

```sh
ssh-obi-server --detach
```

## Using SSH Options

`ssh-obi` invokes the system `ssh` binary and forwards common OpenSSH options
before the destination.

Examples:

```sh
ssh-obi -p 2222 user@example.com
ssh-obi -i ~/.ssh/work_ed25519 user@example.com
ssh-obi -J bastion.example.com user@app.example.com
ssh-obi -F ~/.ssh/config work-host
ssh-obi -o StrictHostKeyChecking=accept-new user@example.com
ssh-obi -vv user@example.com
```

Common passthrough options include:

- Standalone flags such as `-4`, `-6`, `-A`, `-a`, `-C`, `-q`, `-T`, `-t`,
  `-X`, `-x`, `-Y`, and `-v`/`-vv`/`-vvv`.
- Options with values such as `-p`, `-i`, `-J`, `-F`, `-o`, `-l`, `-L`, `-R`,
  `-D`, and `-W`.

`ssh-obi` manages the remote command itself. Remote command arguments are not
supported:

```sh
ssh-obi user@example.com uptime
```

Use plain `ssh` for one-shot remote commands.

## Picking A Session

Free sessions are selectable. Busy sessions may be displayed for awareness but
are not assigned picker numbers.

Columns:

- `INIT`: when the session was created.
- `DETACH`: when a client last detached, or `-` if it has never detached.
- `WHAT`: best-effort foreground command detected for the session.

The `WHAT` column is only a hint.

New sessions print the remote host MOTD before the shell starts. The MOTD
sources are `/run/motd.dynamic`, `/etc/motd`, and readable non-empty files in
`/etc/motd.d/`. Create `~/.hushlogin` on the remote account to suppress this
MOTD output.

`ssh-obi-server --list` is the local server-host equivalent. It lists all alive
sessions for the current remote user, free or busy, and includes session IDs.
When run from inside an `ssh-obi` session, the current session is marked with
`*` in the `CUR` column. When run outside an `ssh-obi` session, no row is
marked current.

## When The Remote Shell Exits

If the remote shell exits while attached, the local client mirrors the exit
status where possible.

If the connection is lost before the client receives a shell-exit report, the
client reconnects first. If the session is gone or no exit status can be
recovered, the client reports failure rather than guessing success.

When an automatic reconnect targets a known session and that session still
reports an attached client, `ssh-obi` sends a detach request for that same
session and retries the attach. Normal first-time attach attempts do not detach
busy sessions automatically.

Reconnect retries use 125ms, 250ms, 500ms, 1s, and then capped 2s delays. The
client gives up after 10 attempts.

A deliberate detach through `ssh-obi-server --detach` is graceful. The client
exits with status 0 and does not reconnect.
