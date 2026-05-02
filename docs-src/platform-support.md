# Platforms and Downloads

## Local Client Platforms

Supported local client platforms:

- Linux
- macOS
- FreeBSD, NetBSD, and illumos where release artifacts are published
- Windows x86_64

The client requires a working system `ssh` binary.

## Remote Server Platforms

Supported remote server platforms:

- Linux
- macOS
- FreeBSD
- NetBSD
- illumos

The server component is not supported on Windows.

systemd-based Linux distributions are supported, but systemd is not required.
No system-wide service install is required.

## Downloads

Release files are served from `https://obi.menhera.org/`.

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
