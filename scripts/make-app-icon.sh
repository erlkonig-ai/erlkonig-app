#!/bin/sh
# Generate the app icon from the production resting vector, never a raster asset.
# Usage: scripts/make-app-icon.sh OUTPUT_DIRECTORY [RENDERER_EXECUTABLE]
# Caller reserves the ordinary Mac build lane when no renderer is supplied.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
[ "$(uname -s)" = Darwin ] || { echo 'app icon: macOS is required' >&2; exit 1; }
[ $# -ge 1 ] && [ $# -le 2 ] || { sed -n '2,4p' "$0" >&2; exit 1; }
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd -P)
target=$out/.app-icon-build-$$
trap 'rm -rf "$target"' EXIT
renderer=${2:-}
if [ -z "$renderer" ]; then
    CARGO_TARGET_DIR=$target CARGO_INCREMENTAL=0 nice -n 10 cargo build \
        --manifest-path "$repo/scripts/app-icon/Cargo.toml" --release --locked --offline -j2
    renderer=$target/release/erlkonig-app-icon
fi
iconset=$out/Erlkonig.iconset
rm -rf "$iconset"
"$renderer" "$iconset"
iconutil --convert icns --output "$out/Erlkonig.icns" "$iconset"
printf 'app icon: wrote %s\n' "$out/Erlkonig.icns"
