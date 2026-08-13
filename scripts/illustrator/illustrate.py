#!/usr/bin/env python3
"""Offline illustrator harness for the session storyboard (#1407, Rev 3 P2).

Walks a merged PR's commits and emits, per commit, a *scene spec* (JSON) plus a
rendered SVG frame. Deliberately split that way: the scene spec is the durable
artifact and the renderer is disposable, so the whole history can be re-rendered
when the renderer improves. That is also why the model — when one is eventually
added to pick emphasis and write captions — will emit a scene spec rather than
SVG: coordinate-juggling is not something to delegate, and a schema can be
validated where raw markup cannot.

There is no model in this version, on purpose. Rev 3 §9 calls for building the
deterministic layer first: which files changed, how much they churned, which
crates the work moved through, and the commit subject as the caption. All of it
comes out of git for free, and having it makes it possible to judge how much a
generative layer would actually add.

Layout is computed once over the union of every file the PR touches, so a file
occupies the same rectangle in every frame. That stability is the thing that
makes a sequence read as one continuous picture rather than as unrelated
diagrams; without it the frames jump and the eye cannot follow the work.

Usage:
    ./illustrate.py --pr 1612 1567 1613 --out /tmp/storyboard
"""

from __future__ import annotations

import argparse
import json
import subprocess
from dataclasses import dataclass, field, asdict
from pathlib import Path

# Tokyo Night, matching the portal's own transcript palette. Frames are
# rendered with a transparent background so they sit on the portal's #1a1b26
# without a seam, and so they stay legible if embedded anywhere else dark.
TEXT = "#c0caf5"
MUTED = "#565f89"
BLUE = "#7aa2f7"
GREEN = "#9ece6a"
RED = "#f7768e"
ORANGE = "#e0af68"
PURPLE = "#bb9af7"
TEAL = "#7dcfff"

WIDTH, HEIGHT = 1200, 700
PAD_TOP = 76      # caption band
PAD_BOTTOM = 44   # progress band
PAD_SIDE = 24


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout


@dataclass
class FileChange:
    path: str
    added: int
    removed: int

    @property
    def churn(self) -> int:
        return self.added + self.removed

    @property
    def kind(self) -> str:
        if self.removed == 0 and self.added > 0:
            return "added"
        if self.added == 0 and self.removed > 0:
            return "removed"
        return "modified"


@dataclass
class Commit:
    sha: str
    subject: str
    files: list[FileChange] = field(default_factory=list)


def pr_commits(pr: int) -> list[Commit]:
    """Commits of a merged PR.

    The repo squash-merges and deletes branches, so a merged PR's individual
    commits are not reachable from `main` — only the squashed result is. They
    remain fetchable through GitHub's `refs/pull/<n>/head`, which is what makes
    this harness able to run against already-merged work at all.
    """
    git("fetch", "-q", "origin", f"refs/pull/{pr}/head")
    head = git("rev-parse", "FETCH_HEAD").strip()
    base = git("merge-base", "main", head).strip()

    log = git("log", "--reverse", "--format=%H%x1f%s", f"{base}..{head}")
    commits: list[Commit] = []
    for line in log.splitlines():
        if not line.strip():
            continue
        sha, _, subject = line.partition("\x1f")
        commits.append(Commit(sha=sha, subject=subject))

    for commit in commits:
        numstat = git("show", "--numstat", "--format=", commit.sha)
        for row in numstat.splitlines():
            parts = row.split("\t")
            if len(parts) != 3:
                continue
            added, removed, path = parts
            # Binary files report "-"; they have no line churn to size by.
            if added == "-" or removed == "-":
                continue
            commit.files.append(
                FileChange(path=path, added=int(added), removed=int(removed))
            )
    return commits


def group_of(path: str) -> str:
    """Top-level crate/directory a file belongs to — the unit a reader thinks in."""
    head, _, tail = path.partition("/")
    return head if tail else "(root)"


