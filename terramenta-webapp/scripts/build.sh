#!/usr/bin/env bash
# Builds the globe module into terramenta-webapp/globe.
#
#   ./scripts/build.sh            debug build (fast to compile, slow to run)
#   ./scripts/build.sh --release  optimized build
#
# The app itself has no build step — it is HTML, CSS and ES modules served as
# they are — so this only exists to produce the WebAssembly it embeds.
set -euo pipefail

APP="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GLOBE="$(cd "$APP/../terramenta-globe" && pwd)"

exec "$GLOBE/scripts/build-wasm.sh" --out-dir "$APP/globe" "$@"
