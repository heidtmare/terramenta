#!/usr/bin/env bash
# Builds the WebAssembly bundle into web/dist.
#
#   ./scripts/build-web.sh            debug build (fast to compile, slow to run)
#   ./scripts/build-web.sh --release  optimized build
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE="dev"
TARGET_DIR="debug"
if [[ "${1:-}" == "--release" ]]; then
  PROFILE="web-release"
  TARGET_DIR="web-release"
fi

WASM_BINDGEN_VERSION="$(cargo metadata --format-version 1 --locked 2>/dev/null \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "wasm-bindgen"))')"

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen CLI not found. Install the version matching the crate:"
  echo "  cargo install wasm-bindgen-cli --version $WASM_BINDGEN_VERSION"
  exit 1
fi

INSTALLED_VERSION="$(wasm-bindgen --version | awk '{print $2}')"
if [[ "$INSTALLED_VERSION" != "$WASM_BINDGEN_VERSION" ]]; then
  echo "wasm-bindgen CLI is $INSTALLED_VERSION but the crate is $WASM_BINDGEN_VERSION."
  echo "They must match exactly:"
  echo "  cargo install wasm-bindgen-cli --version $WASM_BINDGEN_VERSION"
  exit 1
fi

if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
  echo "Adding the wasm32-unknown-unknown target ..."
  rustup target add wasm32-unknown-unknown
fi

"$ROOT/scripts/fetch-assets.sh"

echo "Building ($PROFILE) ..."
cargo build --profile "$PROFILE" --target wasm32-unknown-unknown

DIST="$ROOT/web/dist"
rm -rf "$DIST"
mkdir -p "$DIST"

echo "Generating JS bindings ..."
wasm-bindgen \
  --out-dir "$DIST" \
  --out-name terramenta \
  --target web \
  --no-typescript \
  "target/wasm32-unknown-unknown/$TARGET_DIR/terramenta.wasm"

if [[ "$PROFILE" == "web-release" ]] && command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing with wasm-opt ..."
  wasm-opt -Os --output "$DIST/terramenta_bg.wasm" "$DIST/terramenta_bg.wasm"
fi

cp "$ROOT/web/index.html" "$DIST/"
# Bevy's asset server fetches these over HTTP at the same relative path it uses
# on disk, so the folder has to sit next to index.html.
cp -R "$ROOT/assets" "$DIST/assets"

echo
echo "Bundle ready in web/dist ($(du -sh "$DIST" | cut -f1))"
echo "Serve it with: ./scripts/serve.sh"
