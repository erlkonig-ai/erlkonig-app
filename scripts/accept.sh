#!/bin/sh
# Headless acceptance tests against the assembled bundle's own binaries.
# Everything lives under ~/Erlkonig/build/accept; only PIDs started here are
# signalled. Your real ~/.codex and ~/.claude are checksummed before and after
# and never written: Codex and Claude run with a scratch HOME, CODEX_HOME and
# CLAUDE_CONFIG_DIR. Usage: scripts/accept.sh [APP]
set -u
B=$HOME/Erlkonig/build
HELPERS=$(CDPATH= cd -- "$(dirname -- "$0")/accept" && pwd -P)
APP=${1:-$B/dist/Erlkonig.app}
M=$APP/Contents/MacOS
A=$B/accept
CODEX=/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex
CLAUDE=$HOME/.local/bin/claude
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
for v in $(env | grep -o '^TRIBLESPACE_[A-Za-z0-9_]*'); do unset "$v"; done
unset PILE PERSONA ERLKONIG_DATA_DIR ERLKONIG_BIN_DIR
rm -rf "$A"; mkdir -p "$A"
run() { printf '\n$ %s\n' "$*"; "$@"; printf '(rc=%s)\n' "$?"; }
hdr() { printf '\n######## %s\n' "$*"; }
wait_for() { # seconds, shell condition
    n=0; while [ $n -lt $(($1 * 5)) ]; do eval "$2" && return 0; sleep 0.2; n=$((n + 1)); done; return 1
}
status_field() { /usr/bin/python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$@"; }

hdr "env"
echo "TRIBLESPACE_* count: $(env | grep -c '^TRIBLESPACE_')  PILE=${PILE-unset} PERSONA=${PERSONA-unset}"

hdr "real config checksums, before"
shasum -a 256 "$HOME/.codex/config.toml" "$HOME/.claude/settings.json" "$HOME/.claude/plugins/known_marketplaces.json" "$HOME/.claude/plugins/installed_plugins.json" | tee "$A/real-before.txt"
ls -d "$HOME/.agents" "$HOME/Library/Application Support/Erlkonig" 2>&1
launchctl print "gui/$(id -u)/ai.erlkonig.node" >/dev/null 2>&1 && echo "ai.erlkonig.node IS loaded" || echo "ai.erlkonig.node not loaded"

hdr "A. bundle"
run codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv "$APP" 2>&1 | grep -E '^(Identifier|Format|Signature|TeamIdentifier)'
for f in erlkonig erlkonig-node faculties trible; do
    codesign -dv "$M/$f" 2>&1 | grep -E '^Identifier'
done
run plutil -lint "$APP/Contents/Info.plist" "$APP/Contents/Library/LaunchAgents/ai.erlkonig.node.plist"
plutil -extract LSUIElement raw "$APP/Contents/Info.plist"; plutil -extract CFBundleIdentifier raw "$APP/Contents/Info.plist"
AP=$APP/Contents/Library/LaunchAgents/ai.erlkonig.node.plist
echo "agent: ProgramArguments $(plutil -extract ProgramArguments json -o - "$AP")  KeepAlive $(plutil -extract KeepAlive json -o - "$AP")  ProcessType $(plutil -extract ProcessType raw "$AP")"
du -sh "$APP"; du -sk "$APP"
ls -l "$M"
shasum -a 256 "$M"/*
file "$M"/* | sed "s|$M/||"
"$M/erlkonig-node" --version; "$M/faculties" --version; "$M/trible" --version
cat "$APP/Contents/Resources/provenance.txt"
find "$APP/Contents/Resources/plugin" -type f | sed "s|$APP/||" | sort
[ -e "$APP/Contents/Resources/plugin/bin" ] && echo "plugin HAS bin/" || echo "plugin has no bin/"

hdr "B. node in accept/t1"
T1=$A/t1
env ERLKONIG_DATA_DIR="$T1" nohup "$M/erlkonig-node" run >"$A/t1-run1.err" 2>&1 </dev/null &
P1=$!; echo "node pid $P1"
wait_for 15 "[ -f '$T1/node.status' ] && grep -q '\"node_id\": \"' '$T1/node.status'" && echo "status written"
ls -la "$T1" | sed 1d
echo "status: state=$(status_field "$T1/node.status" 's["state"]') pid=$(status_field "$T1/node.status" 's["pid"]') node_id=$(status_field "$T1/node.status" 's["node_id"]') children=$(status_field "$T1/node.status" 'len(s["children"])')"
run env ERLKONIG_DATA_DIR="$T1" "$M/erlkonig-node" id
run env ERLKONIG_DATA_DIR="$T1" "$M/erlkonig-node" status
KEYSHA1=$(shasum -a 256 "$T1/self.key" | cut -d' ' -f1); echo "key sha256 $KEYSHA1"
run env ERLKONIG_DATA_DIR="$T1" "$M/erlkonig-node" run
kill -TERM "$P1"; wait "$P1"; echo "node exit after SIGTERM: $?"
echo "status after stop: state=$(status_field "$T1/node.status" 's["state"]')"
echo "children of $P1 left: $(pgrep -P "$P1" | wc -l | tr -d ' ')"
env ERLKONIG_DATA_DIR="$T1" nohup "$M/erlkonig-node" run >"$A/t1-run2.err" 2>&1 </dev/null &
P2=$!; echo "node pid $P2 (restart)"
wait_for 15 "grep -q '\"pid\": $P2,' '$T1/node.status'" && echo "status written by $P2"
echo "key sha256 after restart $(shasum -a 256 "$T1/self.key" | cut -d' ' -f1) (same: $([ "$(shasum -a 256 "$T1/self.key" | cut -d' ' -f1)" = "$KEYSHA1" ] && echo yes || echo NO))"
echo "'created a signing key' lines in node.log: $(grep -c 'created a signing key' "$T1/logs/node.log")"
echo "'created an empty pile' lines in node.log: $(grep -c 'created an empty pile' "$T1/logs/node.log")"
echo "-- a LaunchAgent-style node (run --launchd) waits for the lock instead of exiting"
nohup "$M/erlkonig-node" run --launchd --data-dir "$T1" >"$A/t1-launchd.err" 2>&1 </dev/null &
PW=$!; echo "waiting node pid $PW"
sleep 4
kill -0 "$PW" 2>/dev/null && echo "after 4 s: $PW still running (waiting)" || echo "after 4 s: $PW EXITED"
echo "status still written by $(status_field "$T1/node.status" 's["pid"]') (holder $P2)"
grep "waiting for it" "$T1/logs/node.log" | tail -1
kill -TERM "$P2"; wait "$P2"; echo "holder exit after SIGTERM: $?"
wait_for 15 "grep -q '\"pid\": $PW,' '$T1/node.status' && grep -q '\"state\": \"running\"' '$T1/node.status'" && echo "status now written by $PW: it took over"
grep -E "took over|LaunchAgent" "$T1/logs/node.log" | tail -2
kill -TERM "$PW"; wait "$PW"; echo "node exit after SIGTERM: $?"
echo "-- node.log"; cat "$T1/logs/node.log"

hdr "C. maintenance child in accept/t2"
T2=$A/t2; mkdir -p "$T2/D"
run "$M/trible" pile create "$T2/throwaway.pile"
run "$M/trible" pile signing-key init "$T2/throwaway.pile" --key "$T2/throwaway.key"
H=$("$M/trible" pile collection init "$T2/throwaway.pile" throwaway --key "$T2/throwaway.key" 2>"$A/t2-init.err" | grep -o 'blake3:[0-9a-f]\{64\}' | head -1)
echo "handle: $H"; cat "$A/t2-init.err"
cat >"$T2/D/node.toml" <<EOF
pile = "$T2/throwaway.pile"
key = "$T2/throwaway.key"
maintain_targets = ["$H"]
EOF
cat "$T2/D/node.toml"
env ERLKONIG_DATA_DIR="$T2/D" nohup "$M/erlkonig-node" run >"$A/t2-run.err" 2>&1 </dev/null &
P3=$!; echo "node pid $P3"
child_pid() { status_field "$T2/D/node.status" '[c.get("pid") for c in s["children"] if c["name"]=="maintain"][0]' 2>/dev/null; }
for i in 1 2 3; do
    wait_for 30 "c=\$(child_pid); [ -n \"\$c\" ] && [ \"\$c\" != None ] && [ \"\$c\" != \"\${last:-}\" ]" || echo "child did not (re)start"
    c=$(child_pid); last=$c
    echo "round $i: maintain child pid $c, parent $(ps -o ppid= -p "$c" | tr -d ' '), pgid $(ps -o pgid= -p "$c" | tr -d ' ')"
    ps -o command= -p "$c" | sed "s|$M/||"
    kill -KILL "$c"
done
wait_for 30 "[ \"\$(status_field '$T2/D/node.status' '[c[\"restarts\"] for c in s[\"children\"]][0]')\" = 3 ] && c=\$(child_pid) && [ \"\$c\" != None ] && [ \"\$c\" != \"\$last\" ]"
status_field "$T2/D/node.status" 'json.dumps(s["children"], indent=1)' 2>/dev/null || status_field "$T2/D/node.status" 's["children"]'
echo "-- maintain.log (erlkonig lines)"; grep -E 'killed|next start|exited|starting' "$T2/D/logs/maintain.log" | tail -12
C=$(child_pid)
kill -TERM "$P3"; wait "$P3"; echo "node exit after SIGTERM: $?"
kill -0 "$C" 2>/dev/null && echo "child $C STILL RUNNING" || echo "child $C gone"
echo "status after stop: state=$(status_field "$T2/D/node.status" 's["state"]')"

echo "-- a node killed with SIGKILL leaves its child; the next node stops it, even when the dead node's pid is reused"
env ERLKONIG_DATA_DIR="$T2/D" nohup "$M/erlkonig-node" run >"$A/t2-run2.err" 2>&1 </dev/null &
P4=$!; echo "node pid $P4"
wait_for 30 "c=\$(child_pid); [ -n \"\$c\" ] && [ \"\$c\" != None ] && grep -q '\"pid\": $P4,' '$T2/D/node.status'"
C=$(child_pid); echo "maintain child pid $C"
kill -KILL "$P4"; wait "$P4" 2>/dev/null
sleep 0.5
echo "after SIGKILL of the node: child $C $(kill -0 "$C" 2>/dev/null && echo "still runs, parent $(ps -o ppid= -p "$C" | tr -d ' ')" || echo gone)"
/bin/sleep 120 &
REUSED=$!
/usr/bin/python3 -c 'import json, sys; p, pid = sys.argv[1], int(sys.argv[2]); s = json.load(open(p)); s["pid"] = pid; json.dump(s, open(p, "w"))' "$T2/D/node.status" "$REUSED"
echo "node.status now names pid $REUSED (a live sleep) as its owner"
env ERLKONIG_DATA_DIR="$T2/D" nohup "$M/erlkonig-node" run >"$A/t2-run3.err" 2>&1 </dev/null &
P5=$!; echo "node pid $P5"
wait_for 30 "c=\$(child_pid); [ -n \"\$c\" ] && [ \"\$c\" != None ] && [ \"\$c\" != $C ] && grep -q '\"pid\": $P5,' '$T2/D/node.status'"
grep "stale" "$T2/D/logs/node.log" | tail -2
kill -0 "$C" 2>/dev/null && echo "old child $C STILL RUNNING" || echo "old child $C gone"
echo "maintain-all processes on this pile: $(ps -U "$(id -u)" -o command= | grep -c "[m]aintain-all $T2/throwaway.pile")"
kill -0 "$REUSED" 2>/dev/null && echo "pid $REUSED (the reused owner pid) untouched" || echo "pid $REUSED WAS KILLED"
kill "$REUSED"; wait "$REUSED" 2>/dev/null
kill -TERM "$P5"; wait "$P5"; echo "node exit after SIGTERM: $?"
echo "-- node.log"; cat "$T2/D/logs/node.log"

hdr "C2. a pile named in node.toml that appears later (a disk mounted after login) is picked up without an edit"
T3=$A/t3; mkdir -p "$T3/D"
mv "$T2/throwaway.pile" "$T2/throwaway.pile.away"
printf 'pile = "%s"\nkey = "%s"\nmaintain_targets = ["%s"]\n' "$T2/throwaway.pile" "$T2/throwaway.key" "$H" >"$T3/D/node.toml"
cat "$T3/D/node.toml"
env ERLKONIG_DATA_DIR="$T3/D" nohup "$M/erlkonig-node" run --retry-secs 2 >"$A/t3-run.err" 2>&1 </dev/null &
P6=$!; echo "node pid $P6"
wait_for 15 "grep -q 'does not exist' '$T3/D/node.status' 2>/dev/null" && echo "problem: $(status_field "$T3/D/node.status" 's["problem"]')"
sleep 5
[ -e "$T2/throwaway.pile" ] && echo "named pile WAS CREATED" || echo "after 5 s of retries the named pile is still not created"
mv "$T2/throwaway.pile.away" "$T2/throwaway.pile"; echo "pile moved back"
picked() { [ "$(status_field "$T3/D/node.status" 's["problem"]')" = None ] && [ "$(status_field "$T3/D/node.status" '[c["state"] for c in s["children"]]')" = "['running']" ]; }
wait_for 15 picked && echo "picked up: problem cleared, maintain running"
kill -TERM "$P6"; wait "$P6"; echo "node exit after SIGTERM: $?"
echo "-- node.log"; cat "$T3/D/logs/node.log"

hdr "D. plugin MCP command, as .mcp.json declares it"
HM=$A/home-mcp; mkdir -p "$HM"
echo "-- without the app link"
/usr/bin/python3 "$HELPERS/mcp_drive.py" "$APP/Contents/Resources/plugin" "$HM"
mkdir -p "$HM/Library/Application Support/Erlkonig" && chmod 700 "$HM/Library/Application Support/Erlkonig"
ln -s "$M" "$HM/Library/Application Support/Erlkonig/bin"
echo "-- with bin -> $M"
/usr/bin/python3 "$HELPERS/mcp_drive.py" "$APP/Contents/Resources/plugin" "$HM"
D=$HM/Library/Application\ Support/Erlkonig
ls -la "$D" | sed 1d
KS=$(shasum -a 256 "$D/self.key" | cut -d' ' -f1)
echo "-- second run"
/usr/bin/python3 "$HELPERS/mcp_drive.py" "$APP/Contents/Resources/plugin" "$HM" | grep -E '^(exit|tools/list|stderr)'
echo "key reused: $([ "$(shasum -a 256 "$D/self.key" | cut -d' ' -f1)" = "$KS" ] && echo yes || echo NO)"
echo "-- environment and argv faculties sees (planted TRIBLESPACE_KEY, PERSONA, PILE, DISCORD_TOKEN,"
echo "   and ERLKONIG_BIN_DIR / ERLKONIG_DATA_DIR pointing at a fake faculties and another folder)"
FAKE=$A/planted-bin; mkdir -p "$FAKE"
printf '#!/bin/sh\ntouch "%s/FAKE-FACULTIES-RAN"\nexit 1\n' "$A" >"$FAKE/faculties"; chmod 755 "$FAKE/faculties"
/usr/bin/python3 - "$APP/Contents/Resources/plugin" "$HM" "$FAKE" "$A/planted-data" <<'PY'
import json, os, subprocess, sys, time
plugin, home, fake, planted_data = sys.argv[1:5]
spec = json.load(open(os.path.join(plugin, ".mcp.json")))["mcpServers"]["faculties"]
env = {k: v for k, v in os.environ.items() if not k.startswith("TRIBLESPACE_")}
env.update(HOME=home, TRIBLESPACE_KEY="/planted", PERSONA="planted", PILE="/planted.pile", DISCORD_TOKEN="planted",
           ERLKONIG_BIN_DIR=fake, ERLKONIG_DATA_DIR=planted_data)
p = subprocess.Popen([spec["command"], *spec["args"]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd="/", text=True)
p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}) + "\n"); p.stdin.flush()
p.stdout.readline()
cmd = subprocess.run(["ps", "-ww", "-o", "command=", "-p", str(p.pid)], capture_output=True, text=True).stdout.strip()
envd = subprocess.run(["ps", "-E", "-ww", "-o", "command=", "-p", str(p.pid)], capture_output=True, text=True).stdout.strip()
print("pid", p.pid, "argv:", cmd)
names = sorted({w.split("=", 1)[0] for w in envd[len(cmd):].split() if "=" in w and w.split("=", 1)[0].isupper()})
print("environment variable names (ps -E):", " ".join(names) if names else "(ps -E showed none)")
print("planted values visible:", [n for n in ("TRIBLESPACE_KEY", "PERSONA", "PILE", "DISCORD_TOKEN") if n + "=" in envd] or "none")
p.stdin.close(); print("exit", p.wait(timeout=20))
PY
[ -e "$A/FAKE-FACULTIES-RAN" ] && echo "the fake faculties in ERLKONIG_BIN_DIR RAN" || echo "the fake faculties in ERLKONIG_BIN_DIR did not run"
[ -e "$A/planted-data" ] && echo "ERLKONIG_DATA_DIR folder WAS CREATED" || echo "ERLKONIG_DATA_DIR folder not created"

hdr "E. Claude (CLAUDE_CONFIG_DIR and HOME isolated)"
HC=$A/home-claude; CC=$A/claude-config; mkdir -p "$HC" "$CC"
mkdir -p "$HC/Library/Application Support/Erlkonig" && chmod 700 "$HC/Library/Application Support/Erlkonig"
ln -s "$M" "$HC/Library/Application Support/Erlkonig/bin"
cenv() { env HOME="$HC" CLAUDE_CONFIG_DIR="$CC" PATH="$HOME/.local/bin:$PATH" "$@"; }
# The install itself runs with launchd's bare PATH, as the menu app's would.
run env HOME="$HC" CLAUDE_CONFIG_DIR="$CC" PATH=/usr/bin:/bin "$M/erlkonig-node" install-claude --claude-code --claude "$CLAUDE"
Z="$HC/Library/Application Support/Erlkonig/erlkonig-claude-plugin.zip"
ls -l "$Z"; unzip -l "$Z"; unzip -tq "$Z"
run cenv "$CLAUDE" plugin validate "$APP/Contents/Resources/plugin"
run cenv "$CLAUDE" plugin list
run cenv "$CLAUDE" mcp list
echo "-- repeat install"
run cenv "$M/erlkonig-node" install-claude --claude-code --claude "$CLAUDE"

hdr "F. ChatGPT (HOME and CODEX_HOME isolated)"
HX=$A/home-codex; CX=$A/codex-home; mkdir -p "$HX" "$CX"
mkdir -p "$HX/Library/Application Support/Erlkonig" && chmod 700 "$HX/Library/Application Support/Erlkonig"
ln -s "$M" "$HX/Library/Application Support/Erlkonig/bin"
xenv() { env HOME="$HX" CODEX_HOME="$CX" "$@"; }
run "$CODEX" --version
run xenv "$M/erlkonig-node" install-chatgpt --codex "$CODEX"
run xenv "$CODEX" plugin list
run xenv "$CODEX" mcp list
xenv /usr/bin/python3 "$HELPERS/appserver_tools.py" "$CODEX" "$HX" "$CX"
echo "-- repeat install"
run xenv "$M/erlkonig-node" install-chatgpt --codex "$CODEX"

hdr "leftovers"
ps -U "$(id -u)" -o pid,command | grep -E "[E]rlkonig/build/(dist|accept)" || echo "no Erlkonig processes from this run"
# Sol's own codex app-server daemons live under ~/.codex and are not from this run.
ps -U "$(id -u)" -o pid,command | grep "[c]odex app-server" | grep -v "$HOME/.codex/" || echo "no codex app-server from this run"

hdr "real config checksums, after"
shasum -a 256 "$HOME/.codex/config.toml" "$HOME/.claude/settings.json" "$HOME/.claude/plugins/known_marketplaces.json" "$HOME/.claude/plugins/installed_plugins.json" | tee "$A/real-after.txt"
cmp -s "$A/real-before.txt" "$A/real-after.txt" && echo "real configs unchanged" || echo "REAL CONFIGS CHANGED"
ls -d "$HOME/.agents" "$HOME/Library/Application Support/Erlkonig" 2>&1
launchctl print "gui/$(id -u)/ai.erlkonig.node" >/dev/null 2>&1 && echo "ai.erlkonig.node IS loaded" || echo "ai.erlkonig.node not loaded"
echo "== accept done $(date -u +%FT%TZ)"