def squarify(items: list[tuple[str, float]], x: float, y: float, w: float, h: float):
    """Squarified treemap.

    Chosen over slice-and-dice because aspect ratio is what makes a small file
    readable at all: a sliver two pixels wide can be drawn but not labeled, and
    an unlabeled rectangle carries no information.
    """
    out: dict[str, tuple[float, float, float, float]] = {}
    items = [(k, v) for k, v in items if v > 0]
    if not items:
        return out
    total = sum(v for _, v in items)
    scale = (w * h) / total if total else 0
    items = sorted(items, key=lambda kv: -kv[1])

    def worst(row, side):
        if not row or side == 0:
            return float("inf")
        s = sum(row)
        return max((side * side * max(row)) / (s * s), (s * s) / (side * side * min(row)))

    i = 0
    while i < len(items):
        side = min(w, h)
        row: list[float] = []
        keys: list[str] = []
        while i < len(items):
            candidate = row + [items[i][1] * scale]
            if row and worst(candidate, side) > worst(row, side):
                break
            row = candidate
            keys.append(items[i][0])
            i += 1
        s = sum(row)
        thickness = s / side if side else 0
        offset = 0.0
        for key, area in zip(keys, row):
            length = area / thickness if thickness else 0
            if w >= h:
                out[key] = (x, y + offset, thickness, length)
            else:
                out[key] = (x + offset, y, length, thickness)
            offset += length
        if w >= h:
            x += thickness
            w -= thickness
        else:
            y += thickness
            h -= thickness
    return out


def build_layout(commits: list[Commit]):
    """One rectangle per file, computed once over the whole PR.

    Sizing uses total churn across the PR rather than per-commit churn so that
    a rectangle never resizes between frames — same reason positions are fixed.
    """
    totals: dict[str, int] = {}
    for commit in commits:
        for change in commit.files:
            totals[change.path] = totals.get(change.path, 0) + change.churn

    groups: dict[str, list[str]] = {}
    for path in totals:
        groups.setdefault(group_of(path), []).append(path)

    inner_w = WIDTH - 2 * PAD_SIDE
    inner_h = HEIGHT - PAD_TOP - PAD_BOTTOM
    group_weights = [
        (name, float(sum(totals[p] for p in paths))) for name, paths in groups.items()
    ]
    group_rects = squarify(group_weights, PAD_SIDE, PAD_TOP, inner_w, inner_h)

    layout: dict[str, tuple[float, float, float, float]] = {}
    for name, (gx, gy, gw, gh) in group_rects.items():
        # Inset leaves room for the group label and keeps neighbouring crates
        # visually separate.
        pad = 2.0
        label = 16.0
        files = [(p, float(totals[p])) for p in groups[name]]
        layout.update(
            squarify(files, gx + pad, gy + label, max(gw - 2 * pad, 1), max(gh - label - pad, 1))
        )
    return totals, group_rects, layout


def scene_for(commits, index, totals, group_rects, layout) -> dict:
    """The typed scene description for one frame.

    This is the artifact a model would emit once emphasis and captioning become
    generative; the renderer below consumes only this.
    """
    commit = commits[index]
    touched_now = {c.path: c for c in commit.files}
    touched_before = set()
    for earlier in commits[:index]:
        touched_before.update(c.path for c in earlier.files)

    nodes = []
    for path, rect in layout.items():
        if path in touched_now:
            state, kind = "current", touched_now[path].kind
        elif path in touched_before:
            state, kind = "prior", None
        else:
            state, kind = "pending", None
        nodes.append(
            {
                "path": path,
                "group": group_of(path),
                "rect": [round(v, 2) for v in rect],
                "churn_total": totals[path],
                "churn_now": touched_now[path].churn if path in touched_now else 0,
                "state": state,
                "kind": kind,
            }
        )

    return {
        "frame": index + 1,
        "frames": len(commits),
        "sha": commit.sha[:9],
        "caption": commit.subject,
        "groups": [
            {"name": n, "rect": [round(v, 2) for v in r]} for n, r in group_rects.items()
        ],
        "nodes": sorted(nodes, key=lambda n: n["path"]),
        "stats": {
            "files_now": len(touched_now),
            "added": sum(c.added for c in commit.files),
            "removed": sum(c.removed for c in commit.files),
        },
    }


def esc(text: str) -> str:
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


KIND_COLOR = {"added": GREEN, "removed": RED, "modified": BLUE}


