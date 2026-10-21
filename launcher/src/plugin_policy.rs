//! Per-project plugin policy: always or never load a plugin's skills into
//! sessions under a directory, or leave it to detection (`Ask`). Stored on the
//! host, next to the installs it refers to.
use anyhow::{Context, Result};
use shared::api::PluginPolicy;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Canonical project directory → plugin → policy.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Policies(BTreeMap<String, BTreeMap<String, PluginPolicy>>);

/// The policies that govern one directory.
pub(crate) type DirectoryPolicy = BTreeMap<String, PluginPolicy>;

static POLICY_WRITE: Mutex<()> = Mutex::new(());

fn policy_path() -> PathBuf {
    session_lib::paths::config_dir().join("plugin-policy.json")
}

/// The directory a session reads policy for: `~` expanded, symlinks resolved.
/// One that doesn't resolve has no policy.
fn read_key(directory: &Path) -> Option<PathBuf> {
    let expanded = crate::path_policy::expand_home_path(&directory.to_string_lossy()).ok()?;
    fs::canonicalize(expanded).ok()
}

/// The directory a policy is stored under: an absolute (or `~/`) path to an
/// existing directory in the launcher user's home, resolved canonically so
/// `~/repo`, `/home/u/repo/` and a symlink to it share one entry.
fn write_key(directory: &str) -> Result<PathBuf> {
    let expanded = crate::path_policy::expand_home_path(directory)?;
    anyhow::ensure!(
        expanded.is_absolute(),
        "Give the project as an absolute path or one starting with ~/: {directory}"
    );
    crate::path_policy::ensure_existing_dir_under_home(directory)
}

impl Policies {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid plugin policy file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("Could not read plugin policy"),
        }
    }

    /// Each plugin's policy for the canonical directory `key`: the nearest
    /// ancestor with an entry for that plugin decides, so a policy set on a
    /// repository also covers its subdirectories and `.worktrees/` checkouts,
    /// and a subdirectory's own entry (`Ask` included) overrides it.
    fn resolve(&self, key: &Path) -> DirectoryPolicy {
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

    /// Store `policy` at `key`. `Ask` is stored too when an ancestor says
    /// otherwise, so "load when detected" in a subdirectory overrides a
    /// repository-wide always or never; with nothing to override, the entry
    /// is dropped to keep the file minimal.
    fn set(&mut self, key: &Path, plugin: &str, policy: PluginPolicy) {
        let inherited = key
            .parent()
            .map(|parent| self.resolve(parent))
            .and_then(|parents| parents.get(plugin).copied())
            .unwrap_or_default();
        let name = key.to_string_lossy().into_owned();
        let entries = self.0.entry(name.clone()).or_default();
        if policy == PluginPolicy::Ask && inherited == PluginPolicy::Ask {
            entries.remove(plugin);
        } else {
            entries.insert(plugin.to_string(), policy);
        }
        if entries.is_empty() {
            self.0.remove(&name);
        }
    }
}

pub(crate) fn for_directory(directory: &Path) -> Result<DirectoryPolicy> {
    let policies = Policies::read(&policy_path())?;
    Ok(read_key(directory)
        .map(|key| policies.resolve(&key))
        .unwrap_or_default())
}

pub(crate) fn set(directory: &str, plugin: &str, policy: PluginPolicy) -> Result<()> {
    set_at(&policy_path(), &write_key(directory)?, plugin, policy)
}

fn set_at(path: &Path, key: &Path, plugin: &str, policy: PluginPolicy) -> Result<()> {
    let _guard = POLICY_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("Plugin policy lock poisoned"))?;
    let mut policies = Policies::read(path)?;
    policies.set(key, plugin, policy);
    crate::config::write_config_atomic(path, &serde_json::to_string_pretty(&policies)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn nearest_ancestor_decides_and_redundant_ask_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("policy.json");
        let repo = dir.path().join("repo");
        let worktree = repo.join(".worktrees/feature");
        fs::create_dir_all(&worktree).unwrap();
        let (repo, worktree) = (canonical(&repo), canonical(&worktree));

        set_at(&store, &repo, "kicadmium", PluginPolicy::Never).unwrap();
        set_at(&store, &repo, "visilog", PluginPolicy::Always).unwrap();
        set_at(&store, &worktree, "kicadmium", PluginPolicy::Always).unwrap();
        let policies = Policies::read(&store).unwrap();

        let at_repo = policies.resolve(&repo);
        assert_eq!(at_repo["kicadmium"], PluginPolicy::Never);
        assert_eq!(at_repo["visilog"], PluginPolicy::Always);
        let in_worktree = policies.resolve(&worktree);
        assert_eq!(in_worktree["kicadmium"], PluginPolicy::Always);
        assert_eq!(in_worktree["visilog"], PluginPolicy::Always);
        assert!(policies.resolve(&canonical(dir.path())).is_empty());

        set_at(&store, &repo, "kicadmium", PluginPolicy::Ask).unwrap();
        set_at(&store, &repo, "visilog", PluginPolicy::Ask).unwrap();
        set_at(&store, &worktree, "kicadmium", PluginPolicy::Ask).unwrap();
        let policies = Policies::read(&store).unwrap();
        assert!(policies.resolve(&repo).is_empty());
        assert!(policies.0.is_empty(), "nothing left to override");
    }

    /// "Load when detected" in a subdirectory must beat a repository-wide
    /// Always, not fall back to it.
    #[test]
    fn ask_in_a_subdirectory_overrides_an_inherited_always() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("policy.json");
        let repo = dir.path().join("repo");
        let sub = repo.join("firmware");
        fs::create_dir_all(&sub).unwrap();
        let (repo, sub) = (canonical(&repo), canonical(&sub));

        set_at(&store, &repo, "kicadmium", PluginPolicy::Always).unwrap();
        set_at(&store, &sub, "kicadmium", PluginPolicy::Ask).unwrap();
        let policies = Policies::read(&store).unwrap();
        assert_eq!(policies.resolve(&sub)["kicadmium"], PluginPolicy::Ask);
        assert_eq!(policies.resolve(&repo)["kicadmium"], PluginPolicy::Always);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_reads_the_same_policy() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("policy.json");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        set_at(&store, &canonical(&repo), "kicadmium", PluginPolicy::Never).unwrap();
        let policies = Policies::read(&store).unwrap();
        let key = read_key(&link).unwrap();
        assert_eq!(policies.resolve(&key)["kicadmium"], PluginPolicy::Never);
        assert!(read_key(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn writes_need_an_absolute_existing_directory() {
        let relative = write_key("some/relative/dir").unwrap_err().to_string();
        assert!(relative.contains("absolute path"), "{relative}");
        assert!(write_key("/definitely/not/a/real/dir").is_err());
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
