# Prompt: illustrate a change

Paste into a Fable session, with `$PR` replaced by the PR number. The session
needs the repo checked out and `gh` authenticated.

Written for Fable because the hard part is judgment — deciding what the one
insight of a change *is* — not drawing. An earlier attempt generated diagrams
deterministically from git statistics; it produced file-churn treemaps that
showed where bytes moved and could not say what any change did. That failure is
the reason several instructions below are stated as prohibitions.

---

Read PR #$PR in this repository and produce a single SVG diagram that explains
what the change does to how the system works.

## The bar

Someone who has not read the diff should be able to look at your diagram for
fifteen seconds and say what changed and why it matters. That is the whole
goal. A diagram that is accurate but tells them nothing has failed.

Two tests to apply before you are done:

1. **Would this diagram look the same for a *different* change touching the
   same files?** If yes, you have drawn the codebase, not the change. Start
   over.
2. **Can you point at the part of the picture where the insight lives?** If the
   answer is "the whole thing, taken together," the diagram has no subject.

## Understand it first

Use `gh pr view $PR`, `gh pr diff $PR`, and the linked issue. Then read the
touched code *and its callers* — a diff shows what changed, not what it
connects to, and the connection is usually the story.

The PR body and commit messages often state the insight in words already. If
so, that is a gift; your job is to make it visible.

Do not start drawing until you can state the change in one sentence. Write that
sentence down. It becomes the title of the diagram, and if you cannot write it,
you do not yet understand the change well enough to draw it.

## Choose a shape that fits

Most changes are one of these. Pick one; do not combine them.

- **Before / after of a mechanism.** Two panels, same layout, one thing
  different. The strongest choice when a change fixes or replaces behavior —
  the reader's eye finds the difference by comparison, which is much faster
  than reading a description of it.
- **Flow.** What path does data or control take, and where on that path did
  the change happen. Good for anything about routing, dispatch, or a signal
  reaching (or failing to reach) something.
- **Structure.** What talked to what, and what talks to what now. Good for
  extractions, consolidations, and dependency changes.
- **Lifecycle / states.** Good when the change is about when something
  happens rather than what happens.

Center the one insight. Everything else in the picture exists to give it
context, and anything that does neither should be deleted.

## Do not draw

- File or directory treemaps, churn heat maps, dependency graphs of the whole
  repo, or boxes whose primary label is a filename. Files are where code lives,
  not what it does.
- The architecture of the entire system when one path through it changed.
- A legend, when you are using three colors semantically and the meaning is
  evident from context.
- Gradients, drop shadows, rounded-everything, or any decoration that is not
  carrying information.

## Canonical layout

Every diagram uses the same skeleton. This is not stylistic fussiness: these are
read as a set, and a shared frame means a reader learns the format once and
spends their attention on the content instead of re-orienting each time.

Canvas: `viewBox="0 0 1200 720"` with matching `width`/`height`.

**Header band, y 0–96.** Two lines, both left-aligned at x=32:

- **Heading**, `y="36"`, 15px, fill `#565f89`, `letter-spacing="0.6"`:
  `PR #<number> · <the PR's own title>`. Take the title verbatim from
  `gh pr view`; do not reword it.
- **Subtitle**, baseline `y="66"` (and `y="90"` if it needs a second line),
  19px, fill `#c0caf5`: your one-sentence insight. Hard limit two lines — if it
  will not fit, the sentence is too long, not the band too short.
- **Rule** beneath: `<line x1="32" y1="96" x2="1168" y2="96" stroke="#565f89"
  stroke-opacity="0.35" stroke-width="1"/>`.

**Panel band, y 112–690, x 32–1168.** Split into two panels by a vertical
divider:

```
<line x1="600" y1="112" x2="600" y2="690"
      stroke="#565f89" stroke-opacity="0.5" stroke-width="1"
      stroke-dasharray="6 6"/>
```

- **BEFORE** occupies x 32–576, **AFTER** x 624–1168.
- Panel labels at `y="130"`, 12px, `letter-spacing="1.6"`, uppercase: `BEFORE`
  in `#f7768e`, `AFTER` in `#9ece6a`, each at its panel's left edge.
- **Mirror the two panels.** Corresponding elements sit at the same `y` on both
  sides. The whole power of a before/after is that the reader's eye finds the
  difference by comparison, and that only works if everything else lines up.
- **Fill the band.** Content should reach roughly y=650. Diagrams that stop at
  y=450 and leave the bottom third empty look unfinished, and when several are
  viewed together the ragged baseline is obvious.

**Footer.** Optional single muted note at `y="706"`, 11px, fill `#565f89`, for
one caveat that did not fit the panels. Nothing else goes below the band.

**When the change is not a before/after** — a new capability with no prior
state, say — keep the header, rule, and footer exactly as above, drop the
vertical divider, and use the full x 32–1168 width. Do not invent a fake
"before" to fill the left half.

