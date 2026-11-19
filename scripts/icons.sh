#!/usr/bin/env sh
# Render `crates/app/icons/restbook.svg` into the icon every OS asks for.
#
#   scripts/icons.sh
#
# One source, four outputs, each going only where its platform reads it:
#
#   icons/restbook.ico          Windows: embedded in the exe by `winres` at build
#                               time, and used by the installer's shortcuts.
#   icons/restbook.icns         macOS: `Contents/Resources` in the `.app` bundle,
#                               named by `Info.plist`'s `CFBundleIconFile`.
#   icons/restbook-<n>.png      Linux: installed to
#                               `share/icons/hicolor/<n>x<n>/apps/<app_id>.png`,
#                               where <app_id> matches `branding::APP_ID`.
#
# Rendering needs an SVG rasterizer. `rsvg-convert` (librsvg) is used when it is
# present; otherwise the script stops and says what to install rather than
# leaving a stale icon in place silently. On a machine with neither librsvg nor
# cairo -- this repository's Windows box is one -- `scripts/generate-icons.py`
# renders the same SVG with Pillow instead and is documented there. Icon
# generation is a release step, not a build step: the app itself never reads
# these files.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
src="$root/crates/app/icons/restbook.svg"
out="$root/crates/app/icons"

# The sizes each output needs. Linux wants every hicolor size it might be asked
# for; the multi-image formats carry the same set plus 256.
sizes="16 24 32 48 64 128 256 512"

if ! command -v rsvg-convert >/dev/null 2>&1; then
    cat >&2 <<'EOF'
icons: rsvg-convert not found.

Install librsvg and re-run:
  sudo apt install librsvg2-bin        # Debian/Ubuntu
  brew install librsvg                 # macOS
  scoop install librsvg                # Windows (Git Bash/MSYS2)

The icons already committed under crates/app/icons are usable as they are; this
script only regenerates them from the SVG.

If this machine has Pillow but no librsvg or cairo, use instead:
  python scripts/generate-icons.py
EOF
    exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# One PNG per size, kept around because .ico and .icns are built from them.
for size in $sizes; do
    rsvg-convert -w "$size" -h "$size" "$src" -o "$tmp/restbook-$size.png"
done

# Linux reads these directly, so they are the files that are checked in.
for size in $sizes; do
    cp "$tmp/restbook-$size.png" "$out/restbook-$size.png"
done

# A .ico is a length-prefixed container of PNGs. Writing it by hand avoids a
# second tool (ImageMagick, `png2ico`) for a format that is a header plus the
# images concatenated.
if command -v python3 >/dev/null 2>&1; then
    python3 - "$tmp" "$out/restbook.ico" 16 24 32 48 64 128 256 <<'PY'
import struct, sys, pathlib

tmp, dest = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
sizes = [int(s) for s in sys.argv[3:]]
images = [(s, (tmp / f"restbook-{s}.png").read_bytes()) for s in sizes]

# ICONDIR, then one ICONDIRENTRY per image. A 256px entry is written as 0.
header = struct.pack("<HHH", 0, 1, len(images))
entries, offset = b"", 6 + 16 * len(images)
for size, data in images:
    dim = 0 if size >= 256 else size
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
    offset += len(data)

dest.write_bytes(header + entries + b"".join(data for _, data in images))
print(f"icons: wrote {dest}")
PY
else
    echo "icons: python3 not found; skipping restbook.ico" >&2
fi

# macOS: `iconutil` is the only supported .icns writer and ships with macOS.
if command -v iconutil >/dev/null 2>&1; then
    iconset="$tmp/restbook.iconset"
    mkdir -p "$iconset"
    for size in 16 32 64 128 256 512; do
        cp "$tmp/restbook-$size.png" "$iconset/icon_${size}x${size}.png"
        cp "$tmp/restbook-$size.png" "$iconset/icon_${size}x${size}@2x.png"
    done
    iconutil -c icns -o "$out/restbook.icns" "$iconset"
    echo "icons: wrote $out/restbook.icns"
else
    echo "icons: iconutil not found; skipping restbook.icns (run this on macOS)" >&2
fi

echo "icons: done; outputs in $out" >&2
ls -1 "$out" >&2
