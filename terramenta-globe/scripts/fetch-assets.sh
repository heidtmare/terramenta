#!/usr/bin/env bash
# Downloads the public-domain NASA Visible Earth imagery used by the globe.
# Run once after cloning: ./scripts/fetch-assets.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/assets/textures"
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

echo "Fetching Earth textures into assets/textures ..."
fetch earth_day.jpg    "https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57752/land_shallow_topo_2048.jpg"
fetch earth_clouds.jpg "https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57747/cloud_combined_2048.jpg"
fetch earth_night.jpg  "https://eoimages.gsfc.nasa.gov/images/imagerecords/55000/55167/earth_lights_lrg.jpg"
echo "Done."
