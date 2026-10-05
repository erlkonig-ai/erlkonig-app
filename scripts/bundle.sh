#!/bin/sh
# Assembles Erlkonig.app from release build outputs and signs it ad hoc.
#
# Usage: scripts/bundle.sh [--release-dir DIR] [--out DIR] [--tools-dir DIR]
#                          [--faculties PATH] [--trible PATH] [--no-strip]
#
#   --release-dir  where erlkonig and erlkonig-node were built
#                  (default: $CARGO_TARGET_DIR/release, else target/release)
#   --out          where Erlkonig.app goes (default: dist/)
#   --tools-dir    where the lean faculties and trible were built (default:
#                  $ERLKONIG_TOOLS_DIR, else ~/Erlkonig/build/out); a
#                  SHA256SUMS file there is checked before anything is copied
#   --faculties    a faculties binary instead (default: $FACULTIES_BIN, else
#                  <tools-dir>/faculties)
#   --trible       a trible binary instead (default: $TRIBLE_BIN, else
#                  <tools-dir>/trible)
#   --no-strip     bundle faculties and trible unstripped
#
# There is no fallback to whatever faculties or trible is installed: the
# bundle carries the tools you name, or the lean build lane's, or nothing.
#
# Inner executables are signed first, then the bundle; the result must pass
# codesign --verify --deep --strict. Unnotarized: for JP's machines only.
set -eu

say() { printf 'bundle: %s\n' "$*"; }
die() { printf 'bundle: %s\n' "$*" >&2; exit 1; }

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
release=${CARGO_TARGET_DIR:-$repo/target}/release
out=$repo/dist
tools=${ERLKONIG_TOOLS_DIR:-$HOME/Erlkonig/build/out}
faculties=${FACULTIES_BIN:-}
trible=${TRIBLE_BIN:-}
strip_tools=1

while [ $# -gt 0 ]; do
    case $1 in
        --release-dir) [ $# -ge 2 ] || die "$1 needs a value"; release=$2; shift 2 ;;
        --out) [ $# -ge 2 ] || die "$1 needs a value"; out=$2; shift 2 ;;
        --tools-dir) [ $# -ge 2 ] || die "$1 needs a value"; tools=$2; shift 2 ;;
        --faculties) [ $# -ge 2 ] || die "$1 needs a value"; faculties=$2; shift 2 ;;
        --trible) [ $# -ge 2 ] || die "$1 needs a value"; trible=$2; shift 2 ;;
        --no-strip) strip_tools=0; shift ;;
        -h | --help) sed -n '2,26p' "$0"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ "$(uname -s)" = Darwin ] || die "bundling needs macOS (codesign, plutil)"
[ -n "$faculties" ] || faculties=$tools/faculties
[ -n "$trible" ] || trible=$tools/trible
for f in "$release/erlkonig" "$release/erlkonig-node" "$faculties" "$trible"; do
    [ -x "$f" ] || die "missing executable: $f"
done
# Record where the tools really come from, not the symlink that named them.
faculties=$(readlink -f "$faculties")
trible=$(readlink -f "$trible")
# The build lane's SHA256SUMS vouches for its outputs; check each tool that
# comes from that directory against it, so a half-copied binary never ships.
sums=$(readlink -f "$tools" 2>/dev/null || true)/SHA256SUMS
for f in "$faculties" "$trible"; do
    [ -f "$sums" ] && [ "$(dirname "$f")/SHA256SUMS" = "$sums" ] || continue
    want=$(awk -v n="${f##*/}" '$2 == n || $2 == "*" n { print $1 }' "$sums")
    [ -n "$want" ] || die "$sums has no line for ${f##*/}"
    have=$(shasum -a 256 "$f" | cut -d' ' -f1)
    [ "$have" = "$want" ] || die "${f##*/}: sha256 $have, but $sums says $want"
    say "${f##*/} matches $sums"
done
faculties_sha=$(shasum -a 256 "$faculties" | cut -d' ' -f1)
trible_sha=$(shasum -a 256 "$trible" | cut -d' ' -f1)

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
    printf 'faculties source-sha256 %s\n' "$faculties_sha"
    printf 'faculties version %s\n' "$("$tmp/Contents/MacOS/faculties" --version 2>&1 | head -1)"
    printf 'trible source %s\n' "$trible"
    printf 'trible source-sha256 %s\n' "$trible_sha"
    printf 'trible version %s\n' "$("$tmp/Contents/MacOS/trible" --version 2>&1 | head -1)"
    for f in "$tmp/Contents/MacOS/"*; do
        printf 'sha256-before-signing %s %s\n' "$(shasum -a 256 "$f" | cut -d' ' -f1)" "${f##*/}"
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
