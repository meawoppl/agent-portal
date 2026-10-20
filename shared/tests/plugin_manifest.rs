//! Compatibility coverage for both native consumers' TOML parser versions.
use shared::plugin_manifest::{InventoryManifest, RuntimeManifest, SkillsManifest};

const COMPLETE: &str = include_str!("fixtures/plugin_manifest/complete.toml");
const SKILLS_ONLY: &str = include_str!("fixtures/plugin_manifest/skills_only.toml");
const INVENTORY_ONLY: &str = include_str!("fixtures/plugin_manifest/inventory_only.toml");

macro_rules! compatibility_tests {
    ($module:ident, $toml:ident) => {
        mod $module {
            use super::*;
            type Runtime = RuntimeManifest<$toml::Value>;

            #[test]
            fn complete_manifest_retains_consumer_fields() {
                let runtime: Runtime = $toml::from_str(COMPLETE).unwrap();
                let inventory: InventoryManifest = $toml::from_str(COMPLETE).unwrap();
                let skills: SkillsManifest = $toml::from_str(COMPLETE).unwrap();
                assert_eq!(runtime.name, inventory.name);
                assert_eq!(runtime.skills[0].name, skills.skills[0].name);
                assert_eq!(runtime.skills[0].agents, inventory.skills[0].agents);
                assert_eq!(
                    runtime.skills[0].path.to_str(),
                    Some(inventory.skills[0].path.as_str())
                );
                assert_eq!(runtime.install.setup.as_deref(), Some("echo setup"));
                assert_eq!(runtime.install.doctor.as_deref(), Some("echo doctor"));
                assert_eq!(
                    runtime.surface.unwrap().health_path.as_deref(),
                    Some("/health")
                );
                assert_eq!(inventory.surface.unwrap().kind.as_deref(), Some("web"));
                assert_eq!(runtime.commands[0].run, "echo check");
                assert_eq!(runtime.prompts[0].path.to_str(), Some("prompts/context.md"));
                assert_eq!(runtime.toolchains[0].env["FIXTURE"], "value");
                assert_eq!(runtime.capabilities["flag"].as_bool(), Some(true));
                assert_eq!(
                    runtime.capabilities["nested"]["value"].as_str(),
                    Some("kept")
                );
                assert_eq!(inventory.detect[0].any, ["Cargo.toml"]);
            }

            #[test]
            fn minimal_manifests_keep_defaults() {
                let runtime: Runtime = $toml::from_str("name = 'minimal'").unwrap();
                assert!(runtime.skills.is_empty() && runtime.commands.is_empty());
                assert!(runtime.prompts.is_empty() && runtime.toolchains.is_empty());
                assert!(runtime.capabilities.is_empty() && runtime.surface.is_none());
                assert!(runtime.install.setup.is_none() && runtime.install.doctor.is_none());
                let inventory: InventoryManifest = $toml::from_str("name = 'minimal'").unwrap();
                assert!(inventory.detect.is_empty() && inventory.skills.is_empty());
                assert!(inventory.commands.is_empty() && inventory.surface.is_none());
                let skills: SkillsManifest = $toml::from_str("").unwrap();
                assert!(skills.skills.is_empty());
                assert!($toml::from_str::<Runtime>("").is_err());
                assert!($toml::from_str::<InventoryManifest>("").is_err());
            }

            #[test]
            fn skills_view_ignores_unrelated_invalid_fields() {
                let skills: SkillsManifest = $toml::from_str(SKILLS_ONLY).unwrap();
                assert_eq!(skills.skills[0].name, "review");
                assert!(skills.skills[0].agents.is_empty());
                assert!($toml::from_str::<Runtime>(SKILLS_ONLY).is_err());
                assert!($toml::from_str::<InventoryManifest>(SKILLS_ONLY).is_err());
            }

            #[test]
            fn inventory_does_not_validate_runtime_fields() {
                let inventory: InventoryManifest = $toml::from_str(INVENTORY_ONLY).unwrap();
                assert_eq!(inventory.commands.len(), 2);
                assert!(inventory.commands[0].description.is_none());
                assert_eq!(inventory.surface.unwrap().default_width_percent, Some(300));
                assert!(inventory.detect[0].any.is_empty());
                assert!($toml::from_str::<Runtime>(INVENTORY_ONLY).is_err());
                assert!($toml::from_str::<Runtime>(
                    "name='x'\n[surface]\ndefault_width_percent=300"
                )
                .is_err());
                assert!(
                    $toml::from_str::<Runtime>("name='x'\n[[commands]]\nname='missing-run'")
                        .is_err()
                );
            }

            #[test]
            fn all_views_reject_invalid_skill_fields() {
                for raw in [
                    "name='x'\n[[skills]]\nname='missing-path'",
                    "name='x'\n[[skills]]\nname='x'\npath=42",
                    "name='x'\n[[skills]]\nname='x'\npath='x'\nagents=false",
                ] {
                    assert!($toml::from_str::<Runtime>(raw).is_err());
                    assert!($toml::from_str::<InventoryManifest>(raw).is_err());
                    assert!($toml::from_str::<SkillsManifest>(raw).is_err());
                }
            }
        }
    };
}

compatibility_tests!(backend, toml_backend);
compatibility_tests!(launcher, toml_launcher);
