#!/usr/bin/env bash
# Builds the browser demo of the core apps (crates/derisk-web) into a
# folder of static files: index.html, derisk_web.js and derisk_web_bg.wasm.
# Serve it over HTTP (`python3 -m http.server -d dist/web`); browsers do
# not load WebAssembly modules from file:// pages.
#
#   scripts/build-web.sh [OUT_DIR]     default: dist/web
#
# Needs the wasm32-unknown-unknown target and wasm-bindgen-cli at the
# version Cargo.lock pins wasm-bindgen to, since the two must match:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --locked --version <that version>
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/dist/web}

want=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print }' "$root/Cargo.lock")
have=$(wasm-bindgen --version 2>/dev/null | awk '{ print $2 }')
if [ "$have" != "$want" ]; then
    echo "need wasm-bindgen-cli $want (found ${have:-none}):" >&2
    echo "  cargo install wasm-bindgen-cli --locked --version $want" >&2
    exit 1
fi

cargo build --manifest-path "$root/Cargo.toml" --release \
    --package derisk-web --target wasm32-unknown-unknown
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
    "$root/target/wasm32-unknown-unknown/release/derisk_web.wasm"
cp "$root/crates/derisk-web/web/index.html" "$out/"
echo "built $out"
