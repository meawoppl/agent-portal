# Aperture Science Agent Portal exhibit

`/aperture` is a public, narrated, interactive introduction to Agent Portal. It
uses the normal Yew frontend and needs no backend API, login, or agent processes.
Eight sectors progress from code generation to remote test benches, rich web
previews, mechanical CAD, electrical design, logic, controls and engineering
briefings. The narrator is the agent composing the film from inside Portal.
Making-of, exploration mission and final reassignment panels close the story.
The fictional “human meat proxy” retirement program remains the running joke;
setup, forwarding and inter-agent messages are the agents' work.

All machine connections, memberships, and agent messages are explicitly labelled
simulations. The companion-calibration website is a real interactive, sandboxed
page; it is a local sample rather than a connection to a remote service. Demo
invitations open the same exhibit in an observer context. They contain only a
role fragment and never contain session identifiers, credentials, or real
membership grants. Hosting the exhibit behind a private forward still requires
the recipient to have access to that forward.

## Try it

Build and serve the frontend using the normal development process, then visit
`http://localhost:<port>/aperture`. For a frontend-only preview:

```sh
cd frontend
trunk serve --port 8792
```

The **Watch the orientation film** link opens an original narrated orientation film at
`/aperture-assets/trailer/index.html`. Its soundtrack drives the animation clock,
so pause, seek and replay keep narration and captions together. The finite
soundtrack is preloaded as a Blob so seeking works even on embedded static hosts
that do not provide HTTP byte ranges. Capture mode (`?capture`) exposes
`window.seek(seconds)` for deterministic frame rendering and skips audio loading.

Publish one film per version: the full cut. Do not produce separate teaser or
short-cut exports or offer them in watch/download links. The existing `trailer/`
asset path is retained for compatibility; its player presents the full film.

The exhibit works without an API server. Its **Exit to Agent Portal** and
**Open Agent Portal** links lead to `/dashboard`, which requires a normal backend.
If your environment defines `NO_COLOR=1` and Trunk rejects it, run Trunk with
`env -u NO_COLOR`.

Select **Enable audio** to hear the facility announcer. Audio is off by default;
all narration is captioned. Changing chambers or muting stops the previous line.
Animations honor reduced-motion preferences. The whole exhibit works with
keyboard controls, touch, and sound disabled.

## Implementation

- `frontend/src/pages/aperture/model.rs`: reducer-owned experiment state, bounded
  navigation, completion, role changes, and reset.
- `frontend/src/pages/aperture/mod.rs`: public route, chamber navigation,
  captions, invitations, audio controls, and cancellable collaboration timers.
- `frontend/src/pages/aperture/scenes.rs`: review, transit and web/sharing observations in the first three sectors.
- `frontend/src/pages/aperture/plugins.rs`: five separate engineering sectors with real recorded evidence,
  with source links and capability boundaries.
- `frontend/src/pages/aperture/engineering.rs`: software origin, recorded tool panels, making-of and exploration conclusion.
- `frontend/assets/aperture/evidence/`: compact workbench recordings, sources, results and a browsable provenance page.
- `frontend/src/pages/aperture/audio.rs`: one opt-in narration channel and a
  synthesized transit sound. Audio failure never prevents using the exhibit.
- `frontend/styles/aperture.css`: scoped visual theme; does not recolor the
  dashboard.
- `frontend/assets/aperture/`: original signage, fonts and narration. Trunk
  publishes this directory at `/aperture-assets/`.
- `frontend/assets/aperture-demo/companion.html`: sandboxed interactive sample,
  served at `/aperture-demo/companion.html`.

The captions and audio filenames come from `aperture/lines.json`. Keep narration
text and rendered recordings together. Barlow Condensed is bundled under its SIL
Open Font License (`fonts/OFL.txt`). The exhibit is an unofficial fan homage,
with original artwork and narration, and does not imply Valve affiliation.

## Verification

```sh
cargo test -p frontend --lib
cargo clippy -p frontend --target wasm32-unknown-unknown -- -D warnings
cargo fmt --check
cd frontend && trunk build
```

The browser smoke test uses the existing capture harness's Puppeteer convention:

```sh
npm install --prefix /tmp/readme-demo puppeteer-core
DEMO_ROOT=/tmp/readme-demo DEMO_URL=http://localhost:8792 \
  node docs/media/capture/check-aperture.js
DEMO_ROOT=/tmp/readme-demo DEMO_URL=http://localhost:8792 \
  node docs/media/capture/check-aperture-film.js
```

It drives the code prologue, all eight sectors, making-of and mission, interacts with the real sample iframe, opens an
observer invitation, changes roles, completes and resets the exhibit, checks
five viewport widths, reduced motion, audio cancellation, clipboard copying and
its permission-denied fallback, and fails on browser errors. The film check also
verifies actual playback, every measured caption phrase, stationary closing
titles, seeking backwards with captions, replay, mobile stage
bounds, full-size portrait captions/provenance, and asset loading. Screenshots
are written under `$DEMO_ROOT/aperture-qa/`. It expects Chrome at
`/usr/bin/google-chrome`, like the other capture scripts.

