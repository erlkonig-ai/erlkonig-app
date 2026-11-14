#!/bin/sh
# Direct distribution of the current one-executable, entitlement-free app.
# Never builds, installs, launches, or modifies the input app. Requires an
# existing Developer ID Application identity and existing notarytool profile.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
app= out= identity= profile= source_commit= run_id= run_attempt=
fail() { printf 'release-dmg: %s\n' "$*" >&2; exit 1; }
usage() {
    printf 'Usage: scripts/release-dmg.sh --app APP --out EXISTING_DIR --identity "Developer ID Application: ..." --notary-profile NAME --source-commit SHA --run-id ID --run-attempt N\n'
    printf 'Signs a copy, submits its signed DMG to Apple, staples and validates it, then publishes without overwriting.\n'
}
while [ $# -gt 0 ]; do
    case $1 in
        --app|--out|--identity|--notary-profile|--source-commit|--run-id|--run-attempt)
            [ $# -ge 2 ] && [ -n "$2" ] || fail "$1 requires a value"
            case $2 in --*) fail "$1 requires a value" ;; esac
            case $1 in
                --app) app=$2 ;; --out) out=$2 ;;
                --identity) identity=$2 ;; --notary-profile) profile=$2 ;;
                --source-commit) source_commit=$2 ;; --run-id) run_id=$2 ;; --run-attempt) run_attempt=$2 ;;
            esac
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done
[ -n "$app" ] && [ -n "$out" ] && [ -n "$identity" ] && [ -n "$profile" ] || { usage >&2; fail 'all four inputs are required'; }
python3 -I - "$source_commit" "$run_id" "$run_attempt" <<'PYINPUT'
import re, sys
assert re.fullmatch('[0-9a-f]{40}', sys.argv[1]), 'exact source commit required'
assert all(re.fullmatch('[1-9][0-9]*', v) for v in sys.argv[2:]), 'trusted CI run ID/attempt required'
PYINPUT
case $identity in 'Developer ID Application: '?*) ;; *) fail 'use a full Developer ID Application identity name' ;; esac
case "$app$out$identity$profile" in *'
'*) fail 'inputs must not contain newlines' ;; esac
[ "$(uname -s)" = Darwin ] || fail 'macOS is required'
for tool in ditto codesign xcrun python3 shasum; do
    command -v "$tool" >/dev/null || fail "missing tool: $tool"
done
[ -d "$app" ] && [ ! -L "$app" ] || fail 'app must be a real bundle directory'
app=$(CDPATH= cd -- "$app" && pwd -P)
[ "$(basename -- "$app")" = Erlkonig.app ] || fail 'expected Erlkonig.app'
[ -d "$out" ] || fail 'output directory must already exist'
out=$(CDPATH= cd -- "$out" && pwd -P)
case "$out/" in "$app/"*) fail 'output must not be inside the app' ;; esac
work=$(mktemp -d "$out/.erlkonig-release.XXXXXX")
submitted=false
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$status" -ne 0 ] && [ "$submitted" = true ]; then
        # A timeout/disconnect can leave a live Apple submission. Keep its JSON
        # and exact signed bytes for diagnosis; never automatically resubmit.
        printf 'release-dmg: submission artifacts retained at %s\n' "$work" >&2
    else
        rm -rf "$work"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
ditto "$app" "$work/Erlkonig.app"
# No optional entitlements are supported by this release lane. In particular,
# signing must not accidentally activate the dormant Secure Enclave path.
printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict/></plist>' >"$work/entitlements.plist"
codesign --force --sign "$identity" --options runtime --timestamp --entitlements "$work/entitlements.plist" "$work/Erlkonig.app"
codesign --verify --deep --strict "$work/Erlkonig.app"
codesign --display --verbose=4 "$work/Erlkonig.app" 2>"$work/signature.txt"
grep -q '^Authority=Developer ID Application: ' "$work/signature.txt" || fail 'signature is not Developer ID Application'
grep -q '^Timestamp=' "$work/signature.txt" || fail 'signature has no secure timestamp'
grep -q 'flags=.*runtime' "$work/signature.txt" || fail 'signature has no hardened runtime'
mkdir "$work/dmg"
# The existing packager rejects extra code/resources and checks the exact
# source plugin templates. It supports one executable, not nested helpers.
"$repo/scripts/package-dmg.sh" --app "$work/Erlkonig.app" --out "$work/dmg"
set -- "$work/dmg/"*-development.dmg
[ $# -eq 1 ] && [ -f "$1" ] || fail 'expected exactly one development DMG'
dmg=$1
name=$(basename -- "$dmg" -development.dmg).dmg
destination=$out/$name
[ ! -e "$destination" ] && [ ! -L "$destination" ] || fail "output exists: $destination"
[ ! -e "$destination.receipt.json" ] && [ ! -L "$destination.receipt.json" ] || fail 'receipt output exists' 
codesign --sign "$identity" --timestamp --identifier ai.erlkonig.app.dmg "$dmg"
codesign --verify --strict "$dmg"
submitted=true
# The profile is a name, never a password or API key. No credential flags,
# tracing, forced uploads, retries, or profile creation belong in this script.
xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait --timeout 30m --output-format json >"$work/notarization.json"
python3 -I - "$work/notarization.json" <<'PY'
import json, pathlib, sys
result = json.loads(pathlib.Path(sys.argv[1]).read_text())
if result.get("status") != "Accepted":
    raise SystemExit("release-dmg: notarization did not return Accepted")
request_id = result.get("id")
if not isinstance(request_id, str) or not request_id.strip():
    raise SystemExit("release-dmg: Accepted response has no request ID")
print(json.dumps({"notarization": {"id": request_id, "status": "Accepted"}}, separators=(",", ":")))
PY
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg" >"$work/stapler-validation.txt" 2>&1
codesign --verify --strict "$dmg"
python3 -I - "$dmg" "$work" "$source_commit" "$run_id" "$run_attempt" <<'PYRECEIPT'
import hashlib, json, pathlib, plistlib, sys
artifact, work = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
hash = hashlib.sha256()
with artifact.open('rb') as source:
    for chunk in iter(lambda: source.read(1048576), b''): hash.update(chunk)
plist_bytes = (work / 'Erlkonig.app/Contents/Info.plist').read_bytes()
plist = plistlib.loads(plist_bytes)
receipt = {
    'schema': 1, 'signing': 'developer-id-notarized',
    # This bundle is the exact input that package-dmg verifies against its mounted payload.
    'package_version': plist['CFBundleShortVersionString'], 'bundle_build': plist['CFBundleVersion'],
    'bundle_plist_sha256': hashlib.sha256(plist_bytes).hexdigest(),
    'artifact_sha256': hash.hexdigest(), 'artifact_size': artifact.stat().st_size,
    'source_commit': sys.argv[3], 'run_id': int(sys.argv[4]), 'run_attempt': int(sys.argv[5]),
    'notarization': json.loads((work / 'notarization.json').read_text()),
    'codesign_display': (work / 'signature.txt').read_text(),
    'stapler_validation': {'exit_code': 0, 'output': (work / 'stapler-validation.txt').read_text()},
}
(work / 'release-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
PYRECEIPT
# Same filesystem; atomic no-clobber publication even if another run raced us.
ln "$dmg" "$destination"
ln "$work/release-receipt.json" "$destination.receipt.json"
printf 'release-dmg: published %s\n' "$destination"
shasum -a 256 "$destination"
