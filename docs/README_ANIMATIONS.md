# README feature animations — plan

The README's Features section is text today. This document proposes the short
looping animations that would carry it, ranked by how much each one explains
per kilobyte, and records the house rules for producing them.

Nothing here is built yet. Each entry names the README slot it lands in, what
the clip must prove, and how to capture it.

## House rules

- **Four in the README, the rest in docs.** Every animation is bytes on the
  landing page. Ship #1–#4 inline plus the terminal cast in Quick Start; link
  the others from the feature docs they illustrate.
- **Format: APNG** (`.png`, animated) or animated WebP for UI capture, **animated
  SVG** for terminal casts and schematics. GIF's 256-color palette bands badly
  on the portal's dark gradients. Repo-relative `.mp4` does not render in a
  GitHub README — don't plan around it.
- **Budget: ≤ 2 MB and ≤ 10 s each**, 900 px wide (capture at 1800 px / 2× DPR
  and downscale), 12–15 fps. Loop cleanly: first and last frame identical.
- **Gentle motion.** A README animation cannot honour `prefers-reduced-motion`,
  so no strobing, no fast cuts, one idea per clip. Give every one a real `alt`
  describing what happens, for the people who have motion disabled at the OS
  level and for screen readers.
- **Staged, not personal.** Capture against a local dev instance with seeded
  data. No real emails, tokens, repo names, or session ids.
- **Store under `docs/media/`**, named `feature-<slug>.png`, with the capture
  script (VHS `.tape` or Playwright `.ts`) checked in beside it as
  `feature-<slug>.tape` / `.ts` so the clip can be re-shot when the UI moves.

## Tooling

| Kind | Tool | Why |
|------|------|-----|
| Terminal casts | [VHS](https://github.com/charmbracelet/vhs) | Scripted `.tape` files — deterministic, re-runnable, no hand-timed typing |
| Browser UI | Playwright script + `ffmpeg`/`gifski` → APNG | Repeatable clicks and waits; no hand-held recording |
| Schematics | Hand-written SVG with SMIL/CSS | Kilobytes, crisp at any zoom, animates as an `<img>` on GitHub |

Palette for anything hand-drawn: the portal's Tokyo Night — background
`#1a1b26`, text `#c0caf5`, muted `#565f89`, accents `#7aa2f7` blue,
`#9ece6a` green, `#f7768e` red, `#e0af68` orange, `#bb9af7` purple.

---

## Ranked candidates

### 1. The forward chip comes alive → preview window

**Slot:** Features ▸ Port forwarding. **~9 s, browser capture.**

Agent runs `agent-portal forward 8899`; the header chip appears **flat red**;
the dev server starts; within one probe interval the chip **breathes green** and
its tooltip names the process; clicking it genies open the floating preview with
the real app inside; `Ctrl-C` on the server and the chip goes red again.

This is the single highest-value clip: it demonstrates the tunnel, the health
probe, the process resolution, and the in-portal preview in one unbroken shot,
and it is the feature nothing else in this space has.

### 2. One agent messages another

**Slot:** Features ▸ Agents that talk to each other. **~7 s, split capture.**

Left half a terminal (VHS): `agent-portal message list`, then
`agent-portal message send <id> "PR is up — review the auth boundary"`. Right
half the dashboard: the session rail plays its broadcast arc from sender pill to
recipient pill, and the message lands as a turn in the other session.

Proves the multi-agent workflow is real plumbing, not a diagram.

### 3. A decision arrives as a form

**Slot:** Features ▸ Rich rendering. **~5 s, browser capture.**

An agent hits a permission boundary; the transcript renders a click-to-answer
card; the user picks an option; the agent continues in the same shot. Optionally
cross-fade to an `AskUserQuestion` multi-select card.

Smallest clip on the list and it lands the "decisions, not walls of text" claim
instantly.

### 4. Launch a session from the browser

**Slot:** Features ▸ One dashboard, many agents. **~8 s, browser capture.**

Dashboard → launch dialog → pick machine, directory, agent, model, "new
worktree" → a session pill slides into the rail → output starts streaming.

Answers the question a first-time reader actually has: *how does an agent get
onto my machine, and what do I have to type?* (Nothing.)

### 5. Install → login → service

**Slot:** Quick Start. **~8 s, VHS terminal cast, animated SVG.**

`curl … | bash`, `agent-portal login` showing the device code, `agent-portal
service install` reporting the unit is up. Fully scriptable, cheapest clip on
the list, and it makes the three-command onboarding feel as short as it is.

### 6. Desktop → phone handoff

**Slot:** Features ▸ Sessions from anywhere. **~10 s, composite.**

A laptop viewport with a session streaming; the lid "closes" (viewport dims);
a phone viewport picks up the same session mid-stream, transcript intact, and
answers a pending prompt from the phone.

The strongest emotional pitch in the product, and the hardest to stage — two
synchronized captures composited side by side. Consider a schematic animated SVG
version (watermark → replay) if the real capture proves fiddly.

### 7. `agent-portal show` puts a figure in the transcript

**Slot:** Features ▸ Rich rendering (or the media docs). **~6 s.**

Terminal `agent-portal show figure.riz` on the left; the interactive portable
figure appearing inline in the transcript on the right, with a cursor rotating
or scrubbing it to show it is live, not a screenshot.

### 8. Cost ticker and sparkline

**Slot:** Features ▸ Cost and performance visibility. **~4 s, tight crop.**

The per-session cost badge shaking as it increments, and the rail sparkline
growing a new bar per turn. Tiny crop, tiny file, high charm.

### 9. Nav mode

**Slot:** Features ▸ Sessions from anywhere. **~6 s, browser capture with a
key-cap overlay.**

`Ctrl/Cmd+K` → the rail enters nav mode → `w` jumps to the session waiting on
input → `Enter` accepts. Keystrokes drawn as key caps in the corner, since the
motion is meaningless without them.

### 10. Three agents, three renderers

**Slot:** Features ▸ One dashboard, many agents. **~6 s, cross-fade.**

The same dashboard cross-fading between a Claude session, a Codex session, and a
Muse session, pausing on the tool card each protocol produces. Shows breadth
without three separate clips.

### 11. Voice to prompt

**Slot:** Features ▸ Voice input. **~6 s.**

Mic button pressed, live waveform, transcript filling in word by word, edit,
send. Best captured with a sentence full of the jargon the hosted providers get
right and the browser API mangles (`clippy`, `Diesel`, a branch name).

### 12. Architecture packets in flight

**Slot:** Architecture. **Hand-written animated SVG, ~15 KB.**

The existing Mermaid diagram, redrawn as an SVG where dots travel the edges:
agent → launcher → server → browser, a forward tunnel dot running the other way,
a push notification peeling off to the phone. Replaces a static diagram with one
that shows which way data moves, at essentially no file-size cost.

---

## Suggested first batch

Ship **#5 (Quick Start)**, **#1**, **#3**, and **#4** first: one terminal cast
and three browser captures, roughly 5 MB total, covering onboarding, the
showpiece feature, the interaction model, and the "how do I start" question.
Add **#2** once the rail broadcast animation is easy to trigger on demand in a
seeded dev instance.