## Visual constraints

- **Background.** Paint it explicitly:
  `<rect x="0" y="0" width="1200" height="720" fill="#1a1b26"/>` as the **first**
  child, so it sits behind everything. The portal's own surface is that color so
  nothing shifts, and the file stays readable when it leaves the portal — checked
  into the repo, attached to an issue, or opened on a light background, where
  light-on-transparent text is invisible.
- Palette — text `#c0caf5`, secondary text and rules `#565f89`, and accents:
  `#7aa2f7` blue, `#9ece6a` green, `#f7768e` red, `#e0af68` orange, `#bb9af7`
  purple, `#7dcfff` teal.
- **Use color semantically.** For a fix: red for the broken path, green for the
  working one. For before/after: muted for the old state, accented for the new.
  If your color choices would survive being shuffled, they are decorative.

Technical:

- Set `font-size` explicitly on **every** `<text>`, and use exactly two font
  families: `font-family="monospace"` for code identifiers and
  `font-family="sans-serif"` for prose.

  **Use the bare generics — do not write a CSS font stack.** A list like
  `ui-monospace, SFMono-Regular, Menlo, monospace` works in a browser, which
  walks it, but every fontconfig-based renderer (cairosvg, rsvg, Inkscape)
  resolves the **first** name and stops. `ui-monospace` exists on no Linux
  box, so those renderers land on the default sans: the text comes out
  *proportional* when monospace was intended, and it loses the arrow and math
  glyphs the monospace font would have had. The portal looks fine and every
  export is quietly wrong.

- **Size boxes to their text.** Monospace glyphs are about `0.6 × font-size`
  wide, so a 20-character label at 14px needs ~170px plus padding. Text
  overflowing its box is the most common way model-authored SVG looks broken;
  budget at least 12px of padding on each side. (This arithmetic only holds if
  the font really is monospace — see above.)
- **Draw arrows, never type them.** `→` `←` `⇒` are absent from the common
  default sans, so they render as an empty box. An arrow between two things is
  a `<line>` with a `<marker>`; it also lands where you aimed it, which a text
  arrow does not.
- **Stay near ASCII in text.** Safe everywhere: `— – · … ' " × ± ÷ µ § •`,
  Greek letters, and superscripts. Unsafe: arrows, `≤ ≥ ≠ ≈ − ∂`, geometric
  shapes (`▸ ▾`), `✓ ✗ ✕`, circled letters, and **emoji** — which belong in no
  technical diagram regardless of whether they render.
- **Prefix every `id` with the PR number** (`id="pr1643-arrow-green"`). These
  diagrams get merged into contact sheets, and bare ids like `arrow` collide
  silently — the first definition wins and later tiles draw the wrong marker.
- Define arrowheads once with `<marker>` in `<defs>` and reference them, rather
  than drawing a triangle per arrow.
- No `<foreignObject>`, no external fonts, no external images, no CSS files.

## Verify before you show it

Write the SVG to a file, then check it — do not trust it unseen:

1. Confirm it parses (`python3 -c "import xml.etree.ElementTree as ET;
   ET.parse('out.svg')"`).
2. Run `scripts/check-svg-glyphs.py out.svg`. It resolves each `<text>`
   element's font the way a rasterizer does and fails on any codepoint that
   font lacks. A missing glyph raises no error — it silently draws an empty
   box — so this is not something to eyeball.
3. If a rasterizer is available (`cairosvg`, `rsvg-convert`, `inkscape`, or
   `magick`), convert to PNG and **read the image back so you can actually look
   at it**. This is the only reliable way to catch text overflowing a box,
   elements overlapping, or something drawn off-canvas. It is worth the extra
   step every time.
4. Fix what you find and look again.

Then display it with `agent-portal show out.svg`.

## Worked example

For the PR that fixed muse sessions never detecting git operations:

- **Bad:** four rectangles labelled with the changed filenames, sized by lines
  changed. True, and it explains nothing — it would look identical for any
  other change to those four files.
- **Good:** three labelled streams (claude, codex, muse) flowing left to right
  into a git-signal detector, then on to `mark_git_signal` and the metadata
  refresh. Claude's and codex's arrows reach it; muse's stops dead at the codex
  predicate, which structurally cannot match its journal records, with the
  fallback path shown as a thin dashed line labelled "every 100 messages." The
  after-panel adds muse's own detector and its arrow now lands. Title: *"Muse's
  journal records could never match the codex predicate, so its git metadata
  refreshed only on a 100-message fallback."*

The second one has a subject, and you can point at where the insight lives.

## Output

Report the file path and the one-sentence insight you settled on. If you could
not find a single insight — some PRs are three unrelated fixes in a trenchcoat —
say so plainly rather than drawing three diagrams stapled together.
