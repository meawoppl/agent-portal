//! Thin binary entry point. All logic lives in the `backend` library crate
//! (`lib.rs`) so integration tests can construct an `AppState` and drive the
//! real router/server (see `backend/tests/`).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    backend::run().await
}