def render(scene: dict, pr: int) -> str:
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
        f'viewBox="0 0 {WIDTH} {HEIGHT}" font-family="ui-monospace,SFMono-Regular,Menlo,monospace">'
    ]

    caption = scene["caption"]
    if len(caption) > 78:
        caption = caption[:75] + "…"
    parts.append(
        f'<text x="{PAD_SIDE}" y="34" fill="{TEXT}" font-size="21" font-weight="600">{esc(caption)}</text>'
    )
    parts.append(
        f'<text x="{PAD_SIDE}" y="58" fill="{MUTED}" font-size="13">'
        f'PR #{pr} · {scene["sha"]} · {scene["stats"]["files_now"]} files '
        f'<tspan fill="{GREEN}">+{scene["stats"]["added"]}</tspan> '
        f'<tspan fill="{RED}">-{scene["stats"]["removed"]}</tspan></text>'
    )

    for group in scene["groups"]:
        gx, gy, gw, gh = group["rect"]
        parts.append(
            f'<rect x="{gx:.1f}" y="{gy:.1f}" width="{max(gw,0):.1f}" height="{max(gh,0):.1f}" '
            f'fill="none" stroke="{MUTED}" stroke-width="1" stroke-opacity="0.45" rx="3"/>'
        )
        if gw > 46 and gh > 18:
            parts.append(
                f'<text x="{gx+5:.1f}" y="{gy+12:.1f}" fill="{MUTED}" font-size="11" '
                f'letter-spacing="0.5">{esc(group["name"])}</text>'
            )

    for node in scene["nodes"]:
        x, y, w, h = node["rect"]
        w, h = max(w - 1.5, 0), max(h - 1.5, 0)
        if w <= 0 or h <= 0:
            continue
        state = node["state"]
        if state == "current":
            color = KIND_COLOR.get(node["kind"], BLUE)
            fill_op, stroke, stroke_op, sw = 0.85, color, 1.0, 1.6
        elif state == "prior":
            color, fill_op, stroke, stroke_op, sw = MUTED, 0.30, MUTED, 0.7, 1.0
        else:
            color, fill_op, stroke, stroke_op, sw = MUTED, 0.06, MUTED, 0.35, 1.0
        parts.append(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="2" '
            f'fill="{color}" fill-opacity="{fill_op}" stroke="{stroke}" '
            f'stroke-opacity="{stroke_op}" stroke-width="{sw}"/>'
        )
        # Only label a rectangle big enough to hold the text; a clipped label is
        # worse than none.
        name = node["path"].rsplit("/", 1)[-1]
        if w > 8.2 * len(name) * 0.62 and h > 15:
            label_fill = "#1a1b26" if state == "current" else TEXT
            op = "0.95" if state == "current" else "0.55"
            parts.append(
                f'<text x="{x+4:.1f}" y="{y+12:.1f}" fill="{label_fill}" fill-opacity="{op}" '
                f'font-size="10">{esc(name)}</text>'
            )

    # Progress: one tick per commit, current one accented.
    total = scene["frames"]
    tick_w = (WIDTH - 2 * PAD_SIDE) / max(total, 1)
    ty = HEIGHT - 26
    for i in range(total):
        on = i < scene["frame"]
        parts.append(
            f'<rect x="{PAD_SIDE + i*tick_w + 1:.1f}" y="{ty}" width="{max(tick_w-2,1):.1f}" height="4" rx="2" '
            f'fill="{TEAL if on else MUTED}" fill-opacity="{"0.95" if on else "0.3"}"/>'
        )
    parts.append(
        f'<text x="{WIDTH - PAD_SIDE}" y="{ty + 20}" fill="{MUTED}" font-size="11" '
        f'text-anchor="end">{scene["frame"]} / {total}</text>'
    )
    parts.append("</svg>")
    return "\n".join(parts)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--pr", type=int, nargs="+", required=True)
    ap.add_argument("--out", type=Path, default=Path("/tmp/storyboard"))
    ap.add_argument("--repo", type=Path, default=Path.cwd())
    args = ap.parse_args()

    global REPO
    REPO = args.repo

    args.out.mkdir(parents=True, exist_ok=True)
    summary = []
    for pr in args.pr:
        commits = pr_commits(pr)
        if not commits:
            print(f"PR {pr}: no commits")
            continue
        totals, group_rects, layout = build_layout(commits)
        if not layout:
            print(f"PR {pr}: no text file changes")
            continue

        d = args.out / f"pr-{pr}"
        d.mkdir(parents=True, exist_ok=True)
        scenes = []
        for i in range(len(commits)):
            scene = scene_for(commits, i, totals, group_rects, layout)
            scenes.append(scene)
            (d / f"frame-{i+1:02d}.svg").write_text(render(scene, pr))
        (d / "scenes.json").write_text(json.dumps(scenes, indent=2))

        groups = sorted({group_of(p) for p in layout})
        summary.append(
            {
                "pr": pr,
                "frames": len(commits),
                "files": len(layout),
                "churn": sum(totals.values()),
                "groups": groups,
            }
        )
        print(
            f"PR {pr}: {len(commits)} frame(s), {len(layout)} files, "
            f"{sum(totals.values())} lines, crates: {', '.join(groups[:5])}"
        )

    (args.out / "summary.json").write_text(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
