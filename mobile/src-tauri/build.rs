fn main() {
    println!("cargo:rerun-if-env-changed=PORTAL_SHELL_URL");
    println!("cargo:rerun-if-env-changed=PORTAL_IOS_APS_ENVIRONMENT");
    if let Ok(shell_url) = std::env::var("PORTAL_SHELL_URL") {
        println!("cargo:rustc-env=PORTAL_SHELL_URL={shell_url}");
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "ios" {
        configure_ios_entitlements();
    }
    if matches!(target_os.as_str(), "android" | "ios") {
        tauri_build::build();
    }
}

#[cfg(target_os = "macos")]
fn configure_ios_entitlements() {
    let aps_environment =
        std::env::var("PORTAL_IOS_APS_ENVIRONMENT").unwrap_or_else(|_| "development".to_string());
    let aps_environment = match aps_environment.as_str() {
        "development" | "production" => aps_environment,
        other => panic!(
            "PORTAL_IOS_APS_ENVIRONMENT must be \"development\" or \"production\", got {other:?}"
        ),
    };

    tauri_plugin::mobile::update_entitlements(|entitlements| {
        entitlements.insert("aps-environment".into(), aps_environment.into());
    })
    .expect("failed to update iOS APNs entitlement");
}

#[cfg(not(target_os = "macos"))]
fn configure_ios_entitlements() {}
