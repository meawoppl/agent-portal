# Native build profile and cache experiments

The production `release` profile remains size-focused (`opt-level = "z"`, fat
LTO, one codegen unit). These experiments do not change the frontend's optimized
Trunk/wasm-opt pipeline or adopt a native profile without measurements.

## Reproduce

Run the **Native Build Benchmark** workflow manually on its branch, or
apply the `ci-benchmark` label to its PR. The workflow is opt-in and is not a
required PR gate. It builds the actual optimized frontend distribution once,
then runs five independent Ubuntu 24.04 native jobs with Rust 1.96.0 and two
Cargo build jobs per runner:

| Variant | LTO | Optimization | Codegen units | Incremental |
|---|---|---|---|---|
| baseline-fat-z | fat | z | 1 | off |
| thin-z | thin | z | 16 | off |
| thin-3 | thin | 3 | 16 | off |
| thin-2 | thin | 2 | 16 | off |
| thin-3-incremental | thin | 3 | 16 | on |

Every job records the checked-out commit, exact toolchain, runner image/CPU,
profile environment, Cargo timings and fresh-artifact flags, elapsed build time,
binary hash/size and target-directory size. A local backend must report that
checkout's version and revision. The backend-only edit must change its config
response, and the frontend-dist-only edit must be served by the compiled binary.
A 1,000-request `/api/config` sample is a runtime smoke comparison, not a
representative production workload or a CPU microbenchmark.

Scenarios are initial cold/restored build, unchanged build, backend-handler edit,
backend restoration, frontend-distribution edit, and frontend restoration. The
in-place edits deliberately hold git identity fixed to isolate compiler effects;
those are **lower-bound warm-build costs**, not estimates of a normal new-commit
CI build while `shared/build.rs` still changes shared-crate metadata. After a
local archive roundtrip, the harness makes a real source-identical empty git commit and rebuilds, requiring the emitted version and hash to match that
new HEAD. This measures identity invalidation without changing architecture.

## Cache interpretation

The repository's configured shared-cache eviction limit is **10GB**. At the
start of this experiment it already held **11,388,678,884 bytes in 22 entries**.
Uploading large new experiments could evict production caches, so shared native
cache restore/save is disabled. No storage limit was increased. The first native
attempt was canceled before its planned saves; zero native experiment cache
entries were created.

Instead, every runner measures a **local zstd archive roundtrip**. Non-incremental
variants remove workspace packages with `cargo clean -p` before archiving; the
incremental variant retains the whole native target tree, including incremental
data. The isolated target directory is removed and restored, HEAD advances via
an empty commit, then the binary is rebuilt and its new identity is asserted.
Archive size, compression/restoration time, and the rebuilt artifact are recorded.
The incremental/full-workspace lane is a bundled strategy: it changes both
incremental compilation and retention relative to the thin-3 dependency-only
lane. It cannot isolate either variable independently. The dependency pruning
approximates retention, not every Swatinem eviction or registry-cache rule. Cargo registry downloads are included in initial builds.

The local roundtrip also preserves most source-file mtimes in one checkout. A
fresh hosted checkout can invalidate additional workspace fingerprints; these
results are closer to a persistent-workspace lower bound than a full hosted-cache
latency proof.

Actual GitHub cache upload/download costs are **unmeasured** without safe shared
cache headroom. Local archive timings omit network overhead and are
not proof that enabling retained target caches would improve production latency.
Adoption must account for the existing quota, eviction, network transfer and
storage cost. A future production cache must key toolchain, OS/architecture,
target, profile/flags and dependencies, with source-aware saves and compatible
restore prefixes; an immutable exact key would strand later builds on old data.

Do not share a mutable local `CARGO_TARGET_DIR` across concurrent worktrees to
mimic these experiments; that introduces a separate fingerprint/artifact-isolation
problem. The optional `frontend_run_id` workflow input reuses a previous run's
optimized frontend fixture and records its provenance, avoiding a repeated WASM
build during native comparisons.

