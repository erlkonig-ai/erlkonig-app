#!/usr/bin/env bash
# Validate this checkout and its pinned public dependencies on the local Mac.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
tag=erlkonig-native-app-$(date +%Y%m%d%H%M%S)-$$
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/$tag.XXXXXXXX")}
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
df -h "$CARGO_TARGET_DIR"
cd "$root"
rustc --version
git rev-parse HEAD
git diff --binary | shasum -a 256
for d in faculties triblespace-rs anybytes jerky mary GORBIE cubecl-fork burn-fork cubek-fork songbird; do
    printf '%s ' "$d"
    git -C "../$d" rev-parse HEAD
done
# Never inherit live collection/pile/persona test settings.
for name in $(env | sed -n 's/^\(TRIBLESPACE_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$name"; done
unset PILE PERSONA DRIVE_ENDPOINT RUSTFLAGS RUSTC_WRAPPER
nice -n 10 cargo build --release --locked --offline -p erlkonig-mac --example crown-render -j4
"$CARGO_TARGET_DIR/release/examples/crown-render" "$CARGO_TARGET_DIR/crown-render"
nice -n 10 cargo test --release --locked --offline -p faculties-erp -j4 -- --test-threads=1
nice -n 10 cargo test --release --locked --offline -p erlkonig-core --lib --test lifecycle -j4 -- --test-threads=1
nice -n 10 cargo test --release --locked --offline -p erlkonig-app --lib -j4 -- --test-threads=1
nice -n 10 cargo test --release --locked --offline -p erlkonig-mac --bin erlkonig-mac -j4 -- --test-threads=1
nice -n 10 cargo build --release --locked --offline -p erlkonig-mac --bin erlkonig-mac -j4
