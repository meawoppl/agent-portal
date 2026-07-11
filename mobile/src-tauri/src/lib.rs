#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::mobile_entry_point]
pub fn run() {
    if let Err(err) = tauri::Builder::default().run(tauri::generate_context!()) {
        eprintln!("failed to run Agent Portal mobile shell: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn run() {}
