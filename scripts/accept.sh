#!/bin/sh
# Read-only inventory of the selected one-executable candidate; never launches it.
set -eu
app=${1:?usage: scripts/accept.sh /path/to/Erlkonig.app}
codesign --verify --deep --strict "$app"
plutil -lint "$app/Contents/Info.plist"
[ -x "$app/Contents/MacOS/erlkonig" ]
[ "$(find "$app/Contents/MacOS" -type f | wc -l | tr -d ' ')" = 1 ]
[ ! -e "$app/Contents/Library/LaunchAgents" ]
[ ! -e "$app/Contents/Resources/plugin/bin" ]
/usr/bin/python3 - "$app/Contents/Resources/plugin/.mcp.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1])) == {"mcpServers": {}}
PY
file "$app/Contents/MacOS/erlkonig"
shasum -a 256 "$app/Contents/MacOS/erlkonig"
du -sh "$app"
printf 'PASS: one signed executable; no LaunchAgent or plugin launcher/credential\n'
