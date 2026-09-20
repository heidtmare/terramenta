#!/usr/bin/env bash
# Builds the porkchop plot module into WebAssembly.
#
#   ./scripts/build-wasm.sh                       debug build, into ./dist
#   ./scripts/build-wasm.sh --release             optimized build
#   ./scripts/build-wasm.sh --out-dir <path>      somewhere else
#
# There are no assets to go with it — this crate computes a grid and traces
# contours through it, nothing else — so unlike `terramenta-globe`'s own
# script, the output is just the module.
set -euo pipefail

CRATE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE="$(cd "$CRATE/.." && pwd)"

PROFILE="dev"
TARGET_DIR="debug"
OUT_DIR="$CRATE/dist"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      PROFILE="web-release"
      TARGET_DIR="web-release"
      shift
      ;;
    --out-dir)
      OUT_DIR="$2"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

cd "$WORKSPACE"

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

echo "Building ($PROFILE) ..."
cargo build --package terramenta-charta --lib --profile "$PROFILE" --target wasm32-unknown-unknown

mkdir -p "$OUT_DIR"
rm -f "$OUT_DIR"/terramenta_charta*

echo "Generating JS bindings ..."
wasm-bindgen \
  --out-dir "$OUT_DIR" \
  --out-name terramenta_charta \
  --target web \
  --no-typescript \
  "target/wasm32-unknown-unknown/$TARGET_DIR/terramenta_charta.wasm"

if [[ "$PROFILE" == "web-release" ]] && command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing with wasm-opt ..."
  wasm-opt -Os --output "$OUT_DIR/terramenta_charta_bg.wasm" "$OUT_DIR/terramenta_charta_bg.wasm"
fi

echo
echo "Module ready in $OUT_DIR ($(du -sh "$OUT_DIR" | cut -f1))"
