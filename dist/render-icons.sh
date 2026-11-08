#!/bin/sh
# Renders the app icon's PNG and ICO files from the SVG source.
# Needs rsvg-convert (librsvg) and magick (ImageMagick). Run from anywhere.
set -eu

dir="$(dirname "$0")/../crates/leafport-desktop/data/icons"
svg="$dir/dev.leafport.Leafport.svg"

for size in 32 128 256 512 1024; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$dir/$size.png"
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for size in 16 24 32 48 64 256; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$tmp/$size.png"
done
magick "$tmp/16.png" "$tmp/24.png" "$tmp/32.png" "$tmp/48.png" "$tmp/64.png" "$tmp/256.png" \
    "$dir/leafport.ico"
