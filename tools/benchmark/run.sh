#!/bin/sh
# Measure the full pipeline on a fixture. Reports wall time per run and node counts.
set -eu
fixture="${1:-tests/fixtures/page.html}"
runs="${2:-10}"
cd "$(dirname "$0")/../.."
cargo build --quiet -p browser-headless
binary="target/debug/browser-headless"
"$binary" --html "$fixture" --out /tmp/mag-bench.png --json
start=$(date +%s%N)
i=0
while [ "$i" -lt "$runs" ]; do
  "$binary" --html "$fixture" --out /tmp/mag-bench.png >/dev/null
  i=$((i + 1))
done
end=$(date +%s%N)
elapsed=$(( (end - start) / 1000000 ))
echo "runs: $runs"
echo "total: ${elapsed}ms"
echo "per run: $((elapsed / runs))ms"
