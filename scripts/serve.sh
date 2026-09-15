#!/usr/bin/env bash
# Serves web/dist over HTTP. WebGPU and ES modules both require a real origin,
# so opening index.html from the filesystem will not work.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${1:-8080}"

if [[ ! -f "$ROOT/web/dist/terramenta.js" ]]; then
  echo "No bundle yet — building one first."
  "$ROOT/scripts/build-web.sh"
fi

echo "Serving http://localhost:$PORT/ (Ctrl-C to stop)"
cd "$ROOT/web/dist"
exec python3 -m http.server "$PORT"
