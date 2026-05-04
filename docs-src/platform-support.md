# Platforms and Downloads

## Local Client Platforms

Supported local client platforms:

- Linux
- macOS
- FreeBSD, NetBSD, and illumos where release artifacts are published
- Windows x86_64

The client requires a working system `ssh` binary.

On Windows, use Windows Terminal or another console that supports Windows
virtual terminal input. This lets special keys such as arrows, Home/End, and
other line-editing keys reach the remote shell as normal terminal escape
sequences.

## Remote Server Platforms

Supported remote server platforms:

- Linux
- macOS
- FreeBSD
- OpenBSD when installed from Cargo or a distro package
- NetBSD
- illumos
- Other Unix-like systems where `ssh-obi-server` can be built from source and
  installed with Cargo or a distro package.

The server component is not supported on Windows.

systemd-based Linux distributions are supported, but systemd is not required.
No system-wide service install is required.

## Downloads

Release files are served from `https://obi.menhera.org/`.

The tarball list below is the prebuilt artifact set. Some Unix-like platforms,
such as OpenBSD, are intentionally supported through a locally installed
`ssh-obi-server` rather than a prebuilt tarball. Install with Cargo or a distro
package, then connect normally; the bootstrap will use a compatible server from
`~/.cargo/bin/ssh-obi-server` or from `PATH`.

Server-capable tarballs:

- `release-x86_64-unknown-linux-musl.tar.gz`
- `release-aarch64-unknown-linux-musl.tar.gz`
- `release-riscv64gc-unknown-linux-musl.tar.gz`
- `release-powerpc64le-unknown-linux-musl.tar.gz`
- `release-s390x-unknown-linux-musl.tar.gz`
- `release-x86_64-apple-darwin.tar.gz`
- `release-aarch64-apple-darwin.tar.gz`
- `release-x86_64-unknown-freebsd.tar.gz`
- `release-x86_64-unknown-netbsd.tar.gz`
- `release-x86_64-unknown-illumos.tar.gz`

Client-only tarballs:

- `release-x86_64-pc-windows-msvc.tar.gz`

Server-capable tarballs contain `ssh-obi` and `ssh-obi-server`. Windows
client-only tarballs contain `ssh-obi.exe`.

## Out Of Scope

- Android.
- Windows remote servers.
- Terminal pane/window management.
- UDP transport.
- Screen-state replication.
