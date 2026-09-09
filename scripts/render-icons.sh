#!/usr/bin/env bash
# Render the canonical SVG and convert RGBA pixels to StatusNotifier ARGB.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
for size in 16 32 48 128 256; do
  rsvg-convert -w "$size" -h "$size" assets/io.nory.NORY.svg \
    -o "assets/icons/io.nory.NORY-$size.png"
done
# Windows uses ICO resources, not the GTK PNG. Always regenerate both from
# the same SVG so installer / desktop icons cannot lag behind the app logo.
magick assets/icons/io.nory.NORY-16.png assets/icons/io.nory.NORY-32.png \
  assets/icons/io.nory.NORY-48.png assets/icons/io.nory.NORY-128.png \
  assets/icons/io.nory.NORY-256.png assets/icons/io.nory.NORY.ico
for size in 16 32; do
  magick "assets/icons/io.nory.NORY-$size.png" -depth 8 RGBA:- \
    | perl -0777 -pe 's/(.)(.)(.)(.)/$4$1$2$3/sg' \
    > "assets/icons/io.nory.NORY-$size.argb"
done
