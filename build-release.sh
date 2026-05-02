#!/bin/sh

set -eu

cross_bin="${CROSS:-cross}"
out_dir="${OUT_DIR:-.}"
tmp="${TMPDIR:-/tmp}/ssh-obi-release.$$"

server_targets='
x86_64-unknown-linux-musl
x86_64-unknown-freebsd
x86_64-unknown-illumos
x86_64-apple-darwin
x86_64-unknown-netbsd
riscv64gc-unknown-linux-musl
aarch64-unknown-linux-musl
aarch64-apple-darwin
powerpc64le-unknown-linux-musl
'

client_only_targets='
x86_64-pc-windows-gnu
'

cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$out_dir" "$tmp"

archive_target() {
    target="$1"
    exe_suffix="$2"
    include_server="$3"
    build_dir="target/${target}/release"
    stage="${tmp}/${target}"
    archive="${out_dir}/release-${target}.tar.gz"

    rm -rf "$stage"
    mkdir -p "$stage"

    cp LICENSE-APACHE "$stage/LICENSE-APACHE"
    cp LICENSE-MPL "$stage/LICENSE-MPL"
    cp "${build_dir}/ssh-obi${exe_suffix}" "$stage/ssh-obi${exe_suffix}"

    if [ "$include_server" -eq 1 ]; then
        cp "${build_dir}/ssh-obi-server${exe_suffix}" "$stage/ssh-obi-server${exe_suffix}"
        (
            cd "$stage"
            tar -cf - LICENSE-APACHE LICENSE-MPL "ssh-obi${exe_suffix}" "ssh-obi-server${exe_suffix}"
        ) | gzip -c >"$archive"
    else
        (
            cd "$stage"
            tar -cf - LICENSE-APACHE LICENSE-MPL "ssh-obi${exe_suffix}"
        ) | gzip -c >"$archive"
    fi

    printf '%s\n' "$archive"
}

for target in $server_targets; do
    "$cross_bin" build --release --target "$target" --features server-bin --bins
    archive_target "$target" "" 1
done

for target in $client_only_targets; do
    case "$target" in
        *windows*) exe_suffix=".exe" ;;
        *) exe_suffix="" ;;
    esac

    "$cross_bin" build --release --target "$target" --bin ssh-obi
    archive_target "$target" "$exe_suffix" 0
done
