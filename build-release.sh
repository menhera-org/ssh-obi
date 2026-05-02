#!/bin/sh

set -eu

cross_bin="${CROSS:-cross}"
zigbuild_bin="${CARGO_ZIGBUILD:-cargo-zigbuild}"
xwin_bin="${CARGO_XWIN:-cargo-xwin}"
out_dir="${OUT_DIR:-.}"
target_root="${RELEASE_TARGET_ROOT:-target/release-build}"
tmp="${TMPDIR:-/tmp}/ssh-obi-release.$$"
project_dir="$(pwd -P)"
: "${MACOSX_DEPLOYMENT_TARGET:=11.0}"
export MACOSX_DEPLOYMENT_TARGET

default_cross_server_targets='
x86_64-unknown-linux-musl
x86_64-unknown-freebsd
x86_64-unknown-illumos
x86_64-unknown-netbsd
aarch64-unknown-linux-musl
'

default_zigbuild_server_targets='
x86_64-apple-darwin
aarch64-apple-darwin
riscv64gc-unknown-linux-musl
powerpc64le-unknown-linux-musl
'

default_client_only_targets='
x86_64-pc-windows-msvc
'

cross_server_targets="${CROSS_SERVER_TARGETS-$default_cross_server_targets}"
zigbuild_server_targets="${ZIGBUILD_SERVER_TARGETS-$default_zigbuild_server_targets}"
client_only_targets="${CLIENT_ONLY_TARGETS-$default_client_only_targets}"

cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$out_dir" "$tmp"

need_command() {
    command="$1"
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'missing required command: %s\n' "$command" >&2
        exit 1
    fi
}

need_command "$cross_bin"
need_command "$zigbuild_bin"
need_command "$xwin_bin"
need_command rustup
need_command zig

first_available_command() {
    for command in "$@"; do
        if command -v "$command" >/dev/null 2>&1; then
            command -v "$command"
            return 0
        fi
    done

    return 1
}

ensure_rust_target() {
    target="$1"
    rustup target add "$target"
}

is_darwin_target() {
    case "$1" in
        *-apple-darwin) return 0 ;;
        *) return 1 ;;
    esac
}

relative_path_from_project() {
    path="$1"

    if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import os, sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' "$path" "$project_dir"
        return
    fi

    printf 'python3 is required when SDKROOT is absolute; set SDKROOT relative to the repository instead\n' >&2
    exit 1
}

run_zigbuild() {
    target="$1"
    cargo_target_dir="$2"

    if is_darwin_target "$target" && [ -n "${SDKROOT:-}" ]; then
        case "$SDKROOT" in
            /*)
                if [ ! -d "$SDKROOT" ]; then
                    printf 'SDKROOT does not exist: %s\n' "$SDKROOT" >&2
                    exit 1
                fi

                sdkroot_relative="$(relative_path_from_project "$SDKROOT")"
                (
                    cd "$project_dir"
                    SDKROOT="$sdkroot_relative" "$zigbuild_bin" zigbuild \
                        --manifest-path "$project_dir/Cargo.toml" \
                        --target-dir "$cargo_target_dir" \
                        --release \
                        --target "$target" \
                        --features server-bin \
                        --bins
                )
                return
                ;;
        esac
    fi

    "$zigbuild_bin" zigbuild --target-dir "$cargo_target_dir" --release --target "$target" --features server-bin --bins
}

run_xwin_client() {
    target="$1"
    cargo_target_dir="$2"

    llvm_lib="${LLVM_LIB:-}"
    if [ -z "$llvm_lib" ]; then
        if ! llvm_lib="$(first_available_command llvm-lib llvm-lib-21 llvm-lib-20)"; then
            printf 'missing required command for MSVC builds: llvm-lib, llvm-lib-21, or llvm-lib-20\n' >&2
            exit 1
        fi
    fi

    AR_x86_64_pc_windows_msvc="$llvm_lib" "$xwin_bin" build --target-dir "$cargo_target_dir" --release --target "$target" --bin ssh-obi
}

# Cargo host artifacts, including build-script executables, are not portable
# across cross images with different glibc baselines. Keep a separate target
# directory per release target so each image executes the build scripts it built.
archive_target() {
    target="$1"
    exe_suffix="$2"
    include_server="$3"
    cargo_target_dir="$4"
    build_dir="${cargo_target_dir}/${target}/release"
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

for target in $cross_server_targets; do
    cargo_target_dir="${project_dir}/${target_root}/${target}"
    "$cross_bin" build --target-dir "$cargo_target_dir" --release --target "$target" --features server-bin --bins
    archive_target "$target" "" 1 "$cargo_target_dir"
done

for target in $zigbuild_server_targets; do
    cargo_target_dir="${project_dir}/${target_root}/${target}"
    ensure_rust_target "$target"
    run_zigbuild "$target" "$cargo_target_dir"
    archive_target "$target" "" 1 "$cargo_target_dir"
done

for target in $client_only_targets; do
    case "$target" in
        *windows*) exe_suffix=".exe" ;;
        *) exe_suffix="" ;;
    esac

    cargo_target_dir="${project_dir}/${target_root}/${target}"
    case "$target" in
        *windows*)
            ensure_rust_target "$target"
            run_xwin_client "$target" "$cargo_target_dir"
            ;;
        *)
            "$cross_bin" build --target-dir "$cargo_target_dir" --release --target "$target" --bin ssh-obi
            ;;
    esac
    archive_target "$target" "$exe_suffix" 0 "$cargo_target_dir"
done
