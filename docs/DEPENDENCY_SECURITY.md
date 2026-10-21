# Dependency security and SDK updates

The repository owner (@meawoppl) owns dependency triage and assigns the author
and reviewer for updates. Review dependency/SDK releases weekly, and respond to
new security findings immediately rather than waiting for the weekly review.
Dependency PRs need the same CI and independent review as application changes;
there is no automatic merge policy.

## RustSec gate

`.github/workflows/security-audit.yml` runs pinned `cargo-audit` 0.22.2 on PRs,
main pushes, daily, and by manual dispatch. It fetches the current RustSec
advisory database and audits the full workspace lockfile without platform
filters. Vulnerabilities fail the job unless a specific reviewed exception is
recorded in `.cargo/audit.toml`. Every ignore must have matching ownership and
review dates in `.cargo/audit-exceptions.toml`; the gate fails after its review
deadline or if its review window exceeds 31 days. Database/network failures also fail; stale data
is not a success fallback. The job needs read-only repository permissions.

Reproduce with:

```sh
cargo install cargo-audit --version 0.22.2 --locked
python3 scripts/check-audit-exceptions.py
cargo audit --file Cargo.lock
cargo tree --locked --target all --invert PACKAGE
```

Informational warnings (including unsoundness, unmaintained packages and yanked
versions) remain visible under cargo-audit's standard policy; they are not
silenced or treated as resolved. Track and review those alongside failures.
The initial audit found patchable `anyhow` and `event-listener` unsoundness,
SQLite-specific Diesel unsoundness, and `glib` desktop unsoundness; #1209 tracks
remaining remediation. A passing job is not a claim that every warning is gone.

This gate covers RustSec's known Rust dependency advisories. It does not audit
npm packages, operating-system TLS libraries, dependency source authenticity,
or undisclosed vulnerabilities. Use upstream release notes and normal code
review as well; a lockfile change is executable dependency code, not clerical
maintenance.

Official tool behavior: [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit)
and its [configuration reference](https://github.com/rustsec/rustsec/blob/main/cargo-audit/audit.toml.example).

## Specific RSA exposure exception

[RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) affects
RSA private-key operations and has no patched release. The locked `rsa` crate
arrives through `backend -> web-push -> jwt-simple -> superboring -> rsa`.
The exception is based on the call path, not on severity or the absence of a
patch:

- `backend/src/push/webpush.rs` uses `VapidSignatureBuilder` to sign VAPID.
- In web-push 0.11.0, `src/vapid/key.rs` wraps `ES256KeyPair` and
  `src/vapid/builder.rs` parses only EC keys. Portal does not select an RSA
  algorithm or call RSA private-key operations through that chain.
- FCM's separate RSA service-account signing uses `jsonwebtoken` 9/ring, not
  the affected `rsa` crate. Do not confuse those two implementations.

Sources: [web-push's VAPID key type](https://docs.rs/crate/web-push/0.11.0/source/src/vapid/key.rs)
and [VAPID builder](https://docs.rs/crate/web-push/0.11.0/source/src/vapid/builder.rs).
Owner: @meawoppl. Reviewed 2026-10-21; review again by **2026-11-21**, and whenever
web-push, jwt-simple, their features or signing call sites change. Remove the
exception when the dependency is patched/removed. If an RSA private-key path
is introduced, this rationale no longer holds: remove the exception and fix
or replace the path before shipping. No TLS advisory exceptions are allowed by
this assessment.

## Update procedure and SDK ownership

The Portal maintainer owns the consumer update. SDK type/decoder changes belong
upstream in [rust-code-agent-sdks](https://github.com/meawoppl/rust-code-agent-sdks),
which this project also maintains. The root `Cargo.toml` owns requirements for
`claude-codes`, `codex-codes` and `muse-codes`; `Cargo.lock` owns the exact resolved
versions. Do not duplicate version numbers in process documentation.

1. Read the upstream change log/advisory and identify the actual dependency
   path, features and supported targets. Prefer the smallest patched release.
2. Update the targeted package, inspect **every** lockfile edge, and restore
   unrelated resolver churn. In particular, Yew and implicit-clone must share
   the compatible indexmap major version; a targeted Cargo update can reselect
   a broad transitive requirement and break that contract.
3. For SDK updates, capture a representative new protocol frame and verify its
   typed parsing, rendering and session lifecycle behavior. Fix missing/wrong
   types upstream instead of locally probing untyped JSON. If an urgent local
   workaround is necessary, link its upstream issue with `TODO(SDK #NNN)`.
4. Preserve `shared/`'s types-only/WASM-safe feature boundary. Run relevant
   session/renderer tests, native checks and the WASM build, plus the audit.
   Let the full CI matrix cover supported platforms before merging.
5. State what advisories were resolved, what warnings remain, and whether any
   exception's call-path rationale changed. Roll back a bad update through a
   reviewed revert of its manifest/lock changes; do not replace the lockfile
   with an unreviewed broad regeneration.
