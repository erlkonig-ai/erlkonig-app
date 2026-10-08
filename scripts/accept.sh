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
/usr/bin/python3 - "$app/Contents/Resources/plugin" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
for provider in (".codex-plugin", ".claude-plugin"):
    manifest = json.loads((root / provider / "plugin.json").read_text())
    assert manifest["name"] == "erlkonig"
assert (root / "skills/faculties/SKILL.md").is_file()
assert not (root / ".mcp.json").exists(), "Runtime wiring must be generated for this app path"
assert not any(p.name in ("mcp.token", "self.key", "self.pile") for p in root.rglob("*"))
PY
file "$app/Contents/MacOS/erlkonig"
shasum -a 256 "$app/Contents/MacOS/erlkonig"
du -sh "$app"
printf 'PASS: one signed executable; provider templates/shared skill; no LaunchAgent or packaged credential\n'
