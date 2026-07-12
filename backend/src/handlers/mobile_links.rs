//! Native mobile app-link association endpoints.

use crate::AppState;
use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub struct AssetLink {
    relation: [&'static str; 1],
    target: AssetLinkTarget,
}

#[derive(Serialize)]
pub struct AssetLinkTarget {
    namespace: &'static str,
    package_name: String,
    sha256_cert_fingerprints: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppleAppSiteAssociation {
    applinks: AppleAppLinks,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppleAppLinks {
    apps: [String; 0],
    details: [AppleAppLinkDetail; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppleAppLinkDetail {
    #[serde(rename = "appIDs")]
    app_ids: [String; 1],
    components: [AppleAppLinkComponent; 1],
}

#[derive(Serialize)]
pub struct AppleAppLinkComponent {
    #[serde(rename = "/")]
    path: &'static str,
    comment: &'static str,
}

/// Android App Links association document.
pub async fn assetlinks(State(app_state): State<Arc<AppState>>) -> Json<[AssetLink; 1]> {
    Json([AssetLink {
        relation: ["delegate_permission/common.handle_all_urls"],
        target: AssetLinkTarget {
            namespace: "android_app",
            package_name: app_state.mobile_app_links.bundle_id.clone(),
            sha256_cert_fingerprints: app_state
                .mobile_app_links
                .android_sha256_cert_fingerprints
                .clone(),
        },
    }])
}

/// Apple Universal Links association document. The route intentionally has no
/// file extension; iOS expects `/.well-known/apple-app-site-association`.
pub async fn apple_app_site_association(
    State(app_state): State<Arc<AppState>>,
) -> Json<AppleAppSiteAssociation> {
    let app_id = format!(
        "{}.{}",
        app_state.mobile_app_links.apple_team_id, app_state.mobile_app_links.bundle_id
    );

    Json(AppleAppSiteAssociation {
        applinks: AppleAppLinks {
            apps: [],
            details: [AppleAppLinkDetail {
                app_ids: [app_id],
                components: [AppleAppLinkComponent {
                    path: "*",
                    comment: "Open Agent Portal links in the mobile shell",
                }],
            }],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MobileAppLinksConfig;

    #[test]
    fn assetlinks_uses_configured_android_identity() {
        let config = MobileAppLinksConfig {
            bundle_id: "io.example.portal".to_string(),
            apple_team_id: "TEAMID".to_string(),
            android_sha256_cert_fingerprints: vec!["AA:BB".to_string()],
        };

        let link = AssetLink {
            relation: ["delegate_permission/common.handle_all_urls"],
            target: AssetLinkTarget {
                namespace: "android_app",
                package_name: config.bundle_id,
                sha256_cert_fingerprints: config.android_sha256_cert_fingerprints,
            },
        };
        let value = serde_json::to_value([link]).expect("serialize assetlinks");

        assert_eq!(
            value[0]["relation"][0],
            "delegate_permission/common.handle_all_urls"
        );
        assert_eq!(value[0]["target"]["namespace"], "android_app");
        assert_eq!(value[0]["target"]["package_name"], "io.example.portal");
        assert_eq!(value[0]["target"]["sha256_cert_fingerprints"][0], "AA:BB");
    }

    #[test]
    fn apple_association_uses_team_and_bundle_id() {
        let config = MobileAppLinksConfig {
            bundle_id: "io.example.portal".to_string(),
            apple_team_id: "ABCDE12345".to_string(),
            android_sha256_cert_fingerprints: vec![],
        };
        let app_id = format!("{}.{}", config.apple_team_id, config.bundle_id);
        let value = serde_json::to_value(AppleAppSiteAssociation {
            applinks: AppleAppLinks {
                apps: [],
                details: [AppleAppLinkDetail {
                    app_ids: [app_id],
                    components: [AppleAppLinkComponent {
                        path: "*",
                        comment: "Open Agent Portal links in the mobile shell",
                    }],
                }],
            },
        })
        .expect("serialize apple app-site association");

        assert_eq!(
            value["applinks"]["details"][0]["appIDs"][0],
            "ABCDE12345.io.example.portal"
        );
        assert_eq!(value["applinks"]["details"][0]["components"][0]["/"], "*");
    }
}
