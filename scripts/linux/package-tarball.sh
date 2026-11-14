#!/bin/sh
# Build the release binary and icons and pack them with the user installer:
#   scripts/linux/package-tarball.sh [OUTPUT_DIRECTORY]   (default: dist/)
# Produces erlkonig-<version>-linux-<arch>.tar.gz and its .sha256.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/dist}
target=${CARGO_TARGET_DIR:-$root/target}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
name="erlkonig-$version-linux-$(uname -m)"

[ -f "$root/notices/manifest.json" ] && [ -f "$root/notices/README.txt" ] || { echo "package: missing prepared notices" >&2; exit 1; }

cargo build --release --locked -p erlkonig-linux --manifest-path "$root/Cargo.toml"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
cp "$target/release/erlkonig-linux" "$stage/$name/erlkonig"
cargo run --release --locked -p erlkonig-app --example linux-icons \
    --manifest-path "$root/Cargo.toml" -- "$stage/$name/icons"
cp -R "$root/notices" "$stage/$name/notices"
cp "$root/scripts/linux/install-user.sh" "$stage/$name/install-user.sh"
cp "$root/scripts/linux/ai.erlkonig.app.desktop.in" "$stage/$name/"
chmod 0755 "$stage/$name/install-user.sh" "$stage/$name/erlkonig"

mkdir -p "$out"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
