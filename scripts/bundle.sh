#!/bin/sh
# One app, one executable. The native libraries are linked into erlkonig.
# Usage: scripts/bundle.sh [--release-dir DIR] [--out DIR] [--sign IDENTITY]
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
release=${CARGO_TARGET_DIR:-$repo/target}/release
out=$repo/dist.noindex
identity=${ERLKONIG_SIGN_IDENTITY:--}
while [ $# -gt 0 ]; do
    case $1 in
        --release-dir) release=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --sign) identity=$2; shift 2 ;;
        -h|--help) sed -n '2,3p' "$0"; exit 0 ;;
        *) echo "bundle: unknown argument: $1" >&2; exit 1 ;;
    esac
done
[ "$(uname -s)" = Darwin ] || { echo 'bundle: macOS is required' >&2; exit 1; }
[ -x "$release/erlkonig" ] || { echo 'bundle: missing erlkonig executable' >&2; exit 1; }
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd -P)
app=$out/Erlkonig.app
tmp=$out/.Erlkonig.app.tmp-$$
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/Contents/MacOS" "$tmp/Contents/Resources"
cp "$repo/macos/Info.plist" "$tmp/Contents/Info.plist"
printf 'APPL????' >"$tmp/Contents/PkgInfo"
cp "$release/erlkonig" "$tmp/Contents/MacOS/erlkonig"
chmod 755 "$tmp/Contents/MacOS/erlkonig"
cp -R "$repo/plugin" "$tmp/Contents/Resources/plugin"
{
    printf 'built %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'app %s\n' "$(git -C "$repo" rev-parse HEAD)"
    printf 'app-tracked-diff-sha256 %s\n' "$(git -C "$repo" diff --binary HEAD | shasum -a 256 | cut -d' ' -f1)"
    for dependency in faculties triblespace-rs anybytes jerky mary GORBIE cubecl-fork burn-fork cubek-fork soma; do
        printf '%s %s\n' "$dependency" "$(git -C "$repo/../$dependency" rev-parse HEAD)"
    done
    printf 'cargo-lock-sha256 %s\n' "$(shasum -a 256 "$repo/Cargo.lock" | cut -d' ' -f1)"
    printf 'sha256-before-signing %s erlkonig\n' "$(shasum -a 256 "$tmp/Contents/MacOS/erlkonig" | cut -d' ' -f1)"
    printf 'lifecycle one application process; no helper executables or LaunchAgents\n'
} >"$tmp/Contents/Resources/provenance.txt"
xattr -cr "$tmp"
codesign --force --sign "$identity" --timestamp=none "$tmp"
codesign --verify --deep --strict "$tmp"
rm -rf "$app"
mv "$tmp" "$app"
trap - EXIT
printf 'bundle: wrote %s\n' "$app"
