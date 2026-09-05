# README feature animations — plan

All but one of these are **shipped** and embedded in the README. Each entry names
the README slot it lands in, what the clip must prove, and how it was captured,
ranked by how much it explains per kilobyte. The one that is not shipped (#11,
voice) says why.

The shipped clips are **screen recordings of the real app** — a scratch portal
instance driven by headless Chrome — not illustrations. The harness that shoots
them lives in [`docs/media/capture/`](media/capture/README.md); re-run it when
the UI moves.

## House rules

- **Every animation is bytes on the landing page.** The current set is 2.6 MB
  across ten clips; keep new ones under ~400 KB and re-encode an old one rather
  than letting the total creep. `encode.sh` at `-q:v 50`–`60` is the usual knob,
  and shortening the clip beats lowering quality on text.
- **Format: animated WebP** for UI capture, **animated SVG** for terminal casts
  and schematics. WebP measured ~10× smaller than APNG at the same quality on
  these clips (283 KB vs 3.0 MB for the permission card), and GIF's 256-color
  palette bands badly on the portal's dark gradients. Repo-relative `.mp4` does
  not render in a GitHub README — don't plan around it. `encode.sh` emits an
  APNG alongside the WebP if you ever need the fallback.
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
| Browser UI | `puppeteer-core` + CDP `Page.startScreencast` → `ffmpeg` | Repeatable clicks and waits; frame-accurate marks for trimming |
| Schematics | Hand-written SVG with SMIL/CSS | Kilobytes, crisp at any zoom, animates as an `<img>` on GitHub |

Palette for anything hand-drawn: the portal's Tokyo Night — background
`#1a1b26`, text `#c0caf5`, muted `#565f89`, accents `#7aa2f7` blue,
`#9ece6a` green, `#f7768e` red, `#e0af68` orange, `#bb9af7` purple.

---

## Ranked candidates

### 1. Agent serves a site → forwards it → it renders in the portal ✅ shipped

`docs/media/feature-port-forward.webp` — 400 KB, 11.9 s. Shot by `cap-forward.js`.

**Slot:** Features ▸ Port forwarding. **~12 s, browser capture.**

The whole arc, starting **inside the transcript**: the agent is asked to serve a
directory and forward it, starts a `python3 -m http.server` (tool card), runs
`agent-portal forward 8899` (tool card, real CLI, real URL in the output), and
the chip appears in the session header. Clicking it genies open the floating
preview with the site live inside the portal — and a link click *inside* the
panel navigates it, which is what proves it is a tunnel and not a screenshot.

The site is the **rizzma crate's rustdoc**: a JS-driven app with search and
navigation, so it exercises the tunnel rather than serving a static page.

This is the single highest-value clip — it demonstrates the tunnel, the CLI, the
chip, and the in-portal preview in one unbroken shot, and it is the feature
nothing else in this space has.

The earlier cut opened on a **flat red** chip (nothing listening) and let the
probe flip it green when the server came up. Starting from the agent's own
commands tells a better story, so the health beat is now only in the prose; if
you re-shoot and want it back, register the forward before starting the server —
the CLI even prints `origin: nothing is listening on 127.0.0.1:8899` when you do.

### 2. One agent messages another ✅ shipped

`docs/media/feature-agent-message.webp` — 252 KB, 7.2 s. Shot by `cap-message.js`.
Shipped as a single dashboard view (message card landing, recipient going to
work) rather than the split terminal/dashboard composite described below.

**Slot:** Features ▸ Agents that talk to each other. **~7 s, split capture.**

Left half a terminal (VHS): `agent-portal message list`, then
`agent-portal message send <id> "PR is up — review the auth boundary"`. Right
half the dashboard: the session rail plays its broadcast arc from sender pill to
recipient pill, and the message lands as a turn in the other session.

Proves the multi-agent workflow is real plumbing, not a diagram.

### 3. A decision arrives as a form ✅ shipped

`docs/media/feature-permission-card.webp` — 283 KB, 10.4 s. Shot by
`cap-permission.js`. Runs longer than planned because the beats before the card
(typing, the agent reading the file, the proposed diff) earn their seconds.

**Slot:** Features ▸ Rich rendering. **~5 s, browser capture.**

An agent hits a permission boundary; the transcript renders a click-to-answer
card; the user picks an option; the agent continues in the same shot. Optionally
cross-fade to an `AskUserQuestion` multi-select card.

Smallest clip on the list and it lands the "decisions, not walls of text" claim
instantly.

### 4. Launch a session from the browser ✅ shipped

`docs/media/feature-launch-session.webp` — 129 KB, 11.4 s. Shot by `cap-launch.js`.

**Slot:** Features ▸ One dashboard, many agents. **~8 s, browser capture.**

Dashboard → launch dialog → pick machine, directory, agent, model, "new
worktree" → a session pill slides into the rail → output starts streaming.

Answers the question a first-time reader actually has: *how does an agent get
onto my machine, and what do I have to type?* (Nothing.)

### 5. Install → login ✅ shipped


`docs/media/feature-install-cast.webp` — 230 KB, 8.6 s. Shot by `cap-cast.js`,
which records a terminal page replaying **real captured output** from running the
production install script and `agent-portal login` with a scratch `HOME`. It ends
at the device-code prompt: approving needs a real browser login, and
`service install` would clobber the capture machine's own service unit.
**Slot:** Quick Start. **~8 s, VHS terminal cast, animated SVG.**

