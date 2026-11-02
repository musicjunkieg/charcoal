#!/bin/bash
# Run a cargo-fuzz target against the same dependency versions the app uses.
#
# Usage: scripts/fuzz.sh <target> [seconds]     (default 120 s)
#        scripts/fuzz.sh --list
#
# Why a script rather than plain `cargo fuzz run`:
#
# 1. fuzz/ is its own crate with its own Cargo.lock. Left alone it resolves
#    newer versions than the app (ort 2.0.0-rc.15 instead of rc.11 broke the
#    build, #420), so the app's Cargo.lock is copied in before every run.
#
# 2. cargo-fuzz needs unstable compiler flags (-Zsanitizer=address). The
#    nightly toolchain current on 2026-11-01 (1.101.0-nightly f9ceb4a4d) hits
#    a compiler query cycle ("needs async drop") in atrium-api, so we use the
#    STABLE compiler with RUSTC_BOOTSTRAP=1, which unlocks those flags. That
#    also fuzzes code built by the same compiler that builds the app. Drop the
#    workaround once a nightly builds atrium-api again:
#        cargo +nightly fuzz build runpod_batch_response
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "${1:-}" = "--list" ]; then
    exec cargo fuzz list
fi

TARGET="${1:?usage: scripts/fuzz.sh <target> [seconds] | --list}"
SECONDS_TO_RUN="${2:-120}"

cp Cargo.lock fuzz/Cargo.lock

# The committed seeds are read-only examples of real responses; the corpus
# directory (gitignored) is where libFuzzer saves the inputs it discovers.
mkdir -p "fuzz/corpus/$TARGET"
SEEDS=()
[ -d "fuzz/seeds/$TARGET" ] && SEEDS=("fuzz/seeds/$TARGET")

RUSTC_BOOTSTRAP=1 cargo +stable fuzz run "$TARGET" \
    "fuzz/corpus/$TARGET" "${SEEDS[@]}" \
    -- -max_total_time="$SECONDS_TO_RUN"
