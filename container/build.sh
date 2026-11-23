#!/bin/sh
set -eu

# An archive of this commit keeps working files, Git credentials and local
# node data out of the context sent to the builder.
repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
revision=$(git -C "$repo" rev-parse HEAD)
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
native=$(podman info --format '{{.Host.Arch}}')
arch=${2:-$native}
case "$arch" in
    amd64|arm64) ;;
    *) echo "Supported Linux architectures: amd64, arm64" >&2; exit 1 ;;
esac
git -C "$repo" archive --output="$context/source.tar" "$revision" -- \
    Cargo.toml Cargo.lock rust-toolchain.toml LICENSE-APACHE LICENSE-MIT \
    ci/overlays ci/source-inputs.json scripts/ci/release_inputs.py scripts/ci/soma_input.py \
    scripts/accept/fixture.rs scripts/linux/render-icons.rs crates \
    ios/native plugin notices container/Containerfile container/compile.sh
mkdir "$context/source"
tar -xf "$context/source.tar" -C "$context/source"
podman build --platform "linux/$arch" --build-arg "BUILDPLATFORM=linux/$native" \
    --build-arg "ERLKONIG_ARCH=$arch" --label "org.opencontainers.image.revision=$revision" \
    --file "$context/source/container/Containerfile" \
    --tag "${1:-localhost/erlkonig:local}" "$context/source"