`curl … | bash`, `agent-portal login` showing the device code, `agent-portal
service install` reporting the unit is up. Fully scriptable, cheapest clip on
the list, and it makes the three-command onboarding feel as short as it is.

### 6. Desktop and phone, one session ✅ shipped


`docs/media/feature-desktop-phone.webp` — 400 KB, 9.7 s. Shot by `cap-handoff.js`.
Shipped as *both panes live at once* rather than a lid-close metaphor — the phone
asks the question and both panes stream the answer in step, which is the same
claim without staging anything. Needs two separate browsers (see the capture
README).
**Slot:** Features ▸ Sessions from anywhere. **~10 s, composite.**

A laptop viewport with a session streaming; the lid "closes" (viewport dims);
a phone viewport picks up the same session mid-stream, transcript intact, and
answers a pending prompt from the phone.

The strongest emotional pitch in the product, and the hardest to stage — two
synchronized captures composited side by side. Consider a schematic animated SVG
version (watermark → replay) if the real capture proves fiddly.

### 7. `agent-portal show` puts a figure in the transcript ✅ shipped


`docs/media/feature-show-media.webp` — 340 KB, 10.3 s. Shot by `cap-media.js`.
The figure arrives as a poster; pressing play mounts the runtime and the
waveforms travel.
**Slot:** Features ▸ Rich rendering (or the media docs). **~6 s.**

Terminal `agent-portal show figure.riz` on the left; the interactive portable
figure appearing inline in the transcript on the right, with a cursor rotating
or scrubbing it to show it is live, not a screenshot.

### 8. Live turn metrics ✅ shipped


`docs/media/feature-turn-metrics.webp` — 91 KB, 6.5 s, tight crop. Shot by
`cap-metrics.js`. **Retargeted:** there is no cost ticker in the top bar in the
current UI (cost lives on the Performance page and in history), so the clip
shows the metric sparkline building and the picker switching it to cache-hit
rate.
**Slot:** Features ▸ Cost and performance visibility. **~4 s, tight crop.**

The per-session cost badge shaking as it increments, and the rail sparkline
growing a new bar per turn. Tiny crop, tiny file, high charm.

### 9. Nav mode ✅ shipped


`docs/media/feature-nav-mode.webp` — 204 KB, 7.5 s. Shot by `cap-nav.js`, with a
key-cap overlay drawn by the harness because headless capture has no visible
keyboard.
**Slot:** Features ▸ Sessions from anywhere. **~6 s, browser capture with a
key-cap overlay.**

`Ctrl/Cmd+K` → the rail enters nav mode → `w` jumps to the session waiting on
input → `Enter` accepts. Keystrokes drawn as key caps in the corner, since the
motion is meaningless without them.

### 10. Two agents, two renderers ✅ shipped (Muse pending)


`docs/media/feature-multi-agent.webp` — 161 KB, 9.5 s. Shot by `cap-agents.js`.
**Claude and Codex only.** Muse sessions register and spawn
`muse exec --json` per turn, but in the scratch environment the process never
emitted a journal record, so there was nothing to film; adding the demo
directories to muse's `trust.json` did not change it. Worth revisiting — the
third protocol shape is the point of the clip.
**Slot:** Features ▸ One dashboard, many agents. **~6 s, cross-fade.**

The same dashboard cross-fading between a Claude session, a Codex session, and a
Muse session, pausing on the tool card each protocol produces. Shows breadth
without three separate clips.

### 11. Voice to prompt ⛔ not shippable headlessly


No honest path on a capture box: the Web Speech API needs a real microphone and
Chrome's speech service, and there is no TTS installed to feed
`--use-file-for-fake-audio-capture`. Filming the UI and typing the transcript by
hand would be fabricating the feature's output. Shoot this one by hand, or with a
configured `PORTAL_STT_BACKEND` and a recorded WAV.
**Slot:** Features ▸ Voice input. **~6 s.**

Mic button pressed, live waveform, transcript filling in word by word, edit,
send. Best captured with a sentence full of the jargon the hosted providers get
right and the browser API mangles (`clippy`, `Diesel`, a branch name).

### 12. Architecture packets in flight ✅ shipped


`docs/media/architecture.svg` — 5.7 KB, hand-authored SMIL. Replaces the Mermaid
diagram in the README: it shows the same structure plus which way data moves —
session WS up, client WS down, the forward tunnel running the other way, and a
push peeling off to the phone.
**Slot:** Architecture. **Hand-written animated SVG, ~15 KB.**

The existing Mermaid diagram, redrawn as an SVG where dots travel the edges:
agent → launcher → server → browser, a forward tunnel dot running the other way,
a push notification peeling off to the phone. Replaces a static diagram with one
that shows which way data moves, at essentially no file-size cost.

---

## Status

**Shipped:** everything except #11 — ten clips plus the architecture SVG,
2.6 MB total.

**Open:**

- **#11 voice** needs a machine with a microphone or a configured STT backend.
- **#10 Muse** — the clip ships with Claude and Codex; Muse still needs to be
  made to emit in a scratch environment before its renderer can be filmed.
- The forward clip's red → green health beat is still only in prose (see #1).
