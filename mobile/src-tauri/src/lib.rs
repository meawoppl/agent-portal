//! Agent Portal mobile shell.
//!
//! A thin Tauri 2 WebView shell around the deployed Agent Portal web app
//! (decision D3, docs/MOBILE_APPS_PLAN.md §5.B.2: remote-URL, not bundled).
//! This is the E1 scaffold — no push bridges, deep links, or auth handoff yet
//! (those are E2–E6). The window's default URL lives in `tauri.conf.json`; a
//! build-time `PORTAL_SHELL_URL` override is baked in by `build.rs`.

/// Entry point shared by the desktop binary (`main.rs`) and the mobile
/// runtimes (Android/iOS load the library and call the generated entry point).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // If the build was configured with PORTAL_SHELL_URL, point the
            // shell at that URL. Otherwise the window keeps the default from
            // tauri.conf.json.
            if let Some(url) = option_env!("PORTAL_SHELL_URL") {
                if let Some(window) = tauri::Manager::get_webview_window(app, "main") {
                    match url.parse() {
                        Ok(parsed) => {
                            if let Err(e) = window.navigate(parsed) {
                                eprintln!("failed to navigate shell to {url}: {e}");
                            }
                        }
                        Err(e) => eprintln!("invalid PORTAL_SHELL_URL {url:?}: {e}"),
                    }
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Agent Portal mobile shell");
}
