#!/usr/bin/env python3
"""Fail on SVG text whose glyphs the rendering font does not have.

A missing glyph does not error — it draws a "tofu" box, so a diagram can look
finished in the writer's terminal and be unreadable everywhere else. This
resolves each `<text>` element's font the way a rasterizer does and checks
every codepoint against that font's cmap.

Two findings this was written to catch, both from real diagrams:

1. **Font stacks silently do not fall through.** CSS lists like
   `ui-monospace, SFMono-Regular, Menlo, monospace` work in a browser, which
   walks the list. fontconfig-based renderers (cairosvg, rsvg, Inkscape)
   resolve the *first* name and stop — and since `ui-monospace` exists on no
   Linux box, they land on the default sans. So the text renders proportionally
   when monospace was intended, and any width arithmetic based on a fixed
   advance is wrong.

2. **The default sans lacks arrows and math.** Noto Sans has no `→` `⇒` `≤`
   `≠` `✓`, while DejaVu Sans Mono has all of them. Whether a glyph survives
   therefore depends on which font the stack accidentally resolved to.

Usage:
    check-svg-glyphs.py FILE.svg [FILE.svg ...]

Exit status is non-zero if any glyph is missing, so this can gate a pipeline.
"""

from __future__ import annotations

import subprocess
import sys
import unicodedata
import xml.etree.ElementTree as ET
from functools import lru_cache

SVG = "{http://www.w3.org/2000/svg}"

# Characters that render as a box in at least one font a common renderer picks.
# Arrows are the big one: they are the natural thing to type between two
# labels, and they are exactly what the default sans is missing. Draw them as
# a <line> with a <marker> instead — it survives every font.
PREFER_DRAWN = {
    0x2190: "←", 0x2192: "→", 0x2191: "↑", 0x2193: "↓",
    0x21D2: "⇒", 0x21D0: "⇐",
}


@lru_cache(maxsize=None)
def resolve(family: str) -> tuple[str, frozenset[int]]:
    """Resolve one family name to (font file, covered codepoints)."""
    path = subprocess.run(
        ["fc-match", "-f", "%{file}", family],
        capture_output=True, text=True,
    ).stdout.strip()
    try:
        from fontTools.ttLib import TTFont

        font = TTFont(path, fontNumber=0, lazy=True)
        covered: set[int] = set()
        for table in font["cmap"].tables:
            covered |= set(table.cmap.keys())
        return path, frozenset(covered)
    except Exception:
        return path, frozenset()


def first_family(font_family: str) -> str:
    """The family a fontconfig renderer will actually use.

    Deliberately mirrors the naive behavior rather than CSS semantics: taking
    only the first entry is precisely the bug being detected.
    """
    return font_family.split(",")[0].strip().strip("'\"")


def check(path: str) -> list[str]:
    root = ET.parse(path).getroot()
    problems: list[str] = []
    for text in root.iter(SVG + "text"):
        stack = text.get("font-family") or "sans-serif"
        family = first_family(stack)
        font_file, covered = resolve(family)
        if not covered:
            problems.append(f"  could not read font for {family!r} ({font_file})")
            continue
        content = "".join(text.itertext())
        for ch in content:
            if ord(ch) < 128 or ch.isspace():
                continue
            if ord(ch) not in covered:
                try:
                    name = unicodedata.name(ch)
                except ValueError:
                    name = "<unnamed>"
                hint = ""
                if ord(ch) in PREFER_DRAWN:
                    hint = "  [draw this as <line>+<marker>, not text]"
                problems.append(
                    f"  U+{ord(ch):04X} {ch!r} ({name}) missing from "
                    f"{font_file.split('/')[-1]} — resolved from {stack!r}{hint}"
                )
    return problems


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    failed = False
    for path in sys.argv[1:]:
        problems = sorted(set(check(path)))
        if problems:
            failed = True
            print(f"{path}: {len(problems)} problem(s)")
            for p in problems:
                print(p)
        else:
            print(f"{path}: ok")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
