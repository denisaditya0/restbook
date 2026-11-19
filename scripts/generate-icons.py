#!/usr/bin/env python3
"""Rasterize ``crates/app/icons/restbook.svg`` into the PNG set and the .ico.

This is the fallback for machines that have no SVG rasterizer (no librsvg, no
cairo, no ImageMagick, no ffmpeg), so ``scripts/icons.sh`` cannot run. It parses
the handful of SVG values it needs out of the source file -- a vertical gradient
between two rounded stops, a rounded-corner tile, and two round-capped stroked
polylines -- and reproduces them with Pillow, which is the one imaging library
these machines do have.

Everything is drawn in a supersampled space and downsampled with Lanczos, which
is where the antialiasing comes from. Coordinates are kept in the SVG's own
512-unit viewBox and scaled up on the way into each draw call, so the geometry
in this file reads the same as the geometry in the SVG.

    python scripts/generate-icons.py

Only writes ``crates/app/icons/restbook-<n>.png`` and
``crates/app/icons/restbook.ico``. The SVG is the source and is never touched.
"""

from __future__ import annotations

import re
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
SVG = ROOT / "crates" / "app" / "icons" / "restbook.svg"
OUT = ROOT / "crates" / "app" / "icons"

# The PNG sizes ``scripts/icons.sh`` produces, plus the supersampling factor.
SIZES = [16, 24, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
SUPERSAMPLE = 4

# The viewBox the SVG coordinates below are expressed in.
VIEWBOX = 512.0


def parse_svg(text: str) -> dict:
    """Pull the values this script needs out of the SVG source.

    The markup is merged onto single lines, so every lookup runs case-insensitive
    regexes over the whole file rather than walking it line by line.
    """
    def find(pattern: str, what: str) -> str:
        m = re.search(pattern, text, re.IGNORECASE | re.DOTALL)
        if not m:
            raise SystemExit(f"generate-icons: could not find {what} in {SVG}")
        return m.group(1)

    # Gradient stops: offset -> hex colour.
    stops = [
        (float(offset), colour)
        for offset, colour in re.findall(
            r'<stop[^>]*offset=["\']([\d.]+)["\'][^>]*stop-color=["\'](#[0-9a-fA-F]{3,8})["\']',
            text,
            re.IGNORECASE,
        )
    ]
    if len(stops) < 2:
        raise SystemExit(f"generate-icons: need at least two gradient stops in {SVG}")
    stops.sort(key=lambda s: s[0])

    # The tile rectangle.
    rect = re.search(r"<rect\b([^>]*)>", text, re.IGNORECASE | re.DOTALL)
    if not rect:
        raise SystemExit(f"generate-icons: could not find the tile <rect> in {SVG}")
    attrs = rect.group(1)

    def attr(name: str, default: str | None = None) -> str | None:
        m = re.search(rf'\b{name}\s*=\s*["\']([^"\']*)["\']', attrs, re.IGNORECASE)
        return m.group(1) if m else default

    # Stroke properties: read from the <g> that wraps the arrow paths.
    stroke = find(r'stroke\s*=\s*"(#[0-9a-fA-F]{3,8})"', "the stroke colour")
    stroke_width = float(find(r'stroke-width\s*=\s*"([\d.]+)"', "the stroke width"))

    # The two stroked paths, in document order: shaft then head.
    paths = re.findall(r'<path\b[^>]*\bd\s*=\s*"([^"]+)"', text, re.IGNORECASE | re.DOTALL)
    if len(paths) < 2:
        raise SystemExit(f"generate-icons: expected two <path> elements in {SVG}")

    return {
        "stops": stops,
        "rect": {
            "x": float(attr("x", "0")),
            "y": float(attr("y", "0")),
            "width": float(attr("width", "0")),
            "height": float(attr("height", "0")),
            "rx": float(attr("rx", "0")),
        },
        "stroke": stroke,
        "stroke_width": stroke_width,
        "paths": [parse_path(d) for d in paths],
    }


def parse_path(d: str) -> list[tuple[float, float]]:
    """Parse the subset of path syntax the icon uses into absolute points.

    Handles ``M x y``, ``L x y``, ``H x`` and ``V y``, which is everything the
    source contains. Raises on anything else rather than silently misdrawing.
    """
    points: list[tuple[float, float]] = []
    cx = cy = 0.0
    for command, args in re.findall(r"([MLHVmlhv])([^MLHVmlhv]*)", d):
        nums = [float(n) for n in re.findall(r"-?\d+(?:\.\d+)?", args)]
        up = command.upper()
        relative = command.islower()
        if up == "M" or up == "L":
            for i in range(0, len(nums) - 1, 2):
                x, y = nums[i], nums[i + 1]
                if relative:
                    x, y = cx + x, cy + y
                points.append((x, y))
                cx, cy = x, y
        elif up == "H":
            for x in nums:
                cx = cx + x if relative else x
                points.append((cx, cy))
        elif up == "V":
            for y in nums:
                cy = cy + y if relative else y
                points.append((cx, cy))
        else:
            raise SystemExit(f"generate-icons: unsupported path command {command!r} in {d!r}")
    if not points:
        raise SystemExit(f"generate-icons: path had no coordinates: {d!r}")
    return points


def hex_to_rgba(value: str) -> tuple[int, int, int, int]:
    h = value.lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    if len(h) == 6:
        h += "ff"
    if len(h) != 8:
        raise SystemExit(f"generate-icons: bad colour {value!r}")
    return tuple(int(h[i : i + 2], 16) for i in (0, 2, 4, 6))  # type: ignore[return-value]


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def sample_gradient(stops: list[tuple[float, int, tuple[int, int, int, int]]], t: float):
    """Colour at position ``t`` in [0, 1] along the gradient axis."""
    t = min(max(t, 0.0), 1.0)
    if t <= stops[0][0]:
        return stops[0][2]
    if t >= stops[-1][0]:
        return stops[-1][2]
    for (o0, _, c0), (o1, _, c1) in zip(stops, stops[1:]):
        if o0 <= t <= o1:
            span = o1 - o0
            local = 0.0 if span == 0 else (t - o0) / span
            return tuple(round(lerp(c0[i], c1[i], local)) for i in range(4))
    return stops[-1][2]


def rounded_rect_mask(size: int, rect: dict, scale: float) -> Image.Image:
    """An L-mask for the tile's rounded rectangle.

    Pillow's ``rounded_rectangle`` gives the rounded shape; drawing it as a mask
    lets the gradient be composited into the corners without touching them.
    """
    mask = Image.new("L", (size, size), 0)
    draw = ImageDraw.Draw(mask)
    x0 = rect["x"] * scale
    y0 = rect["y"] * scale
    x1 = (rect["x"] + rect["width"]) * scale
    y1 = (rect["y"] + rect["height"]) * scale
    radius = rect["rx"] * scale
    draw.rounded_rectangle((x0, y0, x1, y1), radius=radius, fill=255)
    return mask


def gradient_fill(size: int, stops) -> Image.Image:
    """The vertical gradient as an opaque RGB image, one row at a time."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    # The SVG's gradient runs top to bottom over the viewBox, so the row at
    # y maps to t = y / (size - 1); a single-row image degenerates, so guard.
    denom = max(size - 1, 1)
    for y in range(size):
        t = y / denom
        colour = sample_gradient(stops, t)
        draw.line([(0, y), (size - 1, y)], fill=colour)
    return img


def draw_arrow(img: Image.Image, paths, stroke_rgba, width: float, scale: float) -> None:
    """Stroke the arrow paths with round caps and round joins.

    Pillow's ``line`` has no cap or join setting, so the SVG's
    ``stroke-linecap="round"`` is reproduced with a filled disc of the stroke
    radius at every run endpoint, and ``stroke-linejoin="round"`` with
    ``joint="curve"``, which rounds the corner between segments of one run.
    """
    draw = ImageDraw.Draw(img)
    w = max(1, round(width * scale))
    radius = w / 2.0

    def disc(cx: float, cy: float) -> None:
        draw.ellipse((cx - radius, cy - radius, cx + radius, cy + radius), fill=stroke_rgba)

    for path in paths:
        pts = [(x * scale, y * scale) for x, y in path]
        if len(pts) >= 2:
            draw.line(pts, fill=stroke_rgba, width=w, joint="curve")
        # Round caps at the ends of this run. A single-point path is all cap.
        for cx, cy in {pts[0], pts[-1]}:
            disc(cx, cy)


def render(spec: dict, size: int) -> Image.Image:
    """Render the icon at ``size`` px square, supersampled and downsampled."""
    ss = size * SUPERSAMPLE
    scale = ss / VIEWBOX

    # The tile: gradient restricted to a rounded rectangle.
    stops = [(o, hex_to_rgba(c)) for o, c in spec["stops"]]
    stops = [(o, c, c) for o, c in stops]
    tile = gradient_fill(ss, stops)
    mask = rounded_rect_mask(ss, spec["rect"], scale)
    tile.putalpha(mask)

    # The arrow sits on top of the tile, inside the same canvas.
    draw_arrow(tile, spec["paths"], hex_to_rgba(spec["stroke"]), spec["stroke_width"], scale)

    return tile.resize((size, size), Image.Resampling.LANCZOS)


def write_ico(pngs: dict[int, Path], dest: Path, sizes: list[int]) -> None:
    """Write the .ico directly from the rendered PNG bytes.

    The .ico is a header plus the PNGs concatenated; writing it here keeps the
    output to the same file the shell script produces without needing a second
    tool. A 256px entry is encoded with dimension 0, per the format.
    """
    entries = [(s, pngs[s].read_bytes()) for s in sizes]
    header = struct.pack("<HHH", 0, 1, len(entries))
    directory, offset = b"", 6 + 16 * len(entries)
    for size, data in entries:
        dim = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    dest.write_bytes(header + directory + b"".join(data for _, data in entries))


def main() -> int:
    spec = parse_svg(SVG.read_text(encoding="utf-8"))
    print(
        f"generate-icons: {SVG.name}: "
        f"stops {spec['stops']}, stroke {spec['stroke']} @ {spec['stroke_width']}, "
        f"paths {spec['paths']}",
        file=sys.stderr,
    )

    written: list[Path] = []
    for size in SIZES:
        img = render(spec, size)
        dest = OUT / f"restbook-{size}.png"
        img.save(dest, format="PNG")
        written.append(dest)
        print(f"generate-icons: wrote {dest.name} ({dest.stat().st_size} bytes)", file=sys.stderr)

    ico = OUT / "restbook.ico"
    write_ico({s: OUT / f"restbook-{s}.png" for s in ICO_SIZES}, ico, ICO_SIZES)
    written.append(ico)
    print(f"generate-icons: wrote {ico.name} ({ico.stat().st_size} bytes)", file=sys.stderr)

    print(f"generate-icons: done; {len(written)} files in {OUT}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
