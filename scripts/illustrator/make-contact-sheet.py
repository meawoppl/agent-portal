#!/usr/bin/env python3
"""Merge PR diagrams into a tiled contact sheet.

Namespaces every `id` per tile: bare ids like `arrow-green` collide across
files, and on collision the first definition in document order wins, so later
tiles silently draw the wrong marker.
"""
import re, sys, pathlib

order = [int(x) for x in sys.argv[1].split(",")]
src = pathlib.Path(sys.argv[2]); dest = pathlib.Path(sys.argv[3])
COLS, ROWS = 3, 4
BG, MUTED = "#1a1b26", "#565f89"

first = (src / f"pr-{order[0]}.svg").read_text()
TW = int(re.search(r'viewBox="0 0 (\d+) (\d+)"', first).group(1))
TH = int(re.search(r'viewBox="0 0 (\d+) (\d+)"', first).group(2))

tiles = []
for idx, pr in enumerate(order):
    t = (src / f"pr-{pr}.svg").read_text()
    inner = t[t.index(">", t.index("<svg")) + 1 : t.rindex("</svg>")]
    for i in sorted(set(re.findall(r'id="([^"]+)"', inner)), key=len, reverse=True):
        new = f"t{pr}-{i}"
        inner = (inner.replace(f'id="{i}"', f'id="{new}"')
                      .replace(f"url(#{i})", f"url(#{new})")
                      .replace(f'href="#{i}"', f'href="#{new}"'))
    c, r = idx % COLS, idx // COLS
    tiles.append(f'<svg x="{c*TW}" y="{r*TH}" width="{TW}" height="{TH}" viewBox="0 0 {TW} {TH}">{inner}</svg>')

W, H = COLS * TW, ROWS * TH
rules = "".join(
    [f'<line x1="{c*TW}" y1="0" x2="{c*TW}" y2="{H}" stroke="{MUTED}" stroke-opacity="0.55" stroke-width="2"/>' for c in range(1, COLS)]
    + [f'<line x1="0" y1="{r*TH}" x2="{W}" y2="{r*TH}" stroke="{MUTED}" stroke-opacity="0.55" stroke-width="2"/>' for r in range(1, ROWS)]
)
dest.write_text(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">'
    f'<rect x="0" y="0" width="{W}" height="{H}" fill="{BG}"/>' + "".join(tiles) + rules + "</svg>"
)
print(f"{dest.name}: {W}x{H}, {len(tiles)} tiles, {dest.stat().st_size//1024} KB")
