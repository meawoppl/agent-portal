#!/usr/bin/env bash
# Claude Code re-populates ~/.claude.json's oauthAccount (including the account
# email) at startup, and its first-turn thinking often recites it. Scrub right
# before each launch, and always re-check the transcript afterwards.
python3 - "${DEMO_ROOT:-/tmp/readme-demo}/home/.claude.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
oa = d.get('oauthAccount') or {}
for k in ('emailAddress', 'displayName', 'fullName', 'organizationName'):
    oa.pop(k, None)
d['oauthAccount'] = oa
json.dump(d, open(p, 'w'), indent=1)
print('scrubbed', p)
PY
