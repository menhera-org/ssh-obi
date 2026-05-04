# Developer Docs

This chapter is for contributors and release maintainers. The rest of this book
is intentionally user-facing.

## Repository Commands

Build the client:

```sh
cargo build --release
```

Build both client and server:

```sh
cargo build --release --features server-bin --bins
```

Run tests:

```sh
cargo test --features server-bin
```

Run clippy:

```sh
cargo clippy --all-targets --features server-bin -- -D warnings
```

Format:

```sh
cargo fmt --all
```

## Documentation Site

The site is generated from `docs-src/` with mdBook and published from `docs/`.

Use:

```sh
./build-docs.sh
```

Do not run `mdbook build` directly for published updates. mdBook overwrites
`docs/`, so `build-docs.sh` copies every non-mdBook artifact needed by GitHub
Pages after each build:

- `bootstrap.sh`
- `bootstrap.bat`
- `bootstrap.ps1`
- `.nojekyll`
- release tarballs found in known release output locations

## Release Builds

The current release is `v0.1.2`. It is published to crates.io and tagged on
GitHub as `v0.1.2`. Release tarballs for the bootstrap installers are served
from `https://obi.menhera.org/`.

Use:

```sh
./build-release.sh
```

Default build routing:

- `cross-rs`: x86_64/aarch64 Linux musl, FreeBSD, illumos, NetBSD.
- `cargo-zigbuild` plus `zig`: Darwin, riscv64 Linux musl, powerpc64le Linux
  musl.
- `cargo +nightly zigbuild -Z build-std=std,panic_abort`: s390x Linux musl.
- `cargo-xwin`: Windows x86_64 MSVC client.

Each target gets its own Cargo target directory under `target/release-build/`.
This prevents host-side Cargo build-script executables from being reused across
build environments with different libc baselines.

NetBSD uses a tiny target-only `libexecinfo` fallback because the cross-rs
NetBSD image currently lacks that target library.

Windows uses the MSVC target because it produces a smaller and more compatible
client binary than the GNU target in this release flow.

Useful environment variables:

- `CROSS`: cross-rs command, default `cross`.
- `CARGO_ZIGBUILD`: cargo-zigbuild command, default `cargo-zigbuild`.
- `CARGO_XWIN`: cargo-xwin command, default `cargo-xwin`.
- `OUT_DIR`: output directory for tarballs, default current directory.
- `RELEASE_TARGET_ROOT`: Cargo target root, default `target/release-build`.
- `CROSS_SERVER_TARGETS`: override cross-rs server target list.
- `ZIGBUILD_SERVER_TARGETS`: override zigbuild server target list.
- `CLIENT_ONLY_TARGETS`: override client-only target list.
- `ZIGBUILD_BUILD_STD_TOOLCHAIN`: nightly toolchain for build-std targets,
  default `nightly`.
- `LLVM_LIB`: explicit `llvm-lib` path for MSVC C build steps.

## High-Level Architecture

There are three remote process roles:

- Daemon: long-lived process that owns one PTY and one shell.
- Broker: short-lived process started by SSH to connect the client to a daemon.
- Detach helper: short-lived process run inside the remote shell by
  `ssh-obi-server --detach`.

The local client starts the system `ssh` binary, sends the bootstrap as the
remote command, waits for `OBI-SERVER-READY`, negotiates capabilities, selects
or creates a session, and then forwards terminal bytes.

The daemon must keep reading from the PTY while detached. Stopping reads can
fill the kernel PTY buffer and block remote commands that print output.

## Protocol Notes

Frames use:

```text
msg_type: u8
flags:    u8
length:   u32, big endian
payload:  length bytes of CBOR
```

Maximum payload length is 1 MiB. Unknown message types within the size limit
are skipped silently.

Feature negotiation is capability-based. Existing capability message formats
are frozen once released; breaking changes require new capability names.

Current capability names include:

- `pty.v1`
- `replay.v1`
- `detach.v1`
- `session-list.v1`
- `exit-code.v1`
- `initial-window-size.v1`

`initial-window-size.v1` lets the client send terminal dimensions before an
attach or new-session request. The broker applies the size before replay on
reattach, and new daemons create their PTY with that size when possible. The
protocol baseline remains `0.1`.
