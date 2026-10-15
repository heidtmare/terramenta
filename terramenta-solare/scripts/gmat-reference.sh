#!/usr/bin/env bash
# Regenerates GMAT reference trajectories (tests/gmat/*.txt) from validation/gmat/*.script.
# Usage: ./scripts/gmat-reference.sh <GMAT root> [case ...]
# Needs data/de440s.bsp (scripts/fetch-data.sh). Gravity: embedded EGM2008 20×20 as .cof.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GMAT="$(cd "${1:?usage: $0 <GMAT root> [case ...]}" && pwd)"
shift
SPK="$ROOT/data/de440s.bsp"
[[ -s "$SPK" ]] || { echo "missing $SPK (scripts/fetch-data.sh)" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ICGEM .gfc → GMAT .cof (fixed width; S abuts C).
awk '
  $1 == "earth_gravity_constant" { gm = $2 }
  $1 == "radius" { re = $2 }
  $1 == "gfc" && $2 >= 2 {
    if (!head) { printf "POTFIELD %d %d  1 %.14E %.14E %.14E\n", 20, 20, gm, re, 1.0; head = 1 }
    c = $4; s = $5; gsub(/d/, "E", c); gsub(/d/, "E", s)
    if ($3 == 0) printf "RECOEF%5d%3d%24.14E\n", $2, $3, c
    else printf "RECOEF%5d%3d%24.14E%21.14E\n", $2, $3, c, s
  }
  END { print "END" }
' "$ROOT/src/force/egm2008_20.gfc" > "$WORK/EGM2008_20.cof"

# Plugins needing Python, MATLAB or proprietary libraries are dropped.
grep -v -E 'py312|Matlab|proprietary|OpenFrames|OVtoOFI|^OUTPUT_PATH' "$GMAT/bin/gmat_startup_file.txt" > "$WORK/startup.txt"
echo "OUTPUT_PATH = $WORK/" >> "$WORK/startup.txt"

cases=("$@")
if [[ ${#cases[@]} -eq 0 ]]; then
  for f in "$ROOT"/validation/gmat/*.script; do cases+=("$(basename "$f" .script)"); done
fi

mkdir -p "$ROOT/tests/gmat"
for c in "${cases[@]}"; do
  sed -e "s|@SPK@|$SPK|" -e "s|@COF@|$WORK/EGM2008_20.cof|" -e "s|@OUT@|$WORK/$c.txt|" \
    "$ROOT/validation/gmat/$c.script" > "$WORK/$c.script"
  echo "  run   $c"
  (cd "$GMAT/bin" && ./GmatConsole -s "$WORK/startup.txt" --run "$WORK/$c.script" > "$WORK/$c.log" 2>&1) \
    || { grep -i error "$WORK/$c.log" "$WORK/GmatLog.txt" >&2 || true; exit 1; }
  { echo "# GMAT $(grep -m1 'Build Date' "$WORK/$c.log" | sed 's/^ *//'): validation/gmat/$c.script"
    echo "# $(grep -m1 '^Report R' "$ROOT/validation/gmat/$c.script" | sed 's/^Report R //; s/;$//')"
    cat "$WORK/$c.txt"; } > "$ROOT/tests/gmat/$c.txt"
done
