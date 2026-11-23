#!/bin/sh
set -eu
module=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
python3 "$module/prepare-sources.py"
cd "$module/native"
target=${CARGO_TARGET_DIR:-"$module/build/rust"}
mkdir -p "$target"
export CARGO_TARGET_DIR=$(cd "$target" && pwd)
cargo build --locked -p erlkonig-ios --lib --target aarch64-apple-ios-sim
xcodebuild -project "$module/app/ErlkonigPhone.xcodeproj" -scheme ErlkonigPhone \
    -configuration Debug -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
    -derivedDataPath "$module/build/xcode" ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
    CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
    RUST_LIBRARY_DIR="$CARGO_TARGET_DIR/aarch64-apple-ios-sim/debug" build
