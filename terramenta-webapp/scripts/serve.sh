#!/usr/bin/env bash
# Serves the app over HTTP.
#
#   ./scripts/serve.sh [port]     default 8080
#
# WebGPU and ES modules both need a real origin, so opening index.html from the
# filesystem will not work.
set -euo pipefail

APP="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${1:-8080}"

if [[ ! -f "$APP/globe/terramenta_globe.js" ]]; then
  echo "No globe module yet — building one first."
  "$APP/scripts/build.sh"
fi

echo "Serving http://localhost:$PORT/ (Ctrl-C to stop)"
cd "$APP"
exec python3 -m http.server "$PORT"
