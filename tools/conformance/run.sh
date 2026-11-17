#!/bin/sh
# Validation gate: format, check, tests and lints for the whole workspace.
set -eu
cd "$(dirname "$0")/../.."
echo "== fmt =="
cargo fmt --all -- --check
echo "== check =="
cargo check --workspace
echo "== test =="
cargo test --workspace
echo "== clippy =="
cargo clippy --workspace --all-targets -- -D warnings
echo "OK"
