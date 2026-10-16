#!/usr/bin/env bash
# Fetches ephemeris kernels, Earth orientation, space weather and gravity data into data/.
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
# IAU 2015 rotation elements, DE440 GM, DE440 lunar librations (tests/central_body.rs).
fetch pck00011.tpc     "https://naif.jpl.nasa.gov/pub/naif/generic_kernels/pck/pck00011.tpc"
fetch gm_de440.tpc     "https://naif.jpl.nasa.gov/pub/naif/generic_kernels/pck/gm_de440.tpc"
fetch moon_pa_de440_200625.bpc "https://naif.jpl.nasa.gov/pub/naif/generic_kernels/pck/moon_pa_de440_200625.bpc"
# IERS EOP 14 C04-consistent, IAU 2000A: 1973–present + 1 yr prediction.
fetch finals2000A.all  "https://datacenter.iers.org/data/9/finals2000A.all"
# CelesTrak EOP (CSV): x, y, UT1−UTC, LOD, dψ, dε, dX, dY.
fetch EOP-All.csv      "https://celestrak.org/SpaceData/EOP-All.csv"
# CelesTrak space weather (CSV): Ap, F10.7; observed + daily/monthly prediction.
fetch SW-All.csv       "https://celestrak.org/SpaceData/SW-All.csv"

# EGM2008 (ICGEM, fully normalised, tide-free), truncated to degree 120 while streaming.
# Degree ≤ 20 is embedded in the crate.
EGM_URL="https://icgem.gfz-potsdam.de/getmodel/gfc/c50128797a9cb62e936337c890e4425f03f0461d7329b09a8cc8561504465340/EGM2008.gfc"
if [[ -s "$DEST/EGM2008_120.gfc" ]]; then
  echo "  have  EGM2008_120.gfc"
else
  echo "  get   EGM2008_120.gfc"
  # awk exits early; curl's write failure is expected.
  { curl -fsL --retry 3 "$EGM_URL" || true; } \
    | awk '$1 == "gfc" && $2 > 120 { exit } { print }' > "$DEST/EGM2008_120.gfc.part"
  grep -Eq '^gfc +120 +120 ' "$DEST/EGM2008_120.gfc.part"
  mv "$DEST/EGM2008_120.gfc.part" "$DEST/EGM2008_120.gfc"
fi
