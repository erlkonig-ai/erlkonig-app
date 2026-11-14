#!/bin/sh
# Install Erlkonig for the current user from an unpacked release tarball:
#   ./install-user.sh              install or update
#   ./install-user.sh --uninstall  remove the program, menu entry, icons and
#                                  autostart entry (never the data directory)
# No root, no system files: ~/.local/bin, ~/.local/share/applications and
# ~/.local/share/icons/hicolor and the Erlkonig notice directory. Desktops pick
# up new entries and icons from these folders; no cache is written.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
: "${HOME:?HOME is not set}"
bin_dir="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
app_id=ai.erlkonig.app
bin="$bin_dir/erlkonig"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin" "$data/applications/$app_id.desktop" "$config/autostart/$app_id.desktop"
    for icon in "$data"/icons/hicolor/*/apps/"$app_id".png; do
        [ -e "$icon" ] && rm -f "$icon"
    done
    rm -rf "$data/erlkonig-notices"
    echo "Removed Erlkonig. Its data directory ($data/erlkonig) was left in place."
    exit 0
fi

[ -x "$here/erlkonig" ] || { echo "erlkonig binary not found next to $0" >&2; exit 1; }

[ -f "$here/notices/manifest.json" ] && [ -f "$here/notices/README.txt" ] || { echo "install: missing prepared notices" >&2; exit 1; }

mkdir -p "$bin_dir" "$data/applications"
notice_stage="$data/.erlkonig-notices.partial-$$"
trap 'rm -rf "$notice_stage"' EXIT
cp -R "$here/notices" "$notice_stage"
rm -rf "$data/erlkonig-notices"
mv "$notice_stage" "$data/erlkonig-notices"
trap - EXIT
# Replace atomically, so a running copy keeps its executable.
cp "$here/erlkonig" "$bin.partial"
chmod 0755 "$bin.partial"
mv -f "$bin.partial" "$bin"

for size_dir in "$here"/icons/*x*; do
    size=$(basename "$size_dir")
    mkdir -p "$data/icons/hicolor/$size/apps"
    cp "$size_dir/apps/$app_id.png" "$data/icons/hicolor/$size/apps/$app_id.png"
done

case "$bin" in
    *[!A-Za-z0-9/._+,@-]*) exec_line="\"$bin\"" ;;
    *) exec_line="$bin" ;;
esac
sed "s|@EXEC@|$exec_line|" "$here/$app_id.desktop.in" > "$data/applications/$app_id.desktop.partial"
mv -f "$data/applications/$app_id.desktop.partial" "$data/applications/$app_id.desktop"

echo "Installed $bin and the Erlkonig menu entry."
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "Note: $bin_dir is not on PATH; the menu entry works regardless." ;;
esac
echo "Start it from the application menu, or run: $bin &"
