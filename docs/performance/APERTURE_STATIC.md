# Standalone Aperture exhibit

The public exhibit is built independently by `aperture-frontend`, using its own authored modules and the existing provenance assets. It has no Portal API or auth
client, and the demonstration iframe retains `sandbox="allow-scripts"`.

Destination: `https://meawoppl.github.io/agent-portal/`. The dedicated
`Aperture Pages` workflow publishes only this exhibit, not a Portal dashboard.
The normal application redirects `/aperture`, `/aperture-assets/*`, and
`/aperture-demo/*` to this site. Browser navigation preserves observer hashes.
The backend build rejects accidental inclusion of these directories in its
embedded distribution. The external destination must be verified before this
cutover is merged.

## Build and preview

```
scripts/build-aperture.sh
python3 -m http.server --directory aperture-frontend/dist 8080
```

The default build uses `/agent-portal/` for GitHub Pages. For a root local preview:

```
APERTURE_PUBLIC_PATH=/ scripts/build-aperture.sh
python3 -m http.server --directory aperture-frontend/dist 8080
```

Open `http://localhost:8080/`. The exhibit's exit links return to
`https://txcl.io/dashboard`. Observer invitations preserve the current exhibit
URL and hash; they do not create real Portal session permissions.

The browser check serves the default build at the same project-site prefix as
Pages, exercises all sectors, narration, video, the sandbox, observer links,
film playback, and sample downloads, and rejects Portal API requests:

```
python3 scripts/test-aperture-static.py
python3 scripts/test-aperture-static.py --base https://meawoppl.github.io/agent-portal/
```

Python Playwright and Chromium must be installed. Artwork, captions, source
files, recorded evidence, licenses and provenance remain under
`frontend/assets/aperture`. Static HTML return links are rewritten in the
standalone output only, leaving the current embedded exhibit working during
migration.

## Bundle measurements

Same local toolchain (rustc 1.96.0, Trunk 0.22.1, release profile with `-Oz`
wasm-opt), before/after this cutover:

| Optimized distribution | Before | After |
| --- | ---: | ---: |
| Files | 169 | 63 |
| Total bytes | 35,312,255 | 7,217,492 |
| Exhibit media bytes | 27,961,496 | 0 |

The standalone exhibit remains approximately 28.3 MB with all authored media.
Portal's distribution shrinks by 28,094,763 bytes (79.6%). This is a measured
asset/input reduction, not an assertion that media caused the entire native
compile delay. The two local builds had different cache states, so their wall
clock times are not a controlled build-time comparison.

Pre-cutover production baseline: image `2.15.1907`, revision
`d3fa47887b6cbe62048439641e29f6414af8684f`: backend executable 54,527,832 bytes;
uncompressed Docker image 196,883,696 bytes. Post-cutover image measurements
will be recorded after the production build completes. Native profile/cache
experiments are tracked separately in #2448 and #2449.

Separating all frontend assets from the executable could further decouple UI
changes from native linking, but is not necessary for this migration. It would
require a separate packaging, cache-coherence and fallback design.
