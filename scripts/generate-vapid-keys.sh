#!/bin/bash
# Generate a VAPID application-server key pair for Web Push (mobile-apps plan
# §8.3, work item C3).
#
# Emits two env vars in the canonical VAPID format — URL-safe base64, no
# padding — for a P-256 key pair:
#
#   PORTAL_VAPID_PRIVATE_KEY   private scalar; the backend signs pushes with it
#                              (WebPushTransport). Keep secret.
#   PORTAL_VAPID_PUBLIC_KEY    uncompressed public point; served to browsers as
#                              the `applicationServerKey` and returned by
#                              GET /api/push/vapid-key.
#
# Both must come from the SAME run — the browser subscription is bound to the
# public key, and the server's signature must match it. Requires `openssl`.
#
# Usage:
#   ./scripts/generate-vapid-keys.sh            # print export lines
#   ./scripts/generate-vapid-keys.sh >> .env    # append to a dotenv file

set -euo pipefail

if ! command -v openssl >/dev/null 2>&1; then
    echo "error: openssl not found on PATH" >&2
    exit 1
fi

tmp_pem="$(mktemp)"
trap 'rm -f "$tmp_pem"' EXIT

# P-256 (prime256v1) private key — the curve VAPID mandates.
openssl ecparam -name prime256v1 -genkey -noout -out "$tmp_pem" 2>/dev/null

b64url() {
    # base64 -> URL-safe, strip padding.
    openssl base64 -A | tr '+/' '-_' | tr -d '='
}

# Private scalar: SEC1 DER for a P-256 key is `30 77 02 01 01 04 20 <32 bytes>`,
# so the raw 32-byte scalar starts at offset 7.
private_key="$(openssl ec -in "$tmp_pem" -outform DER 2>/dev/null \
    | dd bs=1 skip=7 count=32 2>/dev/null | b64url)"

# Public point: SubjectPublicKeyInfo DER ends with the 65-byte uncompressed
# point (0x04 || X || Y).
public_key="$(openssl ec -in "$tmp_pem" -pubout -outform DER 2>/dev/null \
    | tail -c 65 | b64url)"

echo "export PORTAL_VAPID_PRIVATE_KEY=$private_key"
echo "export PORTAL_VAPID_PUBLIC_KEY=$public_key"
