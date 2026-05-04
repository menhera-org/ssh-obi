# Sessions

An `ssh-obi` session is one long-lived remote shell. The session can outlive
many SSH connections.

## Creating Sessions

Use `ssh-obi user@host` to attach to a free session or create one when none is
available.

Use `ssh-obi --new user@host` to always create a new session.

Use `ssh-obi --session ID user@host` to attach to a specific session.

New sessions start the remote user's shell as a login shell, using the usual
leading-dash argv[0] convention such as `-bash` or `-zsh`. This lets shell
startup behavior match interactive SSH more closely.

New sessions also start in the remote user's home directory. `TERM` is
forwarded from the local client when it is useful; if it is missing or `dumb`,
`ssh-obi` uses `xterm-256color`.

When both sides support `initial-window-size.v1`, the client sends the current
terminal size before creating or attaching to a session. New remote PTYs start
with that size, and reattaches apply the size before replaying buffered output.

## Busy Sessions

A session can have only one attached client. If another client is already
attached, the session is busy.

Busy sessions are still visible in `--list`. You can also ask a known busy
session to detach its current client:

```sh
ssh-obi --detach --session ID user@host
```

On the server host itself, `ssh-obi-server --list` lists all alive sessions for
the current Unix user. If it is run inside an `ssh-obi` session, that session is
marked in the `CUR` column. If it is run outside an `ssh-obi` session, no
session is marked current.

During automatic reconnect, `ssh-obi` already knows the session it is trying to
recover. If that session is still marked busy because the previous broker has
not fully gone away, the reconnecting client asks the stale attached client to
detach and then retries the attach.

## Detach

Detach means "drop the client, keep the shell".

When the network drops, the remote shell keeps running and waits for another
client.

When the user runs:

```sh
ssh-obi-server --detach
```

the local client exits cleanly and does not reconnect.

The shell is not sent SIGHUP.

## Output While Detached

Remote output continues to be collected while detached.

This prevents commands that print output from getting stuck just because no
client is attached.

Detached output is kept in a bounded replay buffer. When the buffer fills, old
bytes are evicted.

## Reconnect And Replay

After the first attach, the client knows the session id. On an ambiguous
disconnect, it starts a fresh SSH connection and requests that same session.

If that reconnect attempt finds the session busy, the client sends a detach
control request for the same session and retries. Manual first-time attaches
still report a busy session rather than detaching another client automatically.

The session sends recent output, then resumes live forwarding. This can
duplicate bytes the local terminal already displayed before the disconnect.
That is acceptable and expected.

Anything older than the replay buffer belongs in the local terminal scrollback.

## Shell Exit

When the shell exits, the session ends. A later `ssh-obi user@host` invocation
will not list that session.
