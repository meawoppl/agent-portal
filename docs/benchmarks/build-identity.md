# Build identity without protocol-crate invalidation

Build identity is now emitted by `build-support/identity.rs` into the backend,
frontend, launcher and proxy crates. `shared` contains only the stable
`BuildInfo` type, constructor macro, and process registry; it has no build
script. Native entrypoints register their artifact's identity before library
work. Session reminders, proxy registration and Codex import identify that
process rather than a cached dependency. Unregistered library-only consumers
report `unknown`; they do not pretend to be a released binary.

Backend health/config/archive/privacy/service-monitor output and launcher
version/pastebin/registration read boundary constants directly. The frontend
footer retains `crate::VERSION`, now an alias of its own boundary identity.
Protocol version negotiation is unchanged.

## Measurement

Local Linux x86_64, rustc 1.96.0, dev `cargo check -p agent-portal`,
`CARGO_INCREMENTAL=0`, two Cargo jobs, warm dependency cache. Full command:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/path/to/target \
  CARGO_LOG=cargo::core::compiler::fingerprint=info \
  cargo check -p agent-portal --message-format=json --timings
```

After warming, an empty commit moves HEAD without changing any source. Cargo's
JSON `compiler-artifact.fresh` and fingerprint logs identify rebuilt crates.
The baseline is `d3fa4788` plus an empty commit (`74bb0908`); the changed tree's
empty commit is `80ff1777`. Timings include Cargo startup and build scripts.

| Scenario | Before | After | Non-fresh workspace crates |
| --- | ---: | ---: | --- |
| Source-identical HEAD change | 11.37 s | 3.19 s | 9 before, 1 after |
| Session reminder `trim()` to `trim_end()` edit | 6.19 s | 5.08 s | 6 before, 6 after |

Before, a metadata-only commit rechecked `shared`, `portal_sysmon`,
`portal_auth`, `session_lib`, `claude_session_lib`, `codex_session_lib`,
`muse_session_lib`, `antigravity_session_lib`, and `agent-portal`. After, only
`agent-portal` is non-fresh. The source-edit probe changes executable behavior,
then restores the file; the six affected crates are the session core, four
adapters, and launcher. Real dependency changes still propagate.

The initial after-run waited 21 seconds for another Cargo command's target lock;
that contaminated sample is excluded. These are local type-check timings, not
claims about release-link, total CI, cold-cache, or container-build speedups.
Native profile/cache measurements are separate work. The backend remains a
large crate and still rebuilds at its boundary; this removes unnecessary
library invalidation rather than eliminating required artifact relinking.

## Correctness and reproduction

`python3 scripts/test-build-identity.py` compiles the real helper and verifies
full-history commit counts/hashes, a detached worktree, shallow-checkout fallback
to the manifest patch, and source-tarball unknown revision. The timestamp keeps
the existing Pacific-time/date fallback semantics. `shared` unit coverage checks
explicit, idempotent registration and rejection of conflicting process identity.
A fixture compiles the registry library once, then builds two executables with
different identities against that same library and verifies each reports its own
version/revision. The real launcher build at `c2d995e1` printed
`agent-portal 2.15.1912 (c2d995e1; built 2026-11-12 18:52 PST)`, matching
that commit's count and hash.

Build-script rerun tracking covers helper edits, HEAD/reflog, the current branch
ref, packed refs and shallow state when present. No version is frozen, and
no environment override can claim another commit. Source-identical commit
changes still refresh the executable's version, revision and build timestamp.

To reproduce the source-edit measurement, warm the chosen checkout, replace
`body.trim()` with `body.trim_end()` in
`session-lib/src/proxy_session/portal_reminder.rs`, run the command above, then
restore that file. Never retain this measurement-only behavior change.
