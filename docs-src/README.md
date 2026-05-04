# ssh-obi

Developed by [Human-life Information Platforms Institute
(Menhera.org)](https://www.menhera.org/).

`ssh-obi` keeps SSH shells alive when the connection drops, when a laptop
sleeps, or when the client moves between networks.

It is designed to feel like plain SSH:

- Use the same destination names, keys, jump hosts, and SSH config you already
  use.
- Keep using your local terminal scrollback, search, selection, and copy/paste.
- Reconnect to the same remote shell after a network break.
- Keep several independent sessions on the same remote account.
- Install per user, without a root service or a custom network port.

`ssh-obi` is intentionally not a terminal window manager. It does not implement
panes, tabs, or in-band escape commands. If you want window management, run
`tmux` or another multiplexer inside the remote shell.

## Status

`ssh-obi` `v0.1.1` is the current release. It is available on crates.io,
tagged as `v0.1.1` on GitHub, and distributed as release tarballs from
`https://obi.menhera.org/`.

The `v0.1.1` release improves attach startup by sending the local terminal
window size before the remote PTY is attached or created. It also recognizes
remote server binaries installed by a distro package on `PATH` or by Cargo at
`~/.cargo/bin/ssh-obi-server`.

The documentation on this site is the user-facing source for the published
bootstrap scripts, release tarballs, install flow, and supported platforms.

## Quick Examples

Connect to a host:

```sh
ssh-obi user@example.com
```

Create a new session even if free sessions already exist:

```sh
ssh-obi --new user@example.com
```

List existing sessions without attaching:

```sh
ssh-obi --list user@example.com
```

Detach the currently attached client for a known session from your local
machine:

```sh
ssh-obi --detach --session abc123 user@example.com
```

Or detach from inside the remote shell without killing it:

```sh
ssh-obi-server --detach
```

## What To Expect

- A network disconnect does not kill the remote shell.
- Remote output continues to be collected while you are detached.
- Recent output is replayed on reconnect.
- Some recently displayed output may appear twice after reconnect.
- Windows is a client-only platform. Remote servers are Unix-like systems.

## What To Read Next

- [Getting Started](./getting-started.md) for the shortest usable flow.
- [Installation](./installation.md) for Unix and Windows bootstrap commands.
- [Connecting](./usage.md) for commands and session selection.
- [Sessions](./session-model.md) for detach, reconnect, replay, and exit
  behavior.
- [Platforms and Downloads](./platform-support.md) for supported systems and
  published tarball names.
- [Changelog](./changelog.md) for release notes.
