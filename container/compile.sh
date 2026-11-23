#!/bin/sh
set -eu
rustup target add wasm32-unknown-unknown
native=$(dpkg --print-architecture)
if [ "$1" = "$native" ]; then
    cargo build --release --locked -p erlkonig-cli
    binary=target/release/erlkonig
else
    case "$1" in
        amd64) cpu=x86_64; compiler=g++-x86-64-linux-gnu; linker_var=CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER ;;
        arm64) cpu=aarch64; compiler=g++-aarch64-linux-gnu; linker_var=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER ;;
        *) echo "Unsupported Linux architecture: $1" >&2; exit 1 ;;
    esac
    target="$cpu-unknown-linux-gnu"
    cross="$cpu-linux-gnu"
    apt-get update
    apt-get install -y --no-install-recommends "$compiler"
    rustup target add "$target"
    env "$linker_var=$cross-gcc" cargo build --release --locked --target "$target" -p erlkonig-cli
    binary="target/$target/release/erlkonig"
fi
install -D -m 0755 "$binary" /build/bin/erlkonig
