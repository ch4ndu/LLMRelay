#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
cd frontend
deno task typecheck
deno task test
deno task build
cd "$root"
cargo fmt --check
cargo check --locked
cargo test --locked
echo "LLMRelay source, Rust tests, DOM flows, and frontend asset build verified."
