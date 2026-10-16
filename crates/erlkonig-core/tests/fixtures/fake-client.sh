#!/bin/sh
# A fake `claude` or `codex` plugin CLI for tests. It appends its argv to
# argv.log next to itself (fields end in \037, one line per call) and keeps
# its plugin state in files beside it. It never deletes anything. A file
# named `fail` beside it makes every mutating command fail.
D=${0%/*}
N=${0##*/}
{
    for a in "$@"; do printf '%s\037' "$a"; done
    printf '\n'
} >>"$D/argv.log"
installed=0
[ -f "$D/installed" ] && read -r installed <"$D/installed"
root=
[ -f "$D/market" ] && read -r root <"$D/market"
mutate() {
    [ -f "$D/fail" ] && exit 3
    printf '%s\n' "$2" >"$D/$1"
}
case "$N:$*" in
claude:"plugin list --json")
    if [ "$installed" = 1 ]; then
        printf '[{"id":"erlkonig@erlkonig-local","scope":"user","enabled":true}]'
    else
        printf '[]'
    fi
    ;;
codex:"--disable remote_plugin plugin list --marketplace erlkonig-local --json")
    if [ "$installed" = 1 ]; then
        printf '{"installed":[{"pluginId":"erlkonig@erlkonig-local","installed":true,"enabled":true}]}'
    else
        printf '{"installed":[]}'
    fi
    ;;
claude:"plugin marketplace list --json")
    if [ -n "$root" ]; then
        printf '[{"name":"erlkonig-local","source":"directory","path":"%s","installLocation":"%s"}]' "$root" "$root"
    else
        printf '[]'
    fi
    ;;
codex:"plugin marketplace list --json")
    if [ -n "$root" ]; then
        printf '{"marketplaces":[{"name":"erlkonig-local","root":"%s","marketplaceSource":{"sourceType":"local","source":"%s"}}]}' "$root" "$root"
    else
        printf '{"marketplaces":[]}'
    fi
    ;;
*:"plugin marketplace add "*) mutate market "$4" ;;
claude:"plugin install erlkonig@erlkonig-local --scope user") mutate installed 1 ;;
codex:"plugin add erlkonig@erlkonig-local --json") mutate installed 1 ;;
claude:"plugin uninstall erlkonig@erlkonig-local --scope user") mutate installed 0 ;;
claude:"plugin uninstall erlkonig@erlkonig-local --scope user --keep-data") mutate installed 0 ;;
codex:"plugin remove erlkonig@erlkonig-local --json") mutate installed 0 ;;
*) exit 2 ;;
esac
