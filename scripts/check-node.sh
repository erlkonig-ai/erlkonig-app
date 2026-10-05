#!/bin/sh
# Reports what the Erlkonig node is doing on this Mac. Read-only: it changes
# nothing, starts nothing and stops nothing, so it is safe to run over ssh at
# any time.
#
# Usage: scripts/check-node.sh [--data-dir D] [--lines N]
#
#   --data-dir  the node's data directory (default: $ERLKONIG_DATA_DIR, else
#               ~/Library/Application Support/Erlkonig)
#   --lines     how many recent lines to show from each log (default 20)
#
# Sections: the LaunchAgent as launchd sees it, the D/bin link and the bundle
# it points into, D/node.status, the node's process tree, and recent log lines.
set -u

data=${ERLKONIG_DATA_DIR:-$HOME/Library/Application Support/Erlkonig}
lines=20
while [ $# -gt 0 ]; do
    case $1 in
        --data-dir) [ $# -ge 2 ] || { echo "check-node: $1 needs a value" >&2; exit 2; }; data=$2; shift 2 ;;
        --lines) [ $# -ge 2 ] || { echo "check-node: $1 needs a value" >&2; exit 2; }; lines=$2; shift 2 ;;
        -h | --help) sed -n '2,14p' "$0"; exit 0 ;;
        *) echo "check-node: unknown argument: $1" >&2; exit 2 ;;
    esac
done

label=ai.erlkonig.node
section() { printf '\n== %s\n' "$*"; }

section "host"
printf '%s  %s  uid %s  macOS %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(hostname -s)" "$(id -u)" "$(sw_vers -productVersion 2>/dev/null)"
printf 'data directory: %s\n' "$data"

section "LaunchAgent (launchctl print gui/$(id -u)/$label)"
if out=$(launchctl print "gui/$(id -u)/$label" 2>&1); then
    # The full dump is long; these are the lines that say whether it runs.
    printf '%s\n' "$out" | grep -E '^[[:space:]]*(state|pid|program|path|last exit code|runs|active count|job state|spawn type|properties|bundle id|parent bundle identifier|parent bundle version) =' \
        || printf '%s\n' "$out" | head -40
else
    printf 'not loaded: %s\n' "$out" | head -3
    echo "(Open at login is off, or macOS is still waiting for approval in System Settings > General > Login Items & Extensions.)"
fi

section "D/bin link and bundle"
if [ -L "$data/bin" ]; then
    target=$(readlink "$data/bin")
    printf 'bin -> %s\n' "$target"
    app=${target%/Contents/MacOS}
    case $app in
        */AppTranslocation/*) echo "WARNING: the link points into an App-Translocated copy" ;;
    esac
    if [ -d "$app" ]; then
        codesign --verify --deep --strict "$app" 2>&1 && echo "codesign --verify --deep --strict: ok"
        grep -E '^(built|app|faculties version|trible version) ' "$app/Contents/Resources/provenance.txt" 2>/dev/null
    else
        echo "the bundle the link points into is gone: $app"
    fi
else
    echo "no bin link yet (the menu-bar app makes it on first launch)"
fi

section "node.status"
status=$data/node.status
if [ -f "$status" ]; then
    now=$(date +%s)
    /usr/bin/python3 - "$status" "$now" <<'PY' 2>/dev/null || cat "$status"
import json, sys
s = json.load(open(sys.argv[1])); now = int(sys.argv[2])
age = now - s.get("updated_at", 0)
print(f"state {s.get('state')}  pid {s.get('pid')}  written {age}s ago ({s.get('updated')})")
print(f"node id {s.get('node_id')}")
print(f"pile {s.get('pile')}\nkey  {s.get('key')}")
print(f"peers {s.get('configured_peers')}  sync collections {s.get('sync_collections')}  maintain targets {s.get('maintain_targets')}")
if s.get("problem"):
    print(f"PROBLEM: {s['problem']}")
for c in s.get("children", []):
    print(f"child {c['name']}: {c['state']} pid {c.get('pid')} restarts {c.get('restarts')} "
          f"last_exit {c.get('last_exit')} last_error {c.get('last_error')} next_start_in {c.get('next_start_in_secs')}")
if not s.get("children"):
    print("children: none (node.toml configures no sync_collections or maintain_targets)")
for p in s.get("peers", []):
    print(f"peer {p['peer']}: {p['collections']} collections, {now - p['at']}s ago")
if s.get("state") == "running" and age > 20:
    print("WARNING: status is stale (older than 20 s); the node may be gone or hung")
PY
else
    echo "no node.status (the node has never run with this data directory)"
fi

section "processes"
pid=
[ -f "$status" ] && pid=$(sed -n 's/^ *"pid": *\([0-9][0-9]*\),*$/\1/p' "$status" | head -1)
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    ps -o pid,ppid,pgid,etime,rss,command -p "$pid"
    kids=$(pgrep -P "$pid" | tr '\n' ',' | sed 's/,$//')
    if [ -n "$kids" ]; then
        ps -o pid,ppid,pgid,etime,rss,command -p "$kids" | sed 1d
    else
        echo "(no child processes)"
    fi
else
    echo "the node recorded in node.status (pid ${pid:-none}) is not running"
fi
echo "every Erlkonig process of this user (matched by executable, not by argv text):"
ours() {
    ps -U "$(id -u)" -o pid=,comm= | while read -r p c; do
        case $c in
            (*Erlkonig.app/Contents/MacOS/* | erlkonig | erlkonig-node | */erlkonig | */erlkonig-node) printf '%s,' "$p" ;;
            (*"Application Support/Erlkonig/bin/"*) printf '%s,' "$p" ;;
        esac
    done
}
mine=$(ours)
if [ -n "$mine" ]; then
    ps -o pid,ppid,etime,command -p "${mine%,}"
else
    echo "(none)"
fi

section "recent log lines ($lines per log)"
if [ -d "$data/logs" ]; then
    for f in "$data/logs/"*.log; do
        [ -f "$f" ] || continue
        printf -- '-- %s (%s bytes)\n' "${f##*/}" "$(wc -c <"$f" | tr -d ' ')"
        tail -n "$lines" "$f"
    done
else
    echo "no logs directory"
fi
