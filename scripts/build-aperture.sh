#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root/aperture-frontend"
export APERTURE_PORTAL_URL=https://txcl.io/dashboard
trunk build --release --public-url "${APERTURE_PUBLIC_PATH:-/agent-portal/}"
# These two static documents also remain in the old Portal bundle until the
# external deployment is verified. Rewrite only the standalone output links.
python3 - <<'PY'
from pathlib import Path
for name in ('trailer', 'evidence'):
    page = Path('dist/aperture-assets') / name / 'index.html'
    page.write_text(page.read_text().replace('href="/aperture"', 'href="../../"'))
PY
