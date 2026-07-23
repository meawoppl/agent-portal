//! Embed the `viewer-frontend` WASM bundle into the `portal-archive` binary,
//! exactly as `backend/build.rs` embeds `frontend/dist`: `memory_serve` reads
//! the directory here and `serve.rs` mounts it with `memory_serve::load!()`.
//!
//! WHY THE MISSING-DIST TOLERANCE (the CI/dist decision, #1288 PR 3b):
//! `memory_serve::load_directory` canonicalizes the path and *panics* if it is
//! absent. The backend can afford that because its frontend is mandatory and CI
//! stubs an empty `frontend/dist` in every workspace-compiling job. But
//! `portal-archive` is first and foremost a CLI (`list`/`rollup`/`export`/`cat`)
//! that is fully useful without the UI, and it is compiled by `cargo build
//! --workspace`, `cargo clippy --workspace`, and `cargo test --workspace` — none
//! of which should require a WASM toolchain or a prior `trunk build`. So instead
//! of adding a `mkdir -p viewer-frontend/dist` stub step to three CI jobs (and
//! leaving a footgun for anyone running a fresh `cargo build`), we make the
//! embed tolerant: if `viewer-frontend/dist` is missing we create it empty and
//! embed a zero-asset bundle. `serve` then reports the UI is unbuilt until a real
//! `trunk build` populates the directory; the JSON API and every CLI subcommand
//! work regardless. The gitignored `dist/` dir is the only thing written, and
//! only when absent, so a real `trunk build` always wins.

use std::path::Path;

fn main() {
    let dist = "../viewer-frontend/dist";
    if !Path::new(dist).exists() {
        // Best-effort: on a read-only checkout this fails and `load_directory`
        // falls back to its canonicalize panic — the same hard error the
        // backend gives, which is the correct signal on such a build.
        let _ = std::fs::create_dir_all(dist);
        println!(
            "cargo:warning=viewer-frontend/dist not found; embedding an empty UI. \
             Run `trunk build` in viewer-frontend/ before `cargo build -p archive-viewer` \
             to bundle the `portal-archive serve` viewer."
        );
    }
    memory_serve::load_directory(dist);
}
