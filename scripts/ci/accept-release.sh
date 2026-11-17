#!/usr/bin/env bash
# Fixed artifact mapping; no event-supplied path, command or signing flag.
set -euo pipefail
app=$(cd "$(dirname "$0")/../.." && pwd -P)
site=${1:?site checkout required}
target=${2:?target required}
output=${3:?build output required}
packages=${4:?new package transfer directory required}
: "${RUNNER_TEMP:?}" "${GITHUB_RUN_ID:?}" "${GITHUB_RUN_ATTEMPT:?}"
mkdir "$packages"
commit=$(git -C "$app" rev-parse HEAD)
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
    set -- "$output/"*.AppImage
    test "$#" -eq 1 && test -f "$1"
    cp "$1" "$packages/"
    ;;
  aarch64-apple-darwin)
    : "${MAC_SIGN_IDENTITY:?accepted existing Developer ID identity required}"
    : "${MAC_NOTARY_PROFILE:?accepted existing keychain profile required}"
    sh "$app/scripts/release-dmg.sh" --app "$output/Erlkonig.app" --out "$packages" \
      --identity "$MAC_SIGN_IDENTITY" --notary-profile "$MAC_NOTARY_PROFILE" \
      --source-commit "$commit" --run-id "$GITHUB_RUN_ID" --run-attempt "$GITHUB_RUN_ATTEMPT"
    ;;
  *) exit 1 ;;
esac
python3 - "$packages" "$target" > "$packages/record.json" <<'PY'
import json,pathlib,sys
root=pathlib.Path(sys.argv[1]); target=sys.argv[2]
mac=target=='aarch64-apple-darwin'
files=list(root.glob('*.dmg' if mac else '*.AppImage'))
assert len(files)==1
name=files[0].name
record={'target':target,'file':name,'signing':'developer-id-notarized' if mac else 'unsigned','acceptance_file':name+'.acceptance.json'}
if mac: record['signing_file']=name+'.receipt.json'
print(json.dumps(record))
PY
artifact=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["file"])' "$packages/record.json")
python3 "$site/scripts/record_native_acceptance.py" \
  --artifact "$packages/$artifact" --target "$target" --source-root "$app" \
  --source-commit "$commit" --run-id "$GITHUB_RUN_ID" --run-attempt "$GITHUB_RUN_ATTEMPT" \
  --output "$packages/$artifact.acceptance.json" -- \
  python3 "$app/scripts/ci/probe_package.py" --artifact "$packages/$artifact" --target "$target" \
  --app "$app" --fixture "$RUNNER_TEMP/app-target/release/examples/accept-fixture" \
  --root "$RUNNER_TEMP/release-runtime"
