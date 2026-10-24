#!/usr/bin/env bash
set -euo pipefail

cd "$SRC/waymux/fuzz"
cargo +nightly fuzz build -O
for target in hook_input review_notes config rpc_body; do
  cp "target/x86_64-unknown-linux-gnu/release/$target" "$OUT/"
done
