#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)
stage="$root/.local/dist/llmrelay-$version-macos"
archive="$root/.local/dist/llmrelay-$version-macos.zip"
"$root/scripts/verify.sh"
cargo build --locked --release --manifest-path "$root/Cargo.toml"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/share/llmrelay"
cp "$root/target/release/llmrelay" "$stage/bin/llmrelay"
cp "$root/target/release/agenticjira" "$stage/bin/agenticjira"
cp "$root/README.md" "$root/RESEARCH_AND_PLAN.md" "$root/THIRD_PARTY_NOTICES.md" "$stage/share/llmrelay/"
cp -R "$root/docs" "$stage/share/llmrelay/docs"
rm -f "$archive"
ditto -c -k --sequesterRsrc --keepParent "$stage" "$archive"
echo "$archive"
