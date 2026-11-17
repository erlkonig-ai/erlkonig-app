#!/bin/sh
set -eu
assets=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${APPIMAGE_WORK_ROOT:?fresh isolated packaging root}" "${ERLKONIG_APP_ROOT:?selected app source}"
root=$APPIMAGE_WORK_ROOT
: "${CARGO_TARGET_DIR:?set native build target directory}"
appdir="$root/AppDir"
[ ! -e "$appdir" ] || { echo 'AppDir already exists; preserve or explicitly replace it first.' >&2; exit 1; }
install -Dm755 "$CARGO_TARGET_DIR/release/erlkonig-linux" "$appdir/usr/bin/erlkonig"
install -m755 "$assets/packaging/AppRun" "$appdir/AppRun"
install -m644 "$assets/packaging/ai.erlkonig.app.desktop" "$appdir/ai.erlkonig.app.desktop"
"$CARGO_TARGET_DIR/release/examples/linux-icons" "$appdir/usr/share/icons/hicolor"
cp "$appdir/usr/share/icons/hicolor/256x256/apps/ai.erlkonig.app.png" "$appdir/ai.erlkonig.app.png"
ln -s ai.erlkonig.app.png "$appdir/.DirIcon"
mkdir -p "$appdir/usr/share/licenses/ai.erlkonig.app"
cp -R "$ERLKONIG_APP_ROOT/notices" "$appdir/usr/share/licenses/ai.erlkonig.app/"
install -m644 "$assets/packaging/AppImage-runtime-LICENSE.txt" "$appdir/usr/share/licenses/ai.erlkonig.app/AppImage-runtime-LICENSE.txt"
# All installed payload bytes must be readable from the image mount.
chmod -R a+rX "$appdir"
