#!/usr/bin/env bash
# One scoped Mac build lane, including the ordinary reservation and cleanup.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
lock=${GB10_LOCK_HELPER:-$root/../mary/scripts/gb10-lock.sh}
host=${GB10_LOCK_HOST:-mac}
tag=erlkonig-native-app-$(date +%Y%m%d%H%M%S)-$$
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/Erlkonig/build/erlkonig-native-$tag}
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4
bash "$lock" check "$host"
df -h "$root"
available=$(memory_pressure -Q | awk '/free percentage/ {gsub(/%/,"",$NF); print $NF}')
[ "${available:-0}" -gt 20 ] || { echo 'At least 20% memory headroom is required' >&2; exit 1; }
bash "$lock" take "$host" "$tag"
heartbeat=
cleanup() {
    if [ -n "$heartbeat" ]; then kill "$heartbeat" 2>/dev/null || true; wait "$heartbeat" 2>/dev/null || true; fi
    bash "$lock" release "$host" "$tag"
}
trap cleanup EXIT INT TERM
(while true; do sleep 45; bash "$lock" refresh "$host" "$tag"; done) &
heartbeat=$!
cd "$root"
rustc --version
git rev-parse HEAD
git diff --binary | shasum -a 256
for d in faculties triblespace-rs anybytes jerky mary GORBIE cubecl-fork burn-fork cubek-fork soma; do
    printf '%s ' "$d"
    git -C "../$d" rev-parse HEAD
done
# Never inherit live collection/pile/persona test settings.
for name in $(env | sed -n 's/^\(TRIBLESPACE_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$name"; done
unset PILE PERSONA DRIVE_ENDPOINT RUSTFLAGS RUSTC_WRAPPER
nice -n 10 cargo build --release --locked --offline -p erlkonig-mac --example crown-render -j4
"$CARGO_TARGET_DIR/release/examples/crown-render" "$CARGO_TARGET_DIR/crown-render"
nice -n 10 cargo test --release --locked --offline -p erlkonig-core --lib --test lifecycle -j4 -- --test-threads=1
nice -n 10 cargo test --release --locked --offline -p erlkonig-app --lib -j4 -- --test-threads=1
nice -n 10 cargo test --release --locked --offline -p erlkonig-mac --bin erlkonig-mac -j4 -- --test-threads=1
nice -n 10 cargo build --release --locked --offline -p erlkonig-mac --bin erlkonig-mac -j4
