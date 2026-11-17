#!/bin/sh
# Render an HTML file (or URL) to PNG with the headless engine.
# usage: screenshot.sh <input> [out.png] [width] [height]
set -eu
input="${1:?usage: screenshot.sh <input> [out.png] [width] [height]}"
out="${2:-out.png}"
width="${3:-1024}"
height="${4:-768}"
cd "$(dirname "$0")/../.."
if printf '%s' "$input" | grep -q '^http'; then
  cargo run --quiet -p browser-headless -- --url "$input" --out "$out" --width "$width" --height "$height"
else
  cargo run --quiet -p browser-headless -- --html "$input" --out "$out" --width "$width" --height "$height"
fi
