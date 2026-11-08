#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
if [ "${AGENTICJIRA_FRONTEND_DIST+x}" = x ] || [ "${AGENTICJIRA_ALLOW_FOUNDATION+x}" = x ]; then
    echo "Normal distribution verification requires the standard dashboard. Unset AGENTICJIRA_FRONTEND_DIST and AGENTICJIRA_ALLOW_FOUNDATION." >&2
    exit 2
fi
if [ -n "${CARGO_TARGET_DIR:-}" ] || [ -n "${RUSTC_WRAPPER:-}" ] || [ -n "${RUSTC_WORKSPACE_WRAPPER:-}" ]; then
    echo "Exact package verification requires the repository target directory and direct Rust compiler. Clear target/wrapper overrides for this command." >&2
    exit 2
fi
if [ "$(uname -s)" = Darwin ]; then
    sh "$root/scripts/build-macos-menu.sh" prepare
fi
cd frontend
deno task typecheck
deno task test
deno task build
cd "$root"
cargo fmt --check
cargo check --locked
cargo test --locked
if [ "$(uname -s)" = Darwin ]; then
    cargo build --locked --release
    sh "$root/scripts/build-macos-menu.sh" compile
    sh "$root/scripts/build-macos-menu.sh" receipt
fi
echo "LLMRelay source, Rust tests, DOM flows, and frontend asset build verified."
