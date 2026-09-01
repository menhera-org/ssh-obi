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

Before the shell starts, new sessions print the remote host MOTD. `ssh-obi`
prints readable non-empty `/run/motd.dynamic` and `/etc/motd` files, followed
by readable non-empty files in `/etc/motd.d/` in filename order. A
`~/.hushlogin` file in the remote user's home directory suppresses this MOTD
output.

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

For an explicit takeover, use:

```sh
ssh-obi --detach-connect user@host
```

The only existing session is automatic; several sessions produce a selector.
`--session ID` chooses the target directly. A transient busy result while the
old broker clears is handled as recovery rather than a fatal attach error.

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
Reconnect retries continue indefinitely through temporary SSH, DNS, and routing
failures, using delays of 1s, 2s, 4s, 8s, 16s, 32s, 64s, then at most 120s.
Deadlines count suspend-aware monotonic machine time rather than wall time.
Normal terminal mode is restored during recovery so OpenSSH can interactively
request credentials or confirmation.

With v0.3.0 peers, `keepalive.v1` is negotiated automatically. The client sends
a heartbeat every 15 seconds and reconnects if the broker does not answer
within 45 seconds. The broker also releases the session if client heartbeats
stop for 45 seconds. No SSH keepalive options are required. Older peers do not
negotiate this behavior.

The session sends recent output, then resumes live forwarding. This can
duplicate bytes the local terminal already displayed before the disconnect.
That is acceptable and expected.

Anything older than the replay buffer belongs in the local terminal scrollback.

## Shell Exit

When the shell exits, the session ends. A later `ssh-obi user@host` invocation
will not list that session.
