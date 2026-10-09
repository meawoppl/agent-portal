# Aperture Science Agent Portal exhibit

`/aperture` is a public, narrated, interactive introduction to Agent Portal. It
uses the normal Yew frontend and needs no backend API, login, or agent processes.
The four chambers demonstrate selecting a machine, opening an agent-hosted
website, choosing a collaborator's role, and sending work between agents.

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
- `frontend/src/pages/aperture/scenes.rs`: four interactive observation windows.
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
```

It drives all four experiments, interacts with the real sample iframe, opens an
observer invitation, changes roles, completes and resets the exhibit, checks
five viewport widths and reduced motion, and fails on browser errors. Screenshots
are written under `$DEMO_ROOT/aperture-qa/`. It expects Chrome at
`/usr/bin/google-chrome`, like the other capture scripts.
