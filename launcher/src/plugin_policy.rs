//! Per-project plugin policy: always or never load a plugin's skills into
//! sessions under a directory. `Ask` is the absence of a stored policy, where
//! detection decides. Stored on the host, next to the installs it refers to.
use anyhow::{Context, Result};
use shared::api::PluginPolicy;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Project directory → plugin → policy.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Policies(BTreeMap<String, BTreeMap<String, PluginPolicy>>);

/// The policies that govern one directory.
pub(crate) type DirectoryPolicy = BTreeMap<String, PluginPolicy>;

static POLICY_WRITE: Mutex<()> = Mutex::new(());

fn policy_path() -> PathBuf {
    session_lib::paths::config_dir().join("plugin-policy.json")
}

/// Directories are keyed canonically so `~/repo`, `/home/u/repo/` and a
/// symlinked path share one policy.
fn project_key(directory: &Path) -> String {
    fs::canonicalize(directory)
        .unwrap_or_else(|_| directory.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

impl Policies {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid plugin policy file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("Could not read plugin policy"),
        }
    }

    /// Each plugin's policy for `directory`: the nearest ancestor with an
    /// entry for that plugin decides, so a policy set on a repository also
    /// covers its subdirectories and `.worktrees/` checkouts.
    pub(crate) fn for_directory(&self, directory: &Path) -> DirectoryPolicy {
        let key = PathBuf::from(project_key(directory));
        let mut resolved = DirectoryPolicy::new();
        for ancestor in key.ancestors() {
            if let Some(entries) = self.0.get(&*ancestor.to_string_lossy()) {
                for (plugin, policy) in entries {
                    resolved.entry(plugin.clone()).or_insert(*policy);
                }
            }
        }
        resolved
    }

    fn set(&mut self, directory: &Path, plugin: &str, policy: PluginPolicy) {
        let key = project_key(directory);
        let entries = self.0.entry(key.clone()).or_default();
        match policy {
            PluginPolicy::Ask => {
                entries.remove(plugin);
            }
            policy => {
                entries.insert(plugin.to_string(), policy);
            }
        }
        if entries.is_empty() {
            self.0.remove(&key);
        }
    }
}

pub(crate) fn for_directory(directory: &Path) -> Result<DirectoryPolicy> {
    Ok(Policies::read(&policy_path())?.for_directory(directory))
}

pub(crate) fn set(directory: &Path, plugin: &str, policy: PluginPolicy) -> Result<()> {
    set_at(&policy_path(), directory, plugin, policy)
}

fn set_at(path: &Path, directory: &Path, plugin: &str, policy: PluginPolicy) -> Result<()> {
    let _guard = POLICY_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("Plugin policy lock poisoned"))?;
    let mut policies = Policies::read(path)?;
    policies.set(directory, plugin, policy);
    crate::config::write_config_atomic(path, &serde_json::to_string_pretty(&policies)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_ancestor_decides_and_ask_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("policy.json");
        let repo = dir.path().join("repo");
        let worktree = repo.join(".worktrees/feature");
        fs::create_dir_all(&worktree).unwrap();

        set_at(&store, &repo, "kicadmium", PluginPolicy::Never).unwrap();
        set_at(&store, &repo, "visilog", PluginPolicy::Always).unwrap();
        set_at(&store, &worktree, "kicadmium", PluginPolicy::Always).unwrap();
        let policies = Policies::read(&store).unwrap();

        let at_repo = policies.for_directory(&repo);
        assert_eq!(at_repo["kicadmium"], PluginPolicy::Never);
        assert_eq!(at_repo["visilog"], PluginPolicy::Always);
        let in_worktree = policies.for_directory(&worktree);
        assert_eq!(in_worktree["kicadmium"], PluginPolicy::Always);
        assert_eq!(in_worktree["visilog"], PluginPolicy::Always);
        assert!(policies.for_directory(dir.path()).is_empty());

        set_at(&store, &repo, "kicadmium", PluginPolicy::Ask).unwrap();
        set_at(&store, &repo, "visilog", PluginPolicy::Ask).unwrap();
        let policies = Policies::read(&store).unwrap();
        assert!(policies.for_directory(&repo).is_empty());
        assert_eq!(policies.0.len(), 1, "an emptied project is dropped");
    }

    #[test]
    fn unreadable_policy_is_an_error_not_an_empty_policy() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("policy.json");
        fs::write(&store, "broken").unwrap();
        assert!(Policies::read(&store).is_err());
        assert!(set_at(&store, dir.path(), "x", PluginPolicy::Never).is_err());
        assert_eq!(fs::read_to_string(&store).unwrap(), "broken");
    }
}
