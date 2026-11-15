//! Thin binary entry point. All logic lives in the `backend` library crate
//! (`lib.rs`) so integration tests can construct an `AppState` and drive the
//! real router/server (see `backend/tests/`).

// jemalloc with sampled heap profiling: one allocation stack per ~512 KiB
// allocated, a few percent overhead. `backend::heap_profile` dumps these
// samples when the heap spikes. Declared in the binary only, so tests and
// other library consumers keep the system allocator.
#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(target_os = "linux")]
#[allow(non_upper_case_globals)]
#[export_name = "malloc_conf"]
pub static malloc_conf: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    backend::run().await
}
