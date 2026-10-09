# Aperture Science Agent Portal exhibit

`/aperture` is a public, narrated, interactive introduction to Agent Portal. It
uses the normal Yew frontend and needs no backend API, login, or agent processes.
The five chambers demonstrate agents working across machines, opening their own
website tunnels, collaborators observing with owner-granted access, direct agent
review, and a five-part plugin design series. The narrator is an agent conducting
a fictional “human meat proxy” retirement program. Operational copy assigns
setup, forwarding, and message delivery to agents rather than asking the human
to act as a terminal or message courier.

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

The **Watch the orientation film** link opens an original 3:26 animated film at
`/aperture-assets/trailer/index.html`. Its soundtrack drives the animation clock,
so pause, seek and replay keep narration and captions together. The finite
soundtrack is preloaded as a Blob so seeking works even on embedded static hosts
that do not provide HTTP byte ranges. Capture mode (`?capture`) exposes
`window.seek(seconds)` for deterministic frame rendering and skips audio loading.

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
- `frontend/src/pages/aperture/scenes.rs`: the original four interactive observation windows.
- `frontend/src/pages/aperture/plugins.rs`: five illustrated plugin departments,
  with source links and capability boundaries.
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

It drives all five experiments and five plugin departments, interacts with the real sample iframe, opens an
observer invitation, changes roles, completes and resets the exhibit, checks
five viewport widths, reduced motion, audio cancellation, clipboard copying and
its permission-denied fallback, and fails on browser errors. The film check also
verifies actual playback, seeking backwards with captions, replay, mobile stage
bounds, and asset loading. Screenshots
are written under `$DEMO_ROOT/aperture-qa/`. It expects Chrome at
`/usr/bin/google-chrome`, like the other capture scripts.

## Plugin replacement series

The fifth chamber is an illustrated research program, not a live plugin run or a
claim that a manufactured robot has passed validation. The departments are:

1. **yapCAD:** parametric gripper geometry, retained builds, contextual feedback,
   revisions, and supported exports. Analytic STEP needs the optional BREP tier.
2. **Kicadmium:** KiCad board review, checks, and fabrication exports; requires
   KiCad and distinguishes layout lint from native ERC/DRC.
3. **Visilog:** Verilog hierarchy, values, waveforms, stepping and breakpoints;
   the project test runner determines pass/fail, not the illustration.
4. **Unlinked:** model diagrams and the supported simulation subset; unsupported
   models do not silently become validated designs.
5. **Engineering Presentations:** repository-backed briefings, reproducible
   figures, and feedback-driven revisions. This is where the agent narrator
   realizes that presenters are also replaceable.

The capability review is grounded in the plugin repository at
[`04456e4`](https://github.com/meawoppl/agent-portal-plugins/tree/04456e4ff259f006edc1bb2aeaa65d96d9e31f28):
the four engineering plugin READMEs and the `engineering-presentations` manifest.
Each exhibit department links to its reviewed source. No new plugin is installed
or executed by opening the public exhibit.
