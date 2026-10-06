#!/usr/bin/env bash
# One scoped Mac build lane, including the ordinary reservation and cleanup.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
lock=/Users/jp/.local/lib/trible/b8a8d83d43e37ccf71c4e8668a6553ec450b7d67/receipts/mac-lock.sh
tag=erlkonig-native-app-$(date +%Y%m%d%H%M%S)-$$
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/Erlkonig/build/native-app-target-20261006}
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 CARGO_PROFILE_RELEASE_DEBUG=0
bash "$lock" check
df -h "$root"
available=$(memory_pressure -Q | awk '/free percentage/ {gsub(/%/,"",$NF); print $NF}')
[ "${available:-0}" -gt 20 ] || { echo 'At least 20% memory headroom is required' >&2; exit 1; }
bash "$lock" take "$tag"
heartbeat=
cleanup() {
    if [ -n "$heartbeat" ]; then kill "$heartbeat" 2>/dev/null || true; wait "$heartbeat" 2>/dev/null || true; fi
    bash "$lock" release "$tag"
}
trap cleanup EXIT INT TERM
(while true; do sleep 45; bash "$lock" refresh "$tag"; done) &
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
nice -n 10 cargo test --release --locked --offline -p erlkonig-core --lib --test lifecycle -j4 -- --test-threads=1
nice -n 10 cargo build --release --locked --offline -p erlkonig --bin erlkonig -j4
