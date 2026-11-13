# Ready-platform CLI releases

Issue #2452 removes the all-platform barrier from launcher/proxy updates. Every
platform still builds and signs the same binaries; PR platform checks are unchanged.

## Measured baseline

GitHub Release run [43348496928](https://github.com/meawoppl/agent-portal/actions/runs/43348496928)
(source `67b26d554656d1205752619e204ec423e6f84982`, GitHub-hosted runners):

| Event | UTC, 2026-11-13 |
|---|---|
| Linux x86_64 build completed | 01:59:02 |
| Windows x86_64 build completed | 02:03:20 |
| Create Release started | 02:03:21 |
| Create Release completed | 02:03:38 |

A ready Linux pair waited **276 seconds** for the downloadable complete release.
This is observed pipeline delay, not compilation time or a predicted speedup.

## Publication and update contract

1. A small preparation job creates a draft `cli-<full source SHA>` release.
2. Each build lane uploads its signed platform binaries directly to that release,
   before the secondary Actions artifact upload. No other lane is awaited.
3. It uploads `ready-<platform>.json` last. This records the source SHA, semantic
   version and SHA-256/size of both launcher and proxy. GitHub's stored asset
   digests must match before publication continues.
4. The release is published without changing GitHub's `latest` designation.
   Its existing assets are never overwritten. Retry reuses a completed manifest;
   interrupted uploads reuse existing assets from the same source commit.
5. A non-force Git push advances `release-ready/<platform>` to that source commit.
   This is the readiness transaction: readers resolve this ref first, then use
   immutable source-tag URLs. Git's fast-forward check rejects older or divergent
   sources, including old workflow reruns and racing publication.
6. The updater resolves exactly one platform ref. No release-list pagination or
   publication-order assumptions are involved. It validates the manifest, both
   asset digests and the actual downloaded bytes before replacing anything.
   A locally newer semantic version stays installed.

A delayed/failed Windows build leaves the Windows ref pointing at its prior
ready commit. It does not delay a ready Linux update or advertise an old Windows
binary under the Linux commit's version. Network errors, absent/malformed ready
manifests and mismatched checksums fail clearly without replacing the executable.
Only a missing readiness ref permits the migration fallback to `latest`.

## Existing clients and installers

Before merging this workflow change, let older Release runs finish: those older
workflow definitions do not participate in the new publisher serialization or
ancestry guard and could otherwise overwrite the compatibility release.

The existing install script continues to bootstrap from the **complete** `latest`
release; it never attempts a partially populated release. Its first normal
launcher startup checks the platform-ready channel once the bootstrap binary
contains this updater. The first rollout of this feature therefore still needs
one complete release to teach existing clients the new discovery protocol.

Legacy `latest` publication remains after the full matrix. Its publisher is
serialized and guarded by source ancestry, not workflow run numbers. An older
manual rerun cannot replace a newer complete release. This compatibility path
still has the old delete/create gap; clients in that brief gap fail and retry,
and new platform-channel clients do not use it.

## Validation and remaining measurement

- Python publication simulation: a Linux manifest/ref becomes ready without any
  Windows assets, retries retain the first pair, corrupt/incomplete pairs fail.
- Real local Git remote: newer source publishes first; older completion is
  rejected; same-source retries are idempotent.
- Rust updater tests: Linux selection needs no Windows asset; incomplete or
  mixed-source pairs and checksum mismatches are rejected.
- Every real publish records `READY_PLATFORM_SECONDS` and a GitHub job summary.
  Compare the first production run's signed-binary-ready/publication timestamps
  against the 276-second baseline after merge. **No live speedup is claimed yet.**

The platform refs point at ordinary main-source commits and should not be
force-pushed or manually edited. A host policy that makes GitHub release assets
immutable immediately upon publication must be disabled for these incrementally
populated releases, or replaced with separate per-platform releases.

GitHub API contract: [release create/update and assets](https://docs.github.com/en/rest/releases/releases),
[Git reference fast-forward updates](https://docs.github.com/en/rest/git/refs#update-a-reference).