## Eight sectors and evidence

The source for the exploration motivation is the [public Cosmic Frontier Labs
mission](https://www.cosmicfrontier.org/); no separate formal charter is quoted.
The complete evidence page is `/aperture-assets/evidence/index.html`.

1. **Code generation:** agent-written software and peer review. The review
   exchange is illustrative; it never merges a real PR.
2. **Remote test bench:** recorded CF-00020-01/Nucleo v4.1.0 tests supplied by
   the FSM bench agent. Physical 800 µrad circle, nominal 2 kHz telemetry;
   13.732 µrad RMS across the analysis window, 16.472 µrad on-circle only.
   No new hardware actuation for the film; single-board results.
3. **Web proxy:** real interactive companion app, including actual cake render;
   forwarding and invitations are explicitly isolated demo state.
4. **yapCAD:** original cake DSL and native retained runs, filling 0.8→6 mm,
   frosting 3→7 mm. Downloadable assembled STL is 108×108×60 mm, watertight,
   consistently oriented, one body, 184,967.64 mm³ with a flat z=0 base.
   Geometry validation does not mean a physical print trial was performed.
5. **Kicadmium:** actual bp-test M.2 schematic/PCB and Tesla controller views.
   M.2 `178e51d`→`bb640bd43f68007ceb8ea398f57f6e4a920166df`;
   Tesla board `663fc49`. KiCad 10.0.6 export checks pass ERC/DRC/parity,
   but the final M.2 quality check retains 31 contract-documented errors.
   These are development designs, not fabrication approval. Native model
   licences and manufacturer provenance travel with the source archive.
6. **Visilog:** real counter bench, breakpoint, waves and `$finish`, independently
   checked at 500 ns with zero assertion failures; not timing sign-off.
7. **Unlinked/JLG:** actual own-source SI reconstruction and native simulation,
   then review figures showing a quadrature correction and failed sweep cases.
   These are analytic/simulated results, not hardware measurements. Unlinked's
   supported subset is not a MathWorks-equivalence claim. Proprietary vendor
   source files are not included in the exhibit.
8. **Engineering presentations:** real 27-page JLG engineering deck, including
   risk and provenance pages. Its unrouted board and unmet accelerometer
   requirement are retained, not edited into a success claim.

JLG snapshot: `d875d475416b8601b732042f3767462e73a4f9cd`.
Plugin capability snapshot: `04456e4ff259f006edc1bb2aeaa65d96d9e31f28`.
Opening the public exhibit runs no engineering tool and accesses no real session.

## Film production and incremental rendering

`trailer/index.html`, `film.css` and `film.js` implement one full film. The browser
builds `window.FILM` from the twenty narration lines, with scene boundaries,
caption cues and evidence placements. Caption phrases follow authored sentence
and clause boundaries in `captions.json`, aligned to measured word timestamps;
they are not split by character count or timed proportionally to text length.
Voice and video are preloaded as Blobs so
seeking works on static hosts without byte-range support. Capture mode exposes
`window.seek(t)`; `?capture&external-clips` omits media decoding so original
masters can be composited after the deterministic browser render.

The reproducible helpers are:

- `docs/media/capture/align-aperture-captions.py ASSETS`:
  authored caption phrases aligned to `voice/words.json`; validates the rendered
  audio hash and preserves the exact approved narration text.
- `docs/media/capture/mix-aperture-score.py ASSETS TIMELINE OUTPUT.mp3`:
  original music, transitions and narration on the shared film clock. RMS speech
  detection excludes leading silence and room tails from ducking. The bed is
  held at 0.65 gain during speech, with 25 ms attack, 10 ms lookahead, 100 ms hold
  and 160 ms release. A `.mix.json` report records detected speech spans.
  The voice uses a lower, capped pitch contour; generation details accompany
  the voice assets. Closing titles remain stationary, with whole-scene fades.
- `docs/media/capture/render-aperture-film.py URL OUT ASSETS MASTERS --workers 8`:
  content-keyed scene cache, parallel Chrome capture, and one final composite.
  Uses NVENC by default; `--encoder libx264` is the CPU fallback. A changed
  engineering recording can be recomposited without recapturing base scenes.
- `docs/media/capture/check-aperture-film.js`: real browser playback, seeking,
  all sectors, evidence decode, mobile bounds and download checks.

Python helpers require Playwright/Chrome, NumPy, SciPy, SoundFile and ffmpeg.
Use a disk-backed temporary directory for Chrome when `/tmp` is memory-limited.
Masters and exports remain under ignored `builds/aperture-v*/` directories. The original
voice/art scripts live in `~/aperture-work` on the production host. Captured
workbench segments may be cropped, held or accelerated for readability; the
underlying geometry, checks and recorded data are unchanged.

Keep one full film per version. Do not generate or link short-cut/teaser variants.
