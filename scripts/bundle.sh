#!/bin/sh
# One app executable. Client-scoped stdio uses a forwarding-only mode of it.
# Usage: scripts/bundle.sh [--release-dir DIR | --reuse-app APP] [--out DIR] [--sign IDENTITY] [--icon-renderer EXE]
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
release=${CARGO_TARGET_DIR:-$repo/target}/release
executable=
out=$repo/dist.noindex
identity=${ERLKONIG_SIGN_IDENTITY:--}
reuse=
icon_renderer=
while [ $# -gt 0 ]; do
    case $1 in
        --release-dir) release=$2; shift 2 ;;
        --reuse-app) reuse=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --sign) identity=$2; shift 2 ;;
        --icon-renderer) icon_renderer=$2; shift 2 ;;
        -h|--help) sed -n '2,3p' "$0"; exit 0 ;;
        *) echo "bundle: unknown argument: $1" >&2; exit 1 ;;
    esac
done
[ "$(uname -s)" = Darwin ] || { echo 'bundle: macOS is required' >&2; exit 1; }
if [ -n "$reuse" ]; then
    codesign --verify --deep --strict "$reuse"
    [ -f "$reuse/Contents/Resources/provenance.txt" ] || { echo 'bundle: reused app needs binary provenance' >&2; exit 1; }
    executable=$reuse/Contents/MacOS/erlkonig
fi
# Cargo builds erlkonig-mac; the bundle's CFBundleExecutable is erlkonig.
executable=${executable:-$release/erlkonig-mac}
[ -x "$executable" ] || { echo "bundle: missing executable $executable" >&2; exit 1; }
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd -P)
app=$out/Erlkonig.app
tmp=$out/.Erlkonig.app.tmp-$$
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/Contents/MacOS" "$tmp/Contents/Resources"
cp "$repo/macos/Info.plist" "$tmp/Contents/Info.plist"
printf 'APPL????' >"$tmp/Contents/PkgInfo"
cp "$executable" "$tmp/Contents/MacOS/erlkonig"
chmod 755 "$tmp/Contents/MacOS/erlkonig"
cp -R "$repo/plugin" "$tmp/Contents/Resources/plugin"
if [ -n "$icon_renderer" ]; then
    "$repo/scripts/make-app-icon.sh" "$out/icon.noindex" "$icon_renderer"
else
    "$repo/scripts/make-app-icon.sh" "$out/icon.noindex"
fi
cp "$out/icon.noindex/Erlkonig.icns" "$tmp/Contents/Resources/Erlkonig.icns"
if [ -n "$reuse" ]; then
    binary_provenance=$reuse/Contents/Resources/provenance.txt
    if [ -f "$reuse/Contents/Resources/binary-provenance.txt" ]; then
        binary_provenance=$reuse/Contents/Resources/binary-provenance.txt
    fi
    cp "$binary_provenance" "$tmp/Contents/Resources/binary-provenance.txt"
fi
{
    printf 'built %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'app %s\n' "$(git -C "$repo" rev-parse HEAD)"
    printf 'app-tracked-diff-sha256 %s\n' "$(git -C "$repo" diff --binary HEAD | shasum -a 256 | cut -d' ' -f1)"
    if [ -n "$reuse" ]; then
        printf 'app-source-role packaging-only\n'
        printf 'packaging-only-reused-executable true\n'
        printf 'binary-build-provenance binary-provenance.txt\n'
        printf 'binary-provenance-sha256 %s\n' "$(shasum -a 256 "$tmp/Contents/Resources/binary-provenance.txt" | cut -d' ' -f1)"
    else
        for dependency in faculties triblespace-rs anybytes jerky mary GORBIE cubecl-fork burn-fork cubek-fork soma; do
            printf '%s %s\n' "$dependency" "$(git -C "$repo/../$dependency" rev-parse HEAD)"
        done
    fi
    printf 'cargo-lock-sha256 %s\n' "$(shasum -a 256 "$repo/Cargo.lock" | cut -d' ' -f1)"
    printf 'app-icon-sha256 %s\n' "$(shasum -a 256 "$tmp/Contents/Resources/Erlkonig.icns" | cut -d' ' -f1)"
    printf 'app-icon-crown-source-sha256 %s\n' "$(shasum -a 256 "$repo/crates/erlkonig-app/src/crown.rs" | cut -d' ' -f1)"
    printf 'app-icon-geometry exact production crown::frame(Duration::ZERO, None, false, false)\n'
    printf 'sha256-before-signing %s erlkonig\n' "$(shasum -a 256 "$tmp/Contents/MacOS/erlkonig" | cut -d' ' -f1)"
    printf 'lifecycle one resident app owner; same-executable connection-scoped stdio; no second backend or LaunchAgent\n'
} >"$tmp/Contents/Resources/provenance.txt"
xattr -cr "$tmp"
codesign --force --sign "$identity" --timestamp=none "$tmp"
codesign --verify --deep --strict "$tmp"
rm -rf "$app"
mv "$tmp" "$app"
trap - EXIT
printf 'bundle: wrote %s\n' "$app"
