#!/usr/bin/env bash
# Re-render the PNG icons from the SVG sources. Needs rsvg-convert
# (e.g. `nix develop` provides it). Run from anywhere.
set -euo pipefail
cd "$(dirname "$0")"
for size in 16 22 24 32 48 64 128 256 512; do
  mkdir -p "hicolor/${size}x${size}/apps"
  rsvg-convert -w "$size" -h "$size" scalable/net.davidotek.hassdash4linux.svg \
    -o "hicolor/${size}x${size}/apps/net.davidotek.hassdash4linux.png"
done
for state in connected disconnected error; do
  for size in 16 22 24 32 48 64; do
    rsvg-convert -w "$size" -h "$size" "tray/${state}.svg" -o "tray/${state}-${size}.png"
  done
done
