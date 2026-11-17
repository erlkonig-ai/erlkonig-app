#!/bin/bash
# Reuse retained AppRun, stage, pinned tool acquisition and static inspection.
set -euo pipefail
app=$(cd "$(dirname "$0")/../.." && pwd -P)
target=${1:?native Linux target required}; output=${2:?existing output directory required}
cd "$app"
case "$target" in
  aarch64-unknown-linux-gnu) arch=aarch64 ;;
  x86_64-unknown-linux-gnu) arch=x86_64 ;;
  *) exit 1 ;;
esac
[[ "$(uname -s)-$(uname -m)" == "Linux-$arch" ]]
: "${RUNNER_TEMP:?}" "${CARGO_TARGET_DIR:?}"
export ERLKONIG_APP_ROOT="$app" APPIMAGE_WORK_ROOT="$RUNNER_TEMP/appimage-package"
mkdir "$APPIMAGE_WORK_ROOT"
mkdir "$APPIMAGE_WORK_ROOT/evidence"
assets="$app/scripts/linux/appimage"
python3 "$assets/scripts/prepare-tools.py" "$arch"
sh "$assets/scripts/stage-appdir.sh"
tools="$APPIMAGE_WORK_ROOT/tools/$arch"
chmod 700 "$tools/appimagetool-$arch.AppImage"
(cd "$tools"; "./appimagetool-$arch.AppImage" --appimage-extract > "$APPIMAGE_WORK_ROOT/evidence/tool-extract.txt")
version=$(python3 -c 'import pathlib,tomllib;print(tomllib.loads(pathlib.Path("Cargo.toml").read_text())["workspace"]["package"]["version"])')
package="$output/erlkonig-$version-$target.AppImage"
[[ ! -e "$package" ]]
ARCH="$arch" nice -n 10 "$tools/squashfs-root/AppRun" --no-appstream \
  --mksquashfs-opt=-processors --mksquashfs-opt=2 --runtime-file "$tools/runtime-$arch" \
  "$APPIMAGE_WORK_ROOT/AppDir" "$package"
python3 "$assets/scripts/verify-package.py" --root "$APPIMAGE_WORK_ROOT" --arch "$arch" --package "$package"
cp "$APPIMAGE_WORK_ROOT/evidence/package-inspection.json" "$output/package-inspection.json"
cp "$APPIMAGE_WORK_ROOT/evidence/tool-downloads-$arch.json" "$output/appimage-tool-inputs.json"
