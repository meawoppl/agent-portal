fn main() {
    println!("cargo:rerun-if-env-changed=PORTAL_SHELL_URL");
    println!("cargo:rerun-if-env-changed=PORTAL_IOS_APS_ENVIRONMENT");
    println!("cargo:rerun-if-env-changed=TAURI_DEEP_LINK_PLUGIN_CONFIG");
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
    let associated_domains = ios_associated_domains();

    // The deep-link plugin normally writes associated-domains from its own
    // build script, but Cargo can cache dependency build scripts across a
    // regenerated Xcode project. Own both mobile entitlement keys here so every
    // iOS app build deterministically refreshes the generated plist.
    tauri_plugin::mobile::update_entitlements(|entitlements| {
        entitlements.insert("aps-environment".into(), aps_environment.into());
        if associated_domains.is_empty() {
            entitlements.remove("com.apple.developer.associated-domains");
        } else {
            entitlements.insert(
                "com.apple.developer.associated-domains".into(),
                associated_domains.into(),
            );
        }
    })
    .expect("failed to update iOS entitlements");
}

#[cfg(target_os = "macos")]
fn ios_associated_domains() -> Vec<plist::Value> {
    tauri_plugin::plugin_config::<DeepLinkConfig>("deep-link")
        .map(|config| {
            config
                .mobile
                .into_iter()
                .filter(|domain| domain.is_app_link())
                .filter_map(|domain| {
                    domain
                        .host
                        .map(|host| plist::Value::from(format!("applinks:{host}")))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct DeepLinkConfig {
    #[serde(default)]
    mobile: Vec<AssociatedDomain>,
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct AssociatedDomain {
    #[serde(default = "default_schemes")]
    scheme: Vec<String>,
    host: Option<String>,
    #[serde(default, rename = "appLink", alias = "app-link")]
    app_link: Option<bool>,
}

#[cfg(target_os = "macos")]
impl AssociatedDomain {
    fn is_app_link(&self) -> bool {
        self.app_link
            .unwrap_or_else(|| self.is_web_link() && self.host.is_some())
    }

    fn is_web_link(&self) -> bool {
        self.scheme
            .iter()
            .any(|scheme| scheme == "https" || scheme == "http")
    }
}

#[cfg(target_os = "macos")]
fn default_schemes() -> Vec<String> {
    vec!["https".to_string(), "http".to_string()]
}

#[cfg(not(target_os = "macos"))]
fn configure_ios_entitlements() {}
