fn main() {
    println!("cargo:rerun-if-env-changed=PORTAL_SHELL_URL");
    if let Ok(shell_url) = std::env::var("PORTAL_SHELL_URL") {
        println!("cargo:rustc-env=PORTAL_SHELL_URL={shell_url}");
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "android" | "ios") {
        tauri_build::build();
    }
}
