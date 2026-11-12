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

## Current warning dispositions

Reviewed 2026-11-11 against `f007f10116fe0449db552ddebbc05c1883391bcf`,
using cargo-audit 0.22.2 and advisory database commit
`fd6f39814c4d3c359fdbe223d0280dec4933f78f`. The full-lock scan reports zero
**unignored vulnerabilities**, 12 unmaintained-package warnings, five unsoundness
warnings and five yanked package versions. The RSA exception below remains in
force; a successful audit does not mean an empty advisory set.

Owner for all dispositions: the repository maintainer (@meawoppl). Recheck by
**2026-11-18**, or sooner when a parent dependency or affected feature changes.
These are visible follow-ups under #1209 R7, not new ignore entries or a claim
that known-unsound dependencies are safe. The machine-readable snapshot is
[`dependency-audit-2026-11-11.json`](benchmarks/dependency-audit-2026-11-11.json).

### Unsoundness warnings

| Locked package and advisory | Reachability and concrete disposition |
|---|---|
| Diesel 2.3.9 — [RUSTSEC-2026-0172](https://rustsec.org/advisories/RUSTSEC-2026-0172.html) | Backend uses PostgreSQL, not the affected SQLite byte-buffer deserialization API. No direct call in Portal. Upgrade to >=2.3.10 in the next database dependency update; validate migration/schema checks. PostgreSQL-only use limits this particular exposure, not all Diesel risk. |
| glib 0.18.5 — [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) | Linux desktop shell reaches GTK/glib through Tauri/wry/tao/muda; this is not the server/WASM path. No direct Portal `VariantStrIter` use was found, but transitive use is not ruled out. A fix requires the GTK/Tauri parent chain to adopt glib >=0.20; review that chain before distributing Linux desktop builds. |
| lru 0.16.4 — [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253.html) | Backend S3 SDK's S3 Express identity cache uses this crate. The advisory requires a panicking key destructor during `pop` and subsequent use; lock reachability alone does not establish that trigger. Update the S3 SDK to a version using lru >=0.18.2, then test archive operations. No local semver-incompatible override. |
| rustix 1.1.4 — [RUSTSEC-2026-0486](https://rustsec.org/advisories/RUSTSEC-2026-0486.html) | Native launcher dependencies (`listeners`, temporary files) and desktop dependencies reach rustix. No direct Portal xattr calls found; transitive xattr use has not been excluded. Targeted update to >=1.1.6 with native file/transfer tests is the disposition. |
| Tokio 1.52.2 — [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html) | Native server and agents use Tokio throughout. No direct Portal `AsyncFd` constructors found; that is not proof about dependencies. Upgrade to >=1.53.3 and validate Unix I/O plus reconnect tests; audit constructor deprecations rather than suppressing warnings. |

### Maintenance warnings

| Packages and advisory IDs | Parent chain and disposition |
|---|---|
| anymap2 0.13.0 (2026-0319), bincode 1.3.3 (2025-0141) | Yew/prokio/gloo-worker, including frontend WASM dependencies. Track a compatible Yew/gloo upgrade removing these; replacing bincode locally could break worker serialization. Keep visible until the parent chain changes. |
| pin-utils 0.1.0 (2026-0441) | Async ecosystem, including AWS Smithy; several native paths. Remove direct usage in favor of core::pin::pin if introduced; consume parent releases that replace this transitive crate. |
| proc-macro-error 1.0.4 (2024-0370) | Compile-time macros, including glib-macros/GTK. Resolve through parent macro crates; maintenance status alone is not a runtime vulnerability. |
| unic-char-property, unic-char-range, unic-common, unic-ucd-ident, unic-ucd-version 0.9.0 (2025-0081, 2025-0075, 2025-0080, 2025-0100, 2025-0098) | Tauri utilities -> urlpattern, including build-time/mobile-shell dependencies. Track a parent upgrade using maintained Unicode implementations; do not replace tables without URL-pattern compatibility tests. |
| zbus_names 4.3.3, zvariant 5.13.0, zvariant_derive 5.13.0 (2026-0371, 2026-0370, 2026-0372) | Linux desktop opener -> zbus and its macros. Upgrade the opener/zbus chain together when a maintained compatible path is available; no change to server or browser auth is needed. |

Advisory identifiers in the maintenance table use the `RUSTSEC-` prefix; their
full source descriptions and URLs are in the snapshot. None is suppressed.

### Yanked releases

The lockfile contains `async-executor 1.14.0` (desktop zbus), `chacha20 0.10.1`
(rand via web-push/object_store), `crypto-bigint 0.7.8` (web-push's signing
chain), and `spin 0.9.8` / `0.10.0` (lazy_static/RSA and other native parents).
Yanking alone does not establish why a release was withdrawn or its
exploitability. Resolve each to a non-yanked compatible release with a targeted
lockfile update and upstream release-note review; if a parent constrains it,
record that exact constraint in the dependency-update PR. Do not run a blanket
workspace update to clear this list.

Reproduce dependency paths with `cargo tree --locked --target all --invert
PACKAGE@VERSION`. This is lock/feature reachability, not a call-graph proof.
The per-target distinction matters: desktop warnings are retained by the
full-lock gate even when the server and WASM artifacts do not contain them.

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

## Web Push arithmetic patch (2026-10-25)

RUSTSEC-2026-0387 affects crypto-bigint before 0.7.6, including the 0.5.5
selected through web-push 0.11 -> jwt-simple 0.12 -> p256 0.13. VAPID signing
uses this path, so an exposure exception is inappropriate.

The local crates.io patch at `vendor/web-push` retains the published 0.11 API,
message encryption and Apache-2.0 license, but requires jwt-simple 0.15, whose
curve dependencies resolve to crypto-bigint 0.7.8. Its provenance and complete
local changes are recorded in `vendor/web-push/PORTAL-PATCH.md`. Remove the
vendor patch once a published web-push release supports patched arithmetic.
The backend verifies a generated VAPID ES256 JWT independently with
jsonwebtoken/ring, including its audience and expiry. CI continues to audit
the entire lockfile; no advisory is suppressed by this fix.

The RSA exposure exception remains applicable: jwt-simple's RSA path is still
unused; VAPID selects ES256 only and FCM uses jsonwebtoken/ring. Reviewed
2026-10-25. Informational audit warnings remain visible.
