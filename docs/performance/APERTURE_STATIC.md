# Standalone Aperture exhibit

The public exhibit is built independently by `aperture-frontend`, reusing the
existing authored modules and provenance assets. It has no Portal API or auth
client, and the demonstration iframe retains `sandbox="allow-scripts"`.

Destination: `https://meawoppl.github.io/agent-portal/`. The dedicated
`Aperture Pages` workflow publishes only this exhibit, not a Portal dashboard.
The normal application keeps its existing assets until this destination has
been deployed and checked; removal and legacy redirects are the next step.

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
