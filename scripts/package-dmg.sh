#!/bin/sh
# Package an existing signed bundle; never build, re-sign, launch or install it.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
app=
out=$repo/dist.noindex
overwrite=false
usage() {
    printf 'Usage: scripts/package-dmg.sh --app /path/to/Erlkonig.app [--out DIR] [--overwrite]\n'
    printf 'Creates a verified, development-only UDZO image. No signing or notarization.\n'
}
fail() { printf 'package-dmg: %s\n' "$*" >&2; exit 1; }
while [ $# -gt 0 ]; do
    case $1 in
        --app|--out)
            [ $# -ge 2 ] && [ -n "$2" ] || fail "$1 requires a path"
            case $2 in --*) fail "$1 requires a path" ;; esac
            if [ "$1" = --app ]; then app=$2; else out=$2; fi
            shift 2 ;;
        --overwrite) overwrite=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done
[ "$(uname -s)" = Darwin ] || fail 'macOS is required'
[ -n "$app" ] || { usage >&2; fail '--app is required'; }
[ -d "$app" ] && [ ! -L "$app" ] || fail 'app must be a real bundle directory'
app=$(CDPATH= cd -- "$app" && pwd -P)
[ "$(basename -- "$app")" = Erlkonig.app ] || fail 'expected Erlkonig.app'
case "$app$out" in *'
'*) fail 'paths must not contain newlines' ;; esac
for tool in ditto hdiutil codesign plutil lipo otool python3 shasum; do
    command -v "$tool" >/dev/null || fail "missing tool: $tool"
done
plist=$app/Contents/Info.plist
plutil -lint "$plist" >/dev/null
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")
build=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist")
minimum=$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist")
for value in "$version" "$build" "$minimum"; do
    case $value in ''|*[!0-9A-Za-z._-]*) fail 'unsafe or missing bundle version' ;; esac
done
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")" = ai.erlkonig.app ] || fail 'unexpected bundle identifier'
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")" = erlkonig ] || fail 'unexpected executable'
binary=$app/Contents/MacOS/erlkonig
[ -x "$binary" ] || fail 'missing executable'
arch=$(lipo -archs "$binary")
[ "$arch" = arm64 ] || fail "only arm64 is supported (found: $arch)"
deployment=$(otool -l "$binary" | awk '/LC_BUILD_VERSION/{block=1} block && $1 == "minos" {print $2; exit}')
[ -n "$deployment" ] || fail 'missing Mach-O deployment target'
name=Erlkonig-$version-$build-macos-$arch-development.dmg
# Fail closed on extra resources, including personal plugin caches or user state.
# The hidden provider manifest directories below are intentional source templates.
python3 -I - "$app" <<'PY'
import json, pathlib, re, stat, sys
app = pathlib.Path(sys.argv[1])
files = {
    "Contents/Info.plist", "Contents/PkgInfo", "Contents/MacOS/erlkonig",
    "Contents/_CodeSignature/CodeResources", "Contents/Resources/Erlkonig.icns",
    "Contents/Resources/provenance.txt",
}
plugin_files = {"README.md", ".codex-plugin/plugin.json", ".claude-plugin/plugin.json",
                "skills/faculties/SKILL.md"}
files |= {"Contents/Resources/plugin/" + p for p in plugin_files}
optional = {"Contents/Resources/binary-provenance.txt", "Contents/Resources/source-freeze.json"}
dirs = {str(parent) for p in files for parent in pathlib.PurePath(p).parents if str(parent) != "."}
seen = set()
for p in app.rglob("*"):
    rel, mode = str(p.relative_to(app)), p.lstat().st_mode
    assert not stat.S_ISLNK(mode), f"unexpected bundle symlink: {rel}"
    if stat.S_ISDIR(mode):
        assert rel in dirs, f"unexpected bundle directory: {rel}"
    else:
        assert stat.S_ISREG(mode) and rel in files | optional, f"unexpected bundle file: {rel}"
        seen.add(rel)
