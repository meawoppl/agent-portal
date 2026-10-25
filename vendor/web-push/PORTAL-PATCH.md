# Local security patch to web-push 0.11.0

Source: crates.io web-push 0.11.0, checksum
`d5c305b9ee2993ab68b7744b13ef32231d83600dd879ac8183b4c76ae31d28ac`.
Apache-2.0 license retained in LICENSE. Test resources are upstream fixtures.

Changes: jwt-simple 0.12.11 -> 0.15; gate HTTP-client-only helpers/imports
behind their existing client features so the no-client build is warning-free.
No message, encryption or signing behavior is intentionally changed.

The published web-push dependency pins jwt-simple 0.12, retaining crypto-bigint
0.5.5 affected by RUSTSEC-2026-0387. jwt-simple 0.15 uses the patched arithmetic
series. Remove this patch when a published web-push release supports it.
