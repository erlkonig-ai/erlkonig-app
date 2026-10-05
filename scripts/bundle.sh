#!/bin/sh
# Assembles Erlkonig.app from release build outputs and signs it ad hoc.
#
# Usage: scripts/bundle.sh [--release-dir DIR] [--out DIR]
#                          [--faculties PATH] [--trible PATH] [--no-strip]
#
#   --release-dir  where erlkonig and erlkonig-node were built
#                  (default: $CARGO_TARGET_DIR/release, else target/release)
#   --out          where Erlkonig.app goes (default: dist/)
#   --faculties    the faculties binary to bundle (default: $FACULTIES_BIN,
#                  else ~/.local/lib/faculties/current/bin/faculties)
#   --trible       the trible binary to bundle (default: $TRIBLE_BIN, else
#                  `command -v trible`, else ~/.local/bin/trible)
#   --no-strip     bundle faculties and trible unstripped
#
# Inner executables are signed first, then the bundle; the result must pass
# codesign --verify --deep --strict. Unnotarized: for JP's machines only.
set -eu

say() { printf 'bundle: %s\n' "$*"; }
die() { printf 'bundle: %s\n' "$*" >&2; exit 1; }

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
release=${CARGO_TARGET_DIR:-$repo/target}/release
out=$repo/dist
faculties=${FACULTIES_BIN:-}
trible=${TRIBLE_BIN:-}
strip_tools=1

while [ $# -gt 0 ]; do
    case $1 in
        --release-dir) [ $# -ge 2 ] || die "$1 needs a value"; release=$2; shift 2 ;;
        --out) [ $# -ge 2 ] || die "$1 needs a value"; out=$2; shift 2 ;;
        --faculties) [ $# -ge 2 ] || die "$1 needs a value"; faculties=$2; shift 2 ;;
        --trible) [ $# -ge 2 ] || die "$1 needs a value"; trible=$2; shift 2 ;;
        --no-strip) strip_tools=0; shift ;;
        -h | --help) sed -n '2,20p' "$0"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ "$(uname -s)" = Darwin ] || die "bundling needs macOS (codesign, plutil)"
[ -n "$faculties" ] || faculties=$HOME/.local/lib/faculties/current/bin/faculties
if [ -z "$trible" ]; then
    trible=$(command -v trible 2>/dev/null || true)
    [ -n "$trible" ] || trible=$HOME/.local/bin/trible
fi
for f in "$release/erlkonig" "$release/erlkonig-node" "$faculties" "$trible"; do
    [ -x "$f" ] || die "missing executable: $f"
done

mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd -P)
app=$out/Erlkonig.app
tmp=$out/.Erlkonig.app.tmp-$$
trap 'rm -rf "$tmp"' EXIT
rm -rf "$tmp"
mkdir -p "$tmp/Contents/MacOS" "$tmp/Contents/Resources" "$tmp/Contents/Library/LaunchAgents"

cp "$repo/macos/Info.plist" "$tmp/Contents/Info.plist"
printf 'APPL????' >"$tmp/Contents/PkgInfo"
cp "$repo/macos/ai.erlkonig.node.plist" "$tmp/Contents/Library/LaunchAgents/ai.erlkonig.node.plist"
plutil -lint "$tmp/Contents/Info.plist" "$tmp/Contents/Library/LaunchAgents/ai.erlkonig.node.plist"

cp "$release/erlkonig" "$release/erlkonig-node" "$tmp/Contents/MacOS/"
# -L: the installed tools are usually symlinks into a release cohort.
cp -L "$faculties" "$tmp/Contents/MacOS/faculties"
cp -L "$trible" "$tmp/Contents/MacOS/trible"
chmod 755 "$tmp/Contents/MacOS/"*
if [ "$strip_tools" = 1 ]; then
    strip "$tmp/Contents/MacOS/faculties" "$tmp/Contents/MacOS/trible"
fi

cp -R "$repo/plugin" "$tmp/Contents/Resources/plugin"
[ ! -e "$tmp/Contents/Resources/plugin/bin" ] || die "the plugin must not have a top-level bin/"

{
    printf 'built %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'app %s\n' "$(git -C "$repo" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)$(git -C "$repo" diff --quiet 2>/dev/null || echo -dirty)"
    printf 'faculties source %s\n' "$faculties"
    printf 'faculties version %s\n' "$("$tmp/Contents/MacOS/faculties" --version 2>&1 | head -1)"
    printf 'trible source %s\n' "$trible"
    printf 'trible version %s\n' "$("$tmp/Contents/MacOS/trible" --version 2>&1 | head -1)"
    for f in "$tmp/Contents/MacOS/"*; do
        printf 'sha256 %s %s\n' "$(shasum -a 256 "$f" | cut -d' ' -f1)" "${f##*/}"
    done
} >"$tmp/Contents/Resources/provenance.txt"

xattr -cr "$tmp"
for f in erlkonig-node faculties trible; do
    codesign --force --sign - --timestamp=none --identifier "ai.erlkonig.$f" "$tmp/Contents/MacOS/$f"
done
codesign --force --sign - --timestamp=none "$tmp"
codesign --verify --deep --strict --verbose=2 "$tmp"

rm -rf "$app"
mv "$tmp" "$app"
trap - EXIT
say "wrote $app ($(du -sh "$app" | cut -f1))"
sed 's/^/bundle: /' "$app/Contents/Resources/provenance.txt"
