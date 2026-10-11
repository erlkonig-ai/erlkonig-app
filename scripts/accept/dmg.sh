#!/bin/sh
# Narrow Mac packaging checks; no app launches, builds or real user profiles.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
app=${1:?usage: scripts/accept/dmg.sh /path/to/Erlkonig.app}
app=$(CDPATH= cd -- "$app" && pwd -P)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/erlkonig-dmg-test.XXXXXX")
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if mount | grep -Fq "$tmp/"; then
        printf 'FAIL: test mount remains; retained %s\n' "$tmp" >&2
        exit 1
    fi
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$tmp/out" "$tmp/bin" "$tmp/fixture"
ln -s "$repo/scripts/accept/dmg-hdiutil.sh" "$tmp/bin/hdiutil"
pack=$repo/scripts/package-dmg.sh
expect_failure() {
    text=$1; shift
    if "$@" >"$tmp/failure.log" 2>&1; then
        printf 'FAIL: expected rejection: %s\n' "$text" >&2; exit 1
    fi
    grep -Fq "$text" "$tmp/failure.log" || { cat "$tmp/failure.log" >&2; exit 1; }
    [ -z "$(find "$tmp/out" -name '.erlkonig-dmg.*' -print)" ] || { printf 'FAIL: staging remains\n' >&2; exit 1; }
    if mount | grep -Fq "$tmp/"; then printf 'FAIL: mount remains\n' >&2; exit 1; fi
}
expect_failure 'requires a path' "$pack" --app
expect_failure 'unknown argument' "$pack" --unknown
expect_failure 'real bundle directory' "$pack" --app "$tmp/missing.app"
expect_failure 'inside the app' "$pack" --app "$app" --out "$app/packaging-must-not-create"
[ ! -e "$app/packaging-must-not-create" ]
ditto "$app" "$tmp/fixture/Erlkonig.app"
touch "$tmp/fixture/Erlkonig.app/Contents/Resources/self.key"
expect_failure 'unexpected bundle file' "$pack" --app "$tmp/fixture/Erlkonig.app" --out "$tmp/out"
rm "$tmp/fixture/Erlkonig.app/Contents/Resources/self.key"
touch "$tmp/fixture/Erlkonig.app/Contents/Resources/plugin/.mcp.json"
expect_failure 'unexpected bundle file' "$pack" --app "$tmp/fixture/Erlkonig.app" --out "$tmp/out"
rm "$tmp/fixture/Erlkonig.app/Contents/Resources/plugin/.mcp.json"
printf 'changed test template\n' >>"$tmp/fixture/Erlkonig.app/Contents/Resources/plugin/README.md"
expect_failure 'not the source plugin template' "$pack" --app "$tmp/fixture/Erlkonig.app" --out "$tmp/out"
original=$(shasum -a 256 "$app/Contents/MacOS/erlkonig")
"$pack" --app "$app" --out "$tmp/out"
image=$(find "$tmp/out" -name '*.dmg' -type f -print)
[ -n "$image" ]
digest=$(shasum -a 256 "$image")
expect_failure 'output exists' "$pack" --app "$app" --out "$tmp/out"
[ "$digest" = "$(shasum -a 256 "$image")" ]
expect_failure 'injected create failure' env PATH="$tmp/bin:$PATH" ERLKONIG_DMG_TEST_FAILURE=create "$pack" --app "$app" --out "$tmp/out" --overwrite
[ "$digest" = "$(shasum -a 256 "$image")" ]
expect_failure 'injected failure after successful attach' env PATH="$tmp/bin:$PATH" ERLKONIG_DMG_TEST_FAILURE=after-attach "$pack" --app "$app" --out "$tmp/out" --overwrite
[ "$digest" = "$(shasum -a 256 "$image")" ]
mv "$image" "$tmp/original.dmg"
ln -s "$tmp/original.dmg" "$image"
expect_failure 'not a regular file' "$pack" --app "$app" --out "$tmp/out" --overwrite
rm "$image"
mv "$tmp/original.dmg" "$image"
"$pack" --app "$app" --out "$tmp/out" --overwrite
[ "$original" = "$(shasum -a 256 "$app/Contents/MacOS/erlkonig")" ]
codesign --verify --deep --strict "$app"
printf 'PASS: DMG mount/payload/signature; input/secret/cache rejection; no-clobber/overwrite; create and attached-mount failure cleanup; unchanged source\n'
