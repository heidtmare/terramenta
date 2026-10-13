#!/usr/bin/env bash
# Fetches ephemeris kernels and Earth orientation data into data/.
# Usage: ./scripts/fetch-data.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/data"
mkdir -p "$DEST"

fetch() {
  local name="$1" url="$2"
  if [[ -s "$DEST/$name" ]]; then
    echo "  have  $name"
    return
  fi
  echo "  get   $name"
  curl -fsSL --retry 3 -o "$DEST/$name.part" "$url"
  mv "$DEST/$name.part" "$DEST/$name"
}

# JPL DE440s: 1849–2150, ~32 MB.
fetch de440s.bsp       "https://naif.jpl.nasa.gov/pub/naif/generic_kernels/spk/planets/de440s.bsp"
# IERS EOP 14 C04-consistent, IAU 2000A: 1973–present + 1 yr prediction.
fetch finals2000A.all  "https://datacenter.iers.org/data/9/finals2000A.all"
# CelesTrak EOP (CSV): x, y, UT1−UTC, LOD, dψ, dε, dX, dY.
fetch EOP-All.csv      "https://celestrak.org/SpaceData/EOP-All.csv"
