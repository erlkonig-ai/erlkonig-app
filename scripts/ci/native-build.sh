#!/usr/bin/env bash
# Common source/build/package lane for development and trusted tag releases.
# Requires an ephemeral clean runner; output, Cargo cache and fixtures stay in
# RUNNER_TEMP, never inside source trees. No signing credential provisioning.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo"
target=${1:?native target required}
output=${2:?absolute new output directory required}
: "${RUNNER_TEMP:?ephemeral runner required}"
[[ "$output" = /* && ! -e "$output" ]]
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR="$RUNNER_TEMP/app-target" CARGO_HOME="$RUNNER_TEMP/app-cargo"
export GIT_TERMINAL_PROMPT=0 MACOSX_DEPLOYMENT_TARGET=14.0
unset PERSONA PILE DRIVE_ENDPOINT RUSTC_WRAPPER CARGO_ENCODED_RUSTFLAGS
for name in $(env | sed -n 's/^\(TRIBLESPACE_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$name"; done
python3 scripts/ci/release_inputs.py --app "$repo" --fetch-siblings
mkdir -p "$output"
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
    arch=${target%-unknown-linux-gnu}
    test "$(uname -s)-$(uname -m)" = "Linux-$arch"
    . /etc/os-release
    test "$ID-$VERSION_ID" = ubuntu-24.04
    package=erlkonig-linux
    if [ "$arch" = x86_64 ]; then export RUSTFLAGS='-C target-cpu=x86-64'; else export RUSTFLAGS='-C target-cpu=generic'; fi
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends build-essential pkg-config zlib1g-dev \
      libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxrandr2 libxi6 \
      libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-cursor0 \
      libwayland-egl1 libegl1 libgl1 libvulkan1 mesa-vulkan-drivers \
      xdg-utils dbus-x11 xvfb xauth lsof python3 file binutils squashfs-tools
    dpkg-query -W > "$output/native-packages.txt"
    ;;
  aarch64-apple-darwin)
    test "$(uname -s)-$(uname -m)" = Darwin-arm64
    package=erlkonig-mac
    python3 - "$output/native-packages.txt" <<'PY'
import json, pathlib, subprocess, sys
m=json.loads(pathlib.Path('ci/source-inputs.json').read_text())['targets']['macos']
assert m['xcode_developer_dir'] and m['xcode_version'], 'accepted Xcode selection is unresolved'
# A trusted signing runner must already select its SDK. Do not mutate a shared
# runner's Xcode selector as a side effect of a release.
assert subprocess.check_output(['xcode-select','-p'],text=True).strip()==m['xcode_developer_dir']
actual=subprocess.check_output(['xcodebuild','-version'],text=True).strip()
assert actual==m['xcode_version']
pathlib.Path(sys.argv[1]).write_text(actual+'\n')
PY
    ;;
  *) echo 'unsupported native target' >&2; exit 1 ;;
esac
if [ "$package" = erlkonig-linux ]; then
  : "${RETAINED_RUST_ROOT:?accepted native Rust1.98.0+wasm root required}"
  python3 scripts/ci/retained_toolchain.py --app "$repo" --target "$target" --root "$RETAINED_RUST_ROOT"
  export RUSTC="$RETAINED_RUST_ROOT/bin/rustc" RUSTDOC="$RETAINED_RUST_ROOT/bin/rustdoc"
  export PATH="$RETAINED_RUST_ROOT/bin:$PATH"
  cargo_cmd=("$RETAINED_RUST_ROOT/bin/cargo")
else
  rustup toolchain install 1.98.0 --profile minimal
  rustup target add --toolchain 1.98.0 "$target" wasm32-unknown-unknown
  cargo_cmd=(cargo +1.98.0)
fi
"${cargo_cmd[@]}" fetch --locked --target "$target"
if [ "$package" = erlkonig-mac ]; then
  "${cargo_cmd[@]}" fetch --locked --manifest-path scripts/app-icon/Cargo.toml
fi
{
  git rev-parse HEAD
  if [ "$package" = erlkonig-linux ]; then "$RUSTC" -vV; else rustc +1.98.0 -vV; fi
  printf 'runner-image=%s image-version=%s target=%s\n' "${ImageOS:-unrecorded}" "${ImageVersion:-unrecorded}" "$target"
  df -h . "$RUNNER_TEMP"
} > "$output/build-environment.txt"
nice -n 10 "${cargo_cmd[@]}" test --release --locked --offline \
  -p faculties-erp -p erlkonig-cli -p erlkonig-core -p erlkonig-app -p "$package" -- --test-threads=1
nice -n 10 "${cargo_cmd[@]}" build --release --locked --offline -p erlkonig-core --example accept-fixture
nice -n 10 "${cargo_cmd[@]}" build --release --locked --offline -p "$package"
if [ "$package" = erlkonig-linux ]; then
  nice -n 10 "${cargo_cmd[@]}" build --release --locked --offline -p erlkonig-app --example linux-icons
  CARGO_NET_OFFLINE=true bash scripts/linux/package-appimage.sh "$target" "$output"
  binary="$CARGO_TARGET_DIR/release/erlkonig-linux"
  file "$binary" > "$output/elf.txt"
  readelf -h "$binary" >> "$output/elf.txt"
  readelf --version-info "$binary" > "$output/symbol-versions.txt"
  ldd "$binary" > "$output/shared-libraries.txt"
  ! grep -q 'not found' "$output/shared-libraries.txt"
else
  CARGO_TARGET_DIR="$RUNNER_TEMP/app-icon-target" nice -n 10 "${cargo_cmd[@]}" build \
    --release --locked --offline --manifest-path scripts/app-icon/Cargo.toml
  sh scripts/bundle.sh --release-dir "$CARGO_TARGET_DIR/release" --out "$output" \
    --sign - --icon-renderer "$RUNNER_TEMP/app-icon-target/release/erlkonig-app-icon"
  sh scripts/accept.sh "$output/Erlkonig.app"
  test -s "$output/Erlkonig.app/Contents/Resources/notices/manifest.json"
  test -s "$output/Erlkonig.app/Contents/Resources/notices/README.txt"
  sh scripts/accept/dmg.sh "$output/Erlkonig.app"
  sh scripts/package-dmg.sh --app "$output/Erlkonig.app" --out "$output"
fi
cp ci/source-inputs.json "$output/source-inputs.json"
