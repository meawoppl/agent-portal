#!/usr/bin/env python3
"""Validate a visual-pr SVG against the style spec in SKILL.md.

Hard errors (exit 1): unparseable XML, wrong canvas, off-palette color.
Warnings (exit 0): text that looks like it overflows the canvas or its panel,
missing font-size. Overflow estimates use ~0.5 x font-size per character, the
same budget the spec quotes — eyeball anything flagged.
"""

import re
import sys
import xml.etree.ElementTree as ET

PALETTE = {
    "#16161e",  # background
    "#1e202e",  # box fill
    "#3d4666",  # neutral border / hairline
    "#565f89",  # muted
    "#a9b1d6",  # body
    "#c0caf5",  # bright
    "#e6e9f5",  # near-white (thesis)
    "#7aa2f7",  # blue
    "#9ece6a",  # green
    "#f7768e",  # red
    "#e0af68",  # orange
    "#bb9af7",  # purple
    "#7dcfff",  # teal
    "none",
}

CANVAS_W, CANVAS_H = 2000, 1200
MARGIN = 40  # a little tighter than the layout margin; past this is an escape
CHAR_FACTOR = 0.5

errors: list[str] = []
warnings: list[str] = []


def strip_ns(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def check_colors(elem: ET.Element) -> None:
    for attr in ("fill", "stroke"):
        val = elem.get(attr)
        if val is None:
            continue
        v = val.strip().lower()
        if v.startswith("url(") or v in ("currentcolor", "inherit"):
            continue
        if v not in PALETTE:
            errors.append(
                f"<{strip_ns(elem.tag)}> {attr}='{val}' is off-palette "
                f"(allowed: see SKILL.md palette table)"
            )


def text_content(elem: ET.Element) -> str:
    return "".join(elem.itertext()).strip()


def check_text(elem: ET.Element, inherited_size: float) -> None:
    size = elem.get("font-size")
    if size is None and strip_ns(elem.tag) == "text":
        warnings.append(f"<text> '{text_content(elem)[:40]}' has no font-size")
    fs = float(re.sub(r"[a-z%]+$", "", size)) if size else inherited_size

    content = text_content(elem)
    if not content:
        return
    x = elem.get("x")
    if x is None:
        return
    try:
        x = float(x)
    except ValueError:
        return

    est_w = len(content) * fs * CHAR_FACTOR
    anchor = elem.get("text-anchor", "start")
    if anchor == "middle":
        left, right = x - est_w / 2, x + est_w / 2
    elif anchor == "end":
        left, right = x - est_w, x
    else:
        left, right = x, x + est_w

    if right > CANVAS_W - MARGIN or left < 0:
        warnings.append(
            f"text likely overflows canvas (est {left:.0f}..{right:.0f}): "
            f"'{content[:60]}'"
        )
    # Panel-divider crossing: text starting in one panel shouldn't cross x=1000.
    if left < 990 and right > 1010 and fs < 30:  # thesis/footer (>=30 or footer y) span freely
        y = elem.get("y")
        yv = float(y) if y and re.fullmatch(r"[\d.]+", y) else 0.0
        if 190 < yv < 1140:
            warnings.append(
                f"text crosses the BEFORE/AFTER divider (est {left:.0f}..{right:.0f}): "
                f"'{content[:60]}'"
            )


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <file.svg>", file=sys.stderr)
        return 2

    try:
        tree = ET.parse(sys.argv[1])
    except ET.ParseError as e:
        print(f"ERROR: not well-formed XML: {e}", file=sys.stderr)
        return 1

    root = tree.getroot()
    viewbox = (root.get("viewBox") or "").split()
    if viewbox != ["0", "0", str(CANVAS_W), str(CANVAS_H)]:
        errors.append(
            f"viewBox is '{root.get('viewBox')}', spec requires '0 0 {CANVAS_W} {CANVAS_H}'"
        )

    for elem in root.iter():
        check_colors(elem)
        if strip_ns(elem.tag) in ("text", "tspan"):
            check_text(elem, inherited_size=23.0)

    for w in warnings:
        print(f"warning: {w}")
    for e in errors:
        print(f"ERROR: {e}", file=sys.stderr)

    if errors:
        return 1
    print(f"ok: palette + canvas clean, {len(warnings)} warning(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
