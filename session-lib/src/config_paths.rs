//! Native per-user paths shared by the launcher and proxy.
//!
//! Keep the project identifier here: `ProjectDirs` includes it in paths on
//! macOS and Windows, so duplicating it lets components silently disagree.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const QUALIFIER: &str = "org";
const ORGANIZATION: &str = "meawoppl";
const APPLICATION: &str = "agent-portal";

pub fn config_dir() -> Result<PathBuf> {
    let current = directories::ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .context("failed to determine agent-portal config directory")?
        .config_dir()
        .to_path_buf();
    let legacy = directories::ProjectDirs::from("com", "anthropic", APPLICATION)
        .context("failed to determine legacy agent-portal config directory")?
        .config_dir()
        .to_path_buf();
    if let Err(error) = migrate_legacy_dir(&legacy, &current) {
        // Staying on the readable legacy directory is safer than silently
        // appearing logged out or writing a second set of state elsewhere.
        tracing::warn!(
            "{}; continuing to use legacy config directory {}",
            error,
            legacy.display()
        );
        return Ok(legacy);
    }
    Ok(current)
}

/// Move an untouched legacy install as a unit, preserving credentials, IDs,
/// buffered output, and sidecars. If the new location already exists, leave
/// both alone: merging live state would risk overwriting newer credentials.
fn migrate_legacy_dir(legacy: &Path, current: &Path) -> Result<()> {
    if legacy == current || current.exists() || !legacy.exists() {
        return Ok(());
    }
    if let Some(parent) = current.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::rename(legacy, current).with_context(|| {
        format!(
            "failed to migrate agent-portal config from {} to {}",
            legacy.display(),
            current.display()
        )
    })?;
    tracing::info!(
        "Migrated agent-portal config from {} to {}",
        legacy.display(),
        current.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_complete_legacy_directory() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("old");
        let current = root.path().join("new");
        std::fs::create_dir_all(legacy.join("buffers")).unwrap();
        std::fs::write(legacy.join("launcher.json"), "credentials").unwrap();
        std::fs::write(legacy.join("buffers/1.json"), "pending").unwrap();

        migrate_legacy_dir(&legacy, &current).unwrap();

        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(current.join("launcher.json")).unwrap(),
            "credentials"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("buffers/1.json")).unwrap(),
            "pending"
        );
    }

    #[test]
    fn existing_new_directory_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("old");
        let current = root.path().join("new");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(legacy.join("launcher.json"), "old").unwrap();
        std::fs::write(current.join("launcher.json"), "new").unwrap();

        migrate_legacy_dir(&legacy, &current).unwrap();

        assert!(legacy.exists());
        assert_eq!(
            std::fs::read_to_string(current.join("launcher.json")).unwrap(),
            "new"
        );
    }
}
