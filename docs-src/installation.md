# Installation

`ssh-obi` publishes release tarballs at `https://obi.menhera.org/`. The
bootstrap scripts download those tarballs and install binaries into
`~/.ssh-obi/bin` on Unix-like systems or `%USERPROFILE%\.ssh-obi\bin` on
Windows.

The crate is also published on crates.io as `ssh-obi`.

Signature verification is not implemented for the MVP. The bootstrap trusts
HTTPS.

## Local Client Install From crates.io

If you already have a Rust toolchain, install the local client with:

```sh
cargo install ssh-obi
```

This installs the client binary, `ssh-obi`. It does not install the remote
server binary by default. For normal use, let the client bootstrap or update
the remote server component when you connect.

For remote platforms where no prebuilt server tarball is published, install a
server-capable build in the remote account with:

```sh
cargo install ssh-obi --features server-bin
```

The attach bootstrap checks `~/.cargo/bin/ssh-obi-server` directly, so this
works even when that directory is not on `PATH`.

## Unix Install

To preinstall or update a Unix-like account without starting a session:

```sh
wget -O - https://obi.menhera.org/bootstrap.sh | sh -s -- --install
```

The `sh -s -- --install` form is deliberate and portable:

- `-s` tells a POSIX shell to read the script from standard input.
- The first `--` ends shell option parsing.
- The second `--install` is passed to the bootstrap script.

Do not use `sh - -- --install`. Portable `/bin/sh` implementations treat the
first `--` as a script filename, not as "read from stdin".

The Unix installer:

1. Detects the OS and CPU.
2. Downloads `release-<target>.tar.gz`.
3. Extracts `ssh-obi-server` and, when present, `ssh-obi`.
4. Installs binaries into `~/.ssh-obi/bin`.
5. Adds that directory to shell startup files used by non-interactive SSH:
   `~/.bashrc`, `~/.zshenv`, and fish `conf.d`.
6. Prints `OBI-INSTALL-COMPLETE` in install-only mode.

The bootstrap does not edit `~/.bash_profile`, because `ssh-obi` needs paths
available to non-interactive remote commands.

You can run the installer repeatedly to update the installed binaries. It will
not add duplicate `ssh-obi` PATH entries to shell startup files.

## Install During Connect

Most users do not need to run the Unix bootstrap manually. The local client
handles the remote check during normal connection:

```sh
ssh-obi user@example.com
```

If the remote server component is missing or incompatible, the local client asks
for confirmation and installs it into that remote account.

During normal attach, the bootstrap probes for an existing compatible server in
this order:

1. `~/.ssh-obi/bin/ssh-obi-server`
2. `~/.cargo/bin/ssh-obi-server`
3. `ssh-obi-server` found on `PATH`

The `PATH` probe supports distro-packaged installs. The direct Cargo path
supports remote platforms where `cargo install ssh-obi --features server-bin`
is the practical server install path.

## Windows Client Install

Windows can run the client only. It cannot run the remote server.

PowerShell is preferred:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://obi.menhera.org/bootstrap.ps1 | iex"
```

`cmd.exe` cannot execute a batch file directly from an HTTPS URL. Download the
batch file, then run it:

```bat
curl.exe -fsSL -o "%TEMP%\ssh-obi-bootstrap.bat" https://obi.menhera.org/bootstrap.bat && "%TEMP%\ssh-obi-bootstrap.bat" --install
```

Both Windows bootstraps:

- Support x86_64 Windows.
- Download `release-x86_64-pc-windows-msvc.tar.gz`.
- Install `ssh-obi.exe` into `%USERPROFILE%\.ssh-obi\bin`.
- Add that directory to the user's PATH.
- Print `OBI-INSTALL-COMPLETE`.
- Never start a server.

Restart the terminal if `ssh-obi.exe` is not found immediately after PATH
updates.

Windows Terminal is recommended for interactive use. `ssh-obi.exe` enables
Windows virtual terminal input while attached, so arrow keys and other
line-editing keys are forwarded to the remote shell correctly in terminals that
support that console mode.

You can run either Windows bootstrap repeatedly to update `ssh-obi.exe`. The
installer only adds the install directory to the user PATH when it is not
already present.

## Manual Install From A Tarball

Manual install is also possible:

```sh
mkdir -p "$HOME/.ssh-obi/bin"
gzip -dc release-x86_64-unknown-linux-musl.tar.gz | tar -xf - -C "$HOME/.ssh-obi/bin"
```

For Unix server-capable targets, the tarball contains:

- `LICENSE-APACHE`
- `LICENSE-MPL`
- `ssh-obi`
- `ssh-obi-server`

For Windows client-only targets, the tarball contains:

- `LICENSE-APACHE`
- `LICENSE-MPL`
- `ssh-obi.exe`

## Uninstall

Remove the install directory:

```sh
rm -rf "$HOME/.ssh-obi"
```

Then remove the `ssh-obi` PATH lines from any shell startup files that the
installer updated.

On Windows, delete `%USERPROFILE%\.ssh-obi` and remove that directory from the
user PATH.