Download the `native-benchmark-*` artifacts and run:

```sh
python3 scripts/benchmarks/summarize-native.py /path/to/downloaded-artifacts
```

The report shows `INCOMPLETE` rather than silently treating failed lanes as fast.
The underlying Cargo JSONL, HTML timings, server logs and metadata remain the
source of truth. Failed infrastructure runs cannot establish profile speedups.

## Observations

The first run, [43354780944](https://github.com/meawoppl/agent-portal/actions/runs/43354780944),
completed the optimized frontend in **129.24 seconds**, including **18.85 seconds
of wasm-opt**, with a **35,312,837-byte distribution** and **2,032,884 KiB peak
RSS** (Rust 1.96.0, Ubuntu image 20261101.360.1). These times exclude toolchain and
Trunk installation/cache setup. Native jobs failed before compilation because of
service-container command quoting; they supply no native performance evidence.

The quota-safe cancellation also preserved partial native cold-build results
from [43355805885](https://github.com/meawoppl/agent-portal/actions/runs/43355805885):

| Variant | Cold backend seconds | Binary MiB | Runner CPU |
|---|---:|---:|---|
| baseline-fat-z | 484.99 | 51.89 | EPYC 9V74 |
| thin-z | 553.18 | 60.16 | EPYC 7763 |
| thin-2 | 543.12 | 72.15 | EPYC 9V74 |
| thin-3-incremental | 587.69 | 71.45 | EPYC 9V74 |

The thin-3 cold sample and all warm-edit/cache scenarios were incomplete. These
single-run partial results do not establish a winner, but they do refute assuming
that Thin LTO/native opt-level 3 must improve cold builds. They are not a basis
for adopting a profile or claiming a cache speedup.

## Completed experiment and decision

All five lanes of [run 43357864725](https://github.com/meawoppl/agent-portal/actions/runs/43357864725)
completed all seven scenarios and their runtime identity/content assertions.
The native source was explicitly pinned to
`e94cc589811d5f35a79346b45a347a1d782663a6`, before #2456/#2459. The optimized frontend
fixture was reused from run 43355805885; its source was the earlier synthetic PR
merge `2b975a53d28a59007eb32dfd80c27e42177bd9db`. That merge also included #2453's
connection-status UI changes. This fixture was identical across all five lanes.
[Reduced measurements](native-build-results.json) preserve every scenario,
identity, size, timing and artifact freshness count; full Cargo timing HTML and
build logs remain in each run's `native-benchmark-*` artifacts.

| Variant | Cold s | Unchanged s | Backend edit s | Dist edit s | Binary MiB |
|---|---:|---:|---:|---:|---:|
| baseline-fat-z | 583.35 | 0.44 | 247.32 | 331.70 | 51.89 |
| thin-z | 368.91 | 0.28 | 134.79 | 182.77 | 60.16 |
| thin-2 | 557.13 | 0.38 | 240.61 | 310.27 | 72.15 |
| thin-3 | 602.18 | 0.34 | 267.78 | 332.02 | 71.92 |
| thin-3-incremental | 561.40 | 0.35 | 223.41 | 235.67 | 71.45 |

These single samples ran on **different CPUs and runner images**, not controlled
identical hardware:

| Variant | CPU | Ubuntu image |
|---|---|---|
| baseline-fat-z | AMD EPYC 7763 | 20261101.360.1 |
| thin-z | AMD EPYC 9V45 | 20261111.370.1 |
| thin-2 | AMD EPYC 9V74 | 20261101.360.1 |
| thin-3 | Intel Xeon Platinum 8573C | 20261101.360.1 |
| thin-3-incremental | Intel Xeon 6975P-C | 20261111.127.1 |

For example, thin-z took 553.18 seconds cold on the previous run's EPYC 7763 and
368.91 on this run's EPYC 9V45. A ~33% cross-run difference for the same profile
is large enough to defeat a causal claim from the final ranking. The partial
run's baseline and thin-2 were both EPYC 9V74, and thin-2 was slower there.
The `/api/config` smoke samples took 0.683/0.402/0.485/0.487/0.383 seconds for
baseline/thin-z/thin-2/thin-3/incremental respectively; those different-hardware,
client-inclusive samples verify operation, not production throughput improvement.

**#2448 decision: retain the existing production release profile.** Every
candidate increased binary size (thin-z by ~16%, the others by ~38–39%), and
these measurements do not establish a reproducible build or runtime benefit
that justifies that tradeoff. No separate native profile is adopted, and WASM
optimization remains unchanged. This completes the benchmark investigation on
the tested revision without promising a speedup. Future adoption requires a
current-main repeated comparison with matched hardware and representative
runtime tests, especially after the metadata/media changes.

### Local cache costs

| Variant | Retained MiB | Archive MiB | Pack s | Unpack s | New HEAD build s |
|---|---:|---:|---:|---:|---:|
| baseline-fat-z | 1386.22 | 407.37 | 6.03 | 2.27 | 331.08 |
| thin-z | 1427.25 | 419.07 | 3.21 | 1.51 | 187.77 |
| thin-2 | 1617.15 | 480.10 | 5.24 | 2.12 | 309.34 |
| thin-3 | 1611.89 | 479.86 | 5.36 | 1.96 | 324.30 |
| thin-3-incremental | 4031.37 | 1327.53 | 11.32 | 11.88 | 233.92 |

Every restored binary correctly reported its new commit identity. The retained
incremental strategy produces a 1.30 GiB compressed archive versus 0.47 GiB for
thin-3 dependencies, and consumes 23.20 seconds of local pack/unpack versus 7.32.
Its lower build time is confounded by CPU/image differences and the bundled
incremental/retention changes. This does not prove a net hosted-cache win.

**#2449 remains open.** Its acceptance criterion is a representative net
end-to-end improvement. Network transfer, fresh-checkout timestamp invalidation,
and eviction costs were not measured, and existing cache quota has no safe
headroom. Production cache policy is unchanged.

## Where the time went

Cargo's partial-run timing data separates meaningful units, but does not report
pure linker/LTO time for the backend executable. Its binary unit must not be
presented as a linker measurement:

| Cold unit | baseline-fat-z seconds | thin-2 seconds |
|---|---:|---:|
| Time before backend build-script starts | 214.65 | 236.90 |
| Backend build-script run | 68.40 | 69.15 |
| Backend library | 83.47 | 77.48 |
| Backend binary unit | 118.26 | 159.40 |

The baseline library splits into 31.94 seconds compiler frontend and 51.53
seconds codegen; thin-2 splits into 33.83 and 43.65 seconds. Earlier dependency
work includes aws-lc-sys, Diesel and the S3 SDK. Dependency units overlap, so
individual durations cannot simply be summed into an end-to-end time. These
measurements reject attributing the entire cold cost to LTO.

In the complete thin-z lane, a backend edit spends 47.31 seconds in its library
and 87.18 in the binary unit. A frontend-distribution edit additionally runs the
backend build-script for 47.34 seconds, then spends 47.28/87.85 seconds in the
library/binary. Asset compression/embedding is material on this tested revision.
The measurements predate #2456's metadata boundary and #2459's media embedding
changes, so a future adoption decision must rebaseline current main.

## Next cache experiment

Issue #2449 remains open. Before adopting retained native targets, establish
explicit shared-cache headroom without evicting existing production entries or
silently increasing billed limits. Then test current main after #2456/#2459 on
fresh hosted checkouts: source-identical new commits, backend changes and
frontend-only changes, including fresh source-file timestamps. Key toolchain,
OS/architecture, target/profile/flags and dependencies; use source-specific saves
with compatible restore prefixes. Measure actual network restore/upload,
compression and eviction alongside build time, verify each binary's own fresh
identity and contents, and compare incremental/full-workspace retention against
matching non-incremental/dependency-only and non-incremental/workspace controls.
Choose a policy only from the net end-to-end result.
