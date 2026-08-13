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

## Visual constraints

The portal renders SVGs on a **dark `#1a1b26` background**, so:

- **No background rectangle.** Transparent, always. A white `<rect>` fill is
  the single most common way to make a diagram unreadable here.
- Palette — text `#c0caf5`, secondary text and rules `#565f89`, and accents:
  `#7aa2f7` blue, `#9ece6a` green, `#f7768e` red, `#e0af68` orange, `#bb9af7`
  purple, `#7dcfff` teal.
- **Use color semantically.** For a fix: red for the broken path, green for the
  working one. For before/after: muted for the old state, accented for the new.
  If your color choices would survive being shuffled, they are decorative.

Technical:

- `viewBox="0 0 1200 750"` with matching `width`/`height`. Landscape suits the
  portal and reads on a phone.
- Set `font-size` explicitly on **every** `<text>`. Use
  `font-family="ui-monospace, SFMono-Regular, Menlo, monospace"` for code
  identifiers and `font-family="system-ui, -apple-system, Segoe UI, sans-serif"`
  for prose.
- **Size boxes to their text.** Monospace glyphs are about `0.6 × font-size`
  wide, so a 20-character label at 14px needs ~170px plus padding. Text
  overflowing its box is the most common way model-authored SVG looks broken;
  budget at least 12px of padding on each side.
- No `<foreignObject>`, no external fonts, no external images, no CSS files.
  Define arrowheads once with `<marker>` and reference them.
- Aim for 15–25 meaningful elements. Whitespace is not wasted space; a diagram
  with room to breathe reads faster than a dense one.
- Title at the top: your one-sentence insight, not the PR title. Small caption
  bottom-right: `PR #$PR`.

## Verify before you show it

Write the SVG to a file, then check it — do not trust it unseen:

1. Confirm it parses (`python3 -c "import xml.etree.ElementTree as ET;
   ET.parse('out.svg')"`).
2. If a rasterizer is available (`rsvg-convert`, `inkscape`, or `magick`),
   convert to PNG and **read the image back so you can actually look at it**.
   This is the only reliable way to catch text overflowing a box, elements
   overlapping, or something drawn off-canvas. It is worth the extra step every
   time.
3. Fix what you find and look again.

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
