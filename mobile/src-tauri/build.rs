fn main() {
    // Build-time configuration of the portal URL the WebView shell loads.
    //
    // Decision D3 (docs/MOBILE_APPS_PLAN.md §5.B.2): this is a REMOTE-URL shell
    // — it loads the deployed portal directly rather than bundling the
    // frontend. The default lives in `tauri.conf.json` (app.windows[0].url).
    // Setting `PORTAL_SHELL_URL` at build time overrides it: we bake the value
    // as a compile-time env var and `src/lib.rs` navigates the main window to
    // it on startup. When the var is unset, nothing is baked and the config
    // default is used verbatim (no double-load).
    if let Ok(url) = std::env::var("PORTAL_SHELL_URL") {
        if !url.trim().is_empty() {
            println!("cargo:rustc-env=PORTAL_SHELL_URL={url}");
        }
    }
    println!("cargo:rerun-if-env-changed=PORTAL_SHELL_URL");

    tauri_build::build();
}
