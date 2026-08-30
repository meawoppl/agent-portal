---
name: visual-pr
description: Render a before/after visual summary SVG for a PR and show it inline in the session. Use when a PR is ready (right after creating it), when the user asks for a "visual PR" / "visual PR review", or to illustrate an existing PR or commit by number.
---

# Visual PR summary

Produce **one SVG** that argues the PR's thesis visually: what was wrong or missing
before, what mechanism the PR adds, and what guarantee holds after. The output is
shown inline in the session with `agent-portal show` the moment the PR is ready.

## Rule zero: the diagram is grounded in the code

**If you didn't read it, don't draw it.** Every box title, field name, function,
filter expression, and claim in the diagram must come from the actual diff and the
surrounding code — never from the PR description alone, and never invented.

Gather ground truth first:

```bash
gh pr diff <N>                     # or: git diff main...HEAD
gh pr view <N> --json title,body   # intent, not truth — verify claims against code
git show main:<file>               # the BEFORE side of a touched file
```

Then read enough of the surrounding code (callers, the enclosing function, the
schema) to state *why* the before behavior was wrong and *how* the after behavior
holds. Use exact identifiers: `run_expired_token_cleanup`, `expires_at < cutoff`,
`proxy_auth_tokens` — a reviewer cross-checking symbols against the diff must find
every one. If a claim can't be verified from the code, it goes in the footer as an
explicit caveat or it doesn't appear.

## The thesis

One or two lines of large near-white text directly under the header. It states the
**claim** of the PR — the behavioral change and its consequence — not a description
of the diff. Formula that works:

> *X used to ⟨flaw, stated concretely⟩; now ⟨mechanism⟩, so ⟨guarantee⟩.*

Bad: "Refactors token cleanup and updates the settings footer."
Good: "Revoked credentials slipped past both cleanup sweeps and lived forever; a
third pass now ages them out through the same seven-day window."

## Canvas and layout

Start from [template.svg](template.svg) in this directory. Fixed geometry:

- `viewBox="0 0 2000 1200"`, full-bleed background `#16161e`, 55px side margins.
- **Header** (y≈52): `PR #NNNN · <title>` — 24px, `#565f89`, letter-spacing 1.
- **Thesis** (y≈100, second line y≈140): 34px, `#e6e9f5`. Hairline `#3d4666`
  underneath at y≈165.
- **Split**: `BEFORE` label (22px bold, letter-spacing 4, `#f7768e`) at x=55,
  y≈212; `AFTER` (same, `#9ece6a`) at x=1040. Dashed vertical divider
  (`#3d4666`, dash `2 6`) at x=1000 from y≈190 to y≈1135.
- Each panel is ~890px wide: BEFORE spans x 55–945, AFTER spans x 1040–1945.
- **Footer** (y≈1172): one muted 22px `#565f89` line — the scope caveat: what the
  PR deliberately does *not* do, or the sharpest limitation. Always present.

## Palette (Tokyo Night) — semantic, not decorative

| Color | Hex | Means |
|---|---|---|
| red | `#f7768e` | defect, dead-end, rejected path, the BEFORE label |
| green | `#9ece6a` | new component, fixed behavior, guarantee, the AFTER label |
| orange | `#e0af68` | caveat, gate, partially-addressed item |
| blue | `#7aa2f7` | component/type/function names in box titles |
| teal | `#7dcfff` | sparing secondary accent (data payloads, links) |
| purple | `#bb9af7` | sparing tertiary accent |
| bright | `#c0caf5` | box titles that aren't code identifiers |
| body | `#a9b1d6` | detail lines inside boxes |
| near-white | `#e6e9f5` | thesis only |
| muted | `#565f89` | annotations, sub-details, footer, labels on arrows |
| border | `#3d4666` | neutral box borders, hairlines, dividers |
| box fill | `#1e202e` | interior of boxes (or `none`) |
| background | `#16161e` | canvas |

Structure that is *unchanged* between the two panels stays neutral (border
`#3d4666` / `#565f89`, body text) — the reader's eye must be pulled only to what
the PR changed. The same box appearing on both sides should look identical except
where the PR touched it.

## Archetypes — pick one per PR

- **Dataflow** (most PRs): boxes = components/tables/functions, arrows = data or
  control flow. BEFORE arrows dead-end in red annotations; AFTER flows through a
  green new box to a green outcome box.
- **Sequence** (locking, ordering, protocol changes): lifelines with activation
  bars, horizontal call/return arrows (solid call, dashed return), red bar for the
  interleaving hazard, green single call replacing two.
- **Timeline** (latency, delays, retention windows): a horizontal axis with ticks,
  events placed on it, shaded windows, ⊗ for the wrong anchor point, ✓ for the
  right one.
- **Checklist verdict** (validation/health-check logic): BEFORE as red `–` items
  ("never checked", "not counted"), AFTER as green `✓` items, each with a muted
  one-line justification, converging on a result box.

Mixing is fine (a dataflow panel ending in a checklist box), but each panel should
read top-to-bottom or left-to-right in one pass.

## Text fitting — budgets, not vibes

This font averages ~0.5×font-size per character. Hard budgets:

| Text | Size | Max chars |
|---|---|---|
| Thesis line (full width, 1890px) | 34px | 105 |
| Box title (870px panel box, 24px padding) | 26px | 62 |
| Box detail line | 23px | 71 |
| Arrow label / annotation | 22px | fits its gap — keep ≤ 40 |
| Footer (full width) | 22px | 165 |

Never let text touch a box edge; 24px interior padding. Split long lines rather
than shrinking the font below 21px. Line spacing inside a box: 36–40px.

Free-floating labels must not cross an arrow's path: compute the label's span
(chars × 0.5 × size from its x) and keep it inside the horizontal gap between
arrows. When in doubt, rasterize and look:
`convert /tmp/visual-pr-<N>.svg /tmp/check.png` then Read the PNG.

## Procedure

1. **Ground truth**: read the diff and surrounding code (rule zero above).
2. **Thesis**: write it as text first; if you can't state the claim in two lines,
   you don't understand the PR yet — go back to the code.
3. **Pick the archetype** and sketch the box list for each panel: BEFORE must show
   *why* it was wrong (the mechanism of the flaw), AFTER *why* it now holds.
4. **Fill the template**: copy `template.svg`, replace the placeholder panels.
   Real identifiers in blue/monospace-flavored titles; semantic colors per the
   table; footer caveat last.
5. **Validate**: `python3 .claude/skills/visual-pr/check_svg.py <file.svg>` —
   fixes anything it flags (parse errors and off-palette colors are hard errors,
   overflow estimates are warnings to eyeball).
6. **Show it**: save as `/tmp/visual-pr-<N>.svg` (keep the repo clean — do not
   commit generated SVGs) and run `agent-portal show /tmp/visual-pr-<N>.svg`.

## Quality bar

- Every identifier in the diagram appears in the diff or the touched files.
- The two panels are structurally parallel — the reader diffs them by eye.
- Red only where something is genuinely wrong; green only where the PR makes it
  right; a diagram that is all green is marketing, not review.
- The footer names a real limitation, not a humble-brag.
- Validator passes clean.
