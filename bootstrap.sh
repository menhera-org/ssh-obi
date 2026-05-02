#!/bin/sh
set -eu

want="0.1"
install_only=0
term="${TERM:-xterm-256color}"

if [ "$#" -gt 0 ] && [ "$1" = "--" ]; then
    shift
fi

if [ "$#" -gt 0 ] && [ "$1" = "--install" ]; then
    install_only=1
    shift
fi

if [ "$#" -gt 0 ]; then
    want="$1"
    shift
fi

if [ "$#" -gt 0 ] && [ "$1" = "--install" ]; then
    install_only=1
    shift
fi

if [ "$#" -gt 1 ] && [ "$1" = "--term" ]; then
    term="$2"
    shift 2
fi

if [ "$term" = "" ] || [ "$term" = "dumb" ]; then
    term="xterm-256color"
fi
TERM="$term"
export TERM

install_root="${HOME}/.ssh-obi"
bin_dir="${install_root}/bin"
server="${bin_dir}/ssh-obi-server"
base_url="https://obi.menhera.org"

if [ -x "$server" ] && "$server" --protocol-check "$want" >/dev/null 2>&1; then
    if [ "$install_only" -eq 0 ]; then
        printf '%s\n' 'OBI-SERVER-READY'
        exec "$server" "$@"
    fi
fi

if [ "$install_only" -eq 0 ]; then
    printf '%s\n' 'OBI-INSTALL-REQUIRED'
    IFS= read -r answer
    if [ "$answer" != 'OBI-INSTALL-OK' ]; then
        printf '%s\n' 'OBI-INSTALL-ABORTED'
        exit 1
    fi
fi

system="$(uname -s)"
machine="$(uname -m)"

case "${system}:${machine}" in
    Linux:x86_64 | Linux:amd64)
        target="x86_64-unknown-linux-musl"
        ;;
    Linux:aarch64 | Linux:arm64)
        target="aarch64-unknown-linux-musl"
        ;;
    Linux:riscv64)
        target="riscv64gc-unknown-linux-musl"
        ;;
    Linux:ppc64le | Linux:powerpc64le)
        target="powerpc64le-unknown-linux-musl"
        ;;
    Darwin:x86_64 | Darwin:amd64)
        target="x86_64-apple-darwin"
        ;;
    Darwin:arm64 | Darwin:aarch64)
        target="aarch64-apple-darwin"
        ;;
    FreeBSD:x86_64 | FreeBSD:amd64)
        target="x86_64-unknown-freebsd"
        ;;
    NetBSD:x86_64 | NetBSD:amd64)
        target="x86_64-unknown-netbsd"
        ;;
    SunOS:x86_64 | SunOS:amd64)
        target="x86_64-unknown-illumos"
        ;;
    *)
        printf 'OBI-ERROR unsupported target %s/%s\n' "$system" "$machine"
        exit 1
        ;;
esac

archive_url="${base_url}/release-${target}.tar.gz"
tmp="${TMPDIR:-/tmp}/ssh-obi-install.$$"
archive="${tmp}/release.tar.gz"

cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$tmp"

if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$archive_url" -o "$archive"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$archive" "$archive_url"
elif command -v fetch >/dev/null 2>&1; then
    fetch -qo "$archive" "$archive_url"
elif command -v ftp >/dev/null 2>&1; then
    ftp -o "$archive" -M "$archive_url"
else
    printf '%s\n' 'OBI-ERROR no supported downloader found'
    exit 1
fi

gzip -dc "$archive" | tar -xf - -C "$tmp"

if [ ! -f "${tmp}/ssh-obi-server" ]; then
    printf '%s\n' 'OBI-ERROR release archive does not contain ssh-obi-server'
    exit 1
fi

mkdir -p "$bin_dir"
cp "${tmp}/ssh-obi-server" "${bin_dir}/ssh-obi-server"
if [ -f "${tmp}/ssh-obi" ]; then
    cp "${tmp}/ssh-obi" "${bin_dir}/ssh-obi"
fi
chmod 755 "${bin_dir}/ssh-obi-server"
if [ -f "${bin_dir}/ssh-obi" ]; then
    chmod 755 "${bin_dir}/ssh-obi"
fi

path_line='export PATH="$HOME/.ssh-obi/bin:$PATH"'

append_once() {
    file="$1"
    line="$2"
    mkdir -p "$(dirname "$file")"
    touch "$file"
    if ! grep -F "$line" "$file" >/dev/null 2>&1; then
        {
            printf '\n'
            printf '%s\n' '# ssh-obi'
            printf '%s\n' "$line"
        } >>"$file"
    fi
}

append_once "${HOME}/.bashrc" "$path_line"
append_once "${HOME}/.zshenv" "$path_line"

fish_dir="${HOME}/.config/fish/conf.d"
mkdir -p "$fish_dir"
fish_file="${fish_dir}/ssh-obi.fish"
fish_line='fish_add_path -g "$HOME/.ssh-obi/bin"'
touch "$fish_file"
if ! grep -F "$fish_line" "$fish_file" >/dev/null 2>&1; then
    printf '%s\n' "$fish_line" >>"$fish_file"
fi

if "$server" --protocol-check "$want" >/dev/null 2>&1; then
    if [ "$install_only" -eq 1 ]; then
        printf '%s\n' 'OBI-INSTALL-COMPLETE'
        exit 0
    fi
    printf '%s\n' 'OBI-SERVER-READY'
    exec "$server" "$@"
fi

printf '%s\n' 'OBI-ERROR installed server failed protocol check'
exit 1