assert files <= seen, f"missing bundle files: {sorted(files - seen)}"
freeze = app / "Contents/Resources/source-freeze.json"
if freeze.is_file():
    # This optional signed companion carries source identity, never user state.
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            assert key not in result, "duplicate source-freeze field"
            result[key] = value
        return result
    assert freeze.stat().st_size <= 16384, "source-freeze metadata is too large"
    data = json.loads(freeze.read_text(encoding="utf-8"), object_pairs_hook=unique_object)
    assert isinstance(data, dict) and set(data) == {"authority", "app", "core_branch", "source_closure", "locks"}, "unexpected source-freeze fields"
    assert data["authority"] == "Candidate-only compile/test/stage authorized by root; no acceptance or activation", "unexpected source-freeze authority"
    assert isinstance(data["core_branch"], str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]{0,127}; not asserted landed", data["core_branch"]), "invalid source-freeze core branch"
    branch = data["core_branch"].split(";", 1)[0]
    assert not any(part in ("", ".", "..") or part.endswith((".", ".lock")) for part in branch.split("/")) and ".." not in branch, "invalid source-freeze core branch"
    def digest(value, length):
        return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{%d}" % length, value) is not None
    assert digest(data["app"], 40), "invalid source-freeze app revision"
    repositories = {"erlkonig-app", "triblespace-rs", "faculties", "mary", "GORBIE", "songbird", "anybytes", "jerky", "burn-fork", "cubecl-fork", "cubek-fork", "soma"}
    assert isinstance(data["source_closure"], list), "invalid source-freeze source closure"
    sources = {}
    for source in data["source_closure"]:
        assert isinstance(source, dict) and set(source) == {"repository", "revision", "tree"}, "unexpected source-freeze source fields"
        name = source["repository"]
        assert isinstance(name, str) and name in repositories and name not in sources, "unexpected or duplicate source-freeze repository"
        assert digest(source["revision"], 40) and digest(source["tree"], 40), "invalid source-freeze revision or tree"
        sources[name] = source["revision"]
    assert set(sources) == repositories and sources["erlkonig-app"] == data["app"], "incomplete or inconsistent source-freeze source closure"
    lock_files = {"erlkonig-app/Cargo.lock", "faculties/Cargo.lock", "triblespace-rs/Cargo.lock", "erlkonig-app/scripts/app-icon/Cargo.lock"}
    assert isinstance(data["locks"], list), "invalid source-freeze locks"
    locks = set()
    for lock in data["locks"]:
        assert isinstance(lock, dict) and set(lock) == {"file", "sha256"}, "unexpected source-freeze lock fields"
        name = lock["file"]
        assert isinstance(name, str) and name in lock_files and name not in locks, "unexpected or duplicate source-freeze lock path"
        assert digest(lock["sha256"], 64), "invalid source-freeze lock hash"
        locks.add(name)
    assert locks == lock_files, "incomplete source-freeze locks"
PY
for file in README.md .codex-plugin/plugin.json .claude-plugin/plugin.json skills/faculties/SKILL.md; do
    cmp -s "$app/Contents/Resources/plugin/$file" "$repo/plugin/$file" || fail "not the source plugin template: $file"
done
codesign --verify --deep --strict "$app"
if [ -d "$out" ]; then
    out=$(CDPATH= cd -- "$out" && pwd -P)
else
    [ ! -e "$out" ] && [ ! -L "$out" ] || fail 'output is not a directory'
    # Resolve before mkdir: even a new output child must not modify the app.
    parent=$(CDPATH= cd -- "$(dirname -- "$out")" && pwd -P) || fail 'output parent directory must exist'
    out=$parent/$(basename -- "$out")
fi
case "$out/" in "$app/"*) fail 'output must not be inside the app' ;; esac
[ -d "$out" ] || mkdir "$out"
[ -w "$out" ] || fail 'output directory is not writable'
destination=$out/$name
check_destination() {
    if [ -e "$destination" ] || [ -L "$destination" ]; then
        [ "$overwrite" = true ] || fail "output exists; use --overwrite explicitly: $destination"
        [ -f "$destination" ] && [ ! -L "$destination" ] || fail 'output is not a regular file'
    fi
}
check_destination
work=$(mktemp -d "$out/.erlkonig-dmg.XXXXXX")
mountpoint=$work/mount
mount_attempted=false
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$mount_attempted" = true ] && mount | grep -Fq " on $mountpoint "; then
        if ! hdiutil detach "$mountpoint"; then
            hdiutil detach -force "$mountpoint" || {
                printf 'package-dmg: detach failed; retained owned work directory: %s\n' "$work" >&2
                exit 1
            }
        fi
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$work/stage" "$mountpoint"
ditto "$app" "$work/stage/Erlkonig.app"
ln -s /Applications "$work/stage/Applications"
codesign --verify --deep --strict "$work/stage/Erlkonig.app"
diff -qr "$app" "$work/stage/Erlkonig.app"
hdiutil create -quiet -fs HFS+ -format UDZO -volname "Erlkonig $version" -srcfolder "$work/stage" "$work/image.dmg"
hdiutil verify -quiet "$work/image.dmg"
mount_attempted=true
hdiutil attach -readonly -nobrowse -mountpoint "$mountpoint" -plist "$work/image.dmg" >"$work/attach.plist"
python3 -I - "$mountpoint" <<'PY'
import pathlib, sys
root = pathlib.Path(sys.argv[1])
# HFS+ may add its own housekeeping directories; none is distributed user data.
system = {".HFS+ Private Directory Data\r", ".Spotlight-V100", ".fseventsd", ".Trashes"}
assert {p.name for p in root.iterdir()} - system == {"Erlkonig.app", "Applications"}, "unexpected DMG root contents"
link = root / "Applications"
assert link.is_symlink() and str(link.readlink()) == "/Applications", "invalid Applications link"
assert (root / "Erlkonig.app").is_dir() and not (root / "Erlkonig.app").is_symlink(), "invalid app payload"
PY
diff -qr "$app" "$mountpoint/Erlkonig.app"
codesign --verify --deep --strict "$mountpoint/Erlkonig.app"
hdiutil detach "$mountpoint"
mount_attempted=false
check_destination
if [ "$overwrite" = true ]; then
    mv -f "$work/image.dmg" "$destination"
else
    # Atomic no-clobber publication, including a competing packager's output.
    ln "$work/image.dmg" "$destination"
fi
printf 'package-dmg: verified and wrote %s\n' "$destination"
printf 'version=%s build=%s architecture=%s min-macOS=%s Mach-O-minOS=%s format=UDZO\n' "$version" "$build" "$arch" "$minimum" "$deployment"
shasum -a 256 "$destination"
printf 'Development image only: existing app signature preserved; DMG unsigned and not notarized.\n'
