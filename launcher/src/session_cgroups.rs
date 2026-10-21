//! Per-session cgroup v2 leaves under the launcher's delegated systemd unit.
//! Unsupported hosts keep the ordinary spawn path; only the daemon initializes
//! delegation. CLI commands and tests never move the running launcher.

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use uuid::Uuid;

    static ROOT: OnceLock<PathBuf> = OnceLock::new();

    /// Only accept our own service root, never a parent slice or another unit.
    fn service_root(membership: &str) -> Option<PathBuf> {
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::/"))?;
        let path = Path::new(relative);
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        (path.file_name()? == "agent-portal.service")
            .then(|| Path::new("/sys/fs/cgroup").join(path))
    }

    pub fn initialize() {
        let result = (|| -> Result<()> {
            let manager = std::process::Command::new("systemctl")
                .args([
                    "--user",
                    "show",
                    "--property=MainPID",
                    "--value",
                    "agent-portal.service",
                ])
                .output()?;
            anyhow::ensure!(
                manager.status.success()
                    && String::from_utf8_lossy(&manager.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                        == Some(std::process::id()),
                "this process is not the systemd launcher's MainPID"
            );
            let membership = std::fs::read_to_string("/proc/self/cgroup")?;
            let root =
                service_root(&membership).context("not in the agent-portal systemd service")?;
            enable_memory(&root)?;
            ROOT.set(root)
                .map_err(|_| anyhow::anyhow!("cgroup manager already initialized"))?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!("Per-session memory containment unavailable; using ordinary process groups: {error:#}");
        }
    }

    fn enable_memory(root: &Path) -> Result<()> {
        anyhow::ensure!(
            root.join("cgroup.kill").exists(),
            "kernel lacks cgroup.kill for session teardown"
        );
        let controllers = std::fs::read_to_string(root.join("cgroup.controllers"))?;
        anyhow::ensure!(
            controllers.split_whitespace().any(|s| s == "memory"),
            "memory controller is not delegated; restart the updated systemd service"
        );
        // Probe access before moving anything. systemd owns the delegation;
        // we neither chmod its files nor edit an ancestor slice.
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("cgroup.subtree_control"))?;
        let daemon = root.join("launcher");
        std::fs::create_dir_all(&daemon)?;
        std::fs::write(daemon.join("cgroup.procs"), std::process::id().to_string())?;
        // cgroup v2 requires a processless parent before memory can be
        // enabled below it. A failed enable leaves us safely in our leaf.
        std::fs::write(root.join("cgroup.subtree_control"), "+memory")?;
        Ok(())
    }

    pub struct SessionGroup {
        path: PathBuf,
        wrapper: PathBuf,
    }

    impl SessionGroup {
        pub fn prepare(
            id: Uuid,
            binary: &Path,
            high: Option<u64>,
            max: Option<u64>,
        ) -> Result<Option<Self>> {
            let Some(root) = ROOT.get() else {
                return Ok(None);
            };
            Self::prepare_at(
                root,
                &session_lib::paths::config_dir().join("session-wrappers"),
                id,
                binary,
                high,
                max,
            )
            .map(Some)
        }

        fn prepare_at(
            root: &Path,
            wrapper_dir: &Path,
            id: Uuid,
            binary: &Path,
            high: Option<u64>,
            max: Option<u64>,
        ) -> Result<Self> {
            anyhow::ensure!(
                high.zip(max).is_none_or(|(h, m)| h <= m),
                "session memory high must not exceed max"
            );
            let binary =
                which::which(binary).context("resolve agent executable before wrapping")?;
            std::fs::create_dir_all(wrapper_dir)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(wrapper_dir, std::fs::Permissions::from_mode(0o700))?;
            let path = root.join(format!("session-{id}-{}", Uuid::new_v4()));
            std::fs::create_dir(&path)?;
            let wrapper = wrapper_dir.join(format!("{}.sh", Uuid::new_v4()));
            let group = Self { path, wrapper };
            for (file, limit) in [("memory.high", high), ("memory.max", max)] {
                if let Some(limit) = limit {
                    anyhow::ensure!(limit > 0, "session memory limits must be positive bytes");
                    std::fs::write(group.path.join(file), limit.to_string())?;
                }
            }
            // Kill only the selected victim by default: a compiler failure
            // should not kill the agent reporting it.
            std::fs::write(group.path.join("memory.oom.group"), "0")?;
            write_wrapper(&group.wrapper, &group.path.join("cgroup.procs"), &binary)?;
            Ok(group)
        }

        pub fn command(&self) -> PathBuf {
            self.wrapper.clone()
        }
        pub fn events(&self) -> PathBuf {
            self.path.join("memory.events")
        }
    }

    fn quote(path: &Path) -> Result<String> {
        let value = path.to_str().context("agent path is not UTF-8")?;
        Ok(format!("'{}'", value.replace('\'', "'\\''")))
    }

    fn write_wrapper(wrapper: &Path, procs: &Path, binary: &Path) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let script = format!("#!/bin/sh\n# Join before exec: all agent descendants inherit this cgroup.\nprintf '%s\\n' \"$$\" > {} || {{ echo 'Cannot join session cgroup; refusing uncontained launch' >&2; exit 125; }}\nexec {} \"$@\"\n", quote(procs)?, quote(binary)?);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(wrapper)?;
        file.write_all(script.as_bytes())?;
        Ok(())
    }

    impl Drop for SessionGroup {
        fn drop(&mut self) {
            // Scope is this run's unique leaf only; never touch siblings or the
            // daemon. Group kill also reaps descendants that changed process group.
            if let Err(error) = std::fs::write(self.path.join("cgroup.kill"), "1") {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %self.path.display(), "Could not kill session cgroup descendants: {error}");
                }
            }
            let _ = std::fs::remove_file(&self.wrapper);
            let path = self.path.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    for _ in 0..20 {
                        match std::fs::remove_dir(&path) {
                            Ok(()) => return,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                            Err(_) => {}
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    tracing::warn!(path = %path.display(), "Could not remove finished session cgroup");
                });
            } else {
                let _ = std::fs::remove_dir(&path);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        /// Run only in a disposable, explicitly delegated transient service:
        /// systemd-run --user --wait --pipe --unit=portal-cgroup-regression-<id>
        ///   --property=Delegate=memory <test-binary> <this-test> --ignored --exact
        #[test]
        #[ignore = "requires a disposable delegated portal-cgroup-regression-* unit"]
        fn kernel_oom_is_contained_and_cleanup_spares_siblings() {
            use std::os::unix::process::ExitStatusExt;
            let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
            let relative = membership.trim().strip_prefix("0::/").unwrap();
            let root = Path::new("/sys/fs/cgroup").join(relative);
            assert!(
                root.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("portal-cgroup-regression-"),
                "refusing to touch a real service"
            );
            enable_memory(&root).unwrap();
            let wrappers = tempfile::tempdir().unwrap();
            let victim = SessionGroup::prepare_at(
                &root,
                wrappers.path(),
                Uuid::new_v4(),
                Path::new("/usr/bin/python3"),
                None,
                Some(64 * 1024 * 1024),
            )
            .unwrap();
            std::fs::write(victim.path.join("memory.swap.max"), "0").unwrap();
            let sibling = SessionGroup::prepare_at(
                &root,
                wrappers.path(),
                Uuid::new_v4(),
                Path::new("/bin/sleep"),
                None,
                None,
            )
            .unwrap();
            let mut sibling_child = std::process::Command::new(sibling.command())
                .arg("60")
                .spawn()
                .unwrap();
            let victim_status = std::process::Command::new(victim.command())
                .args(["-c", "x = bytearray(128 * 1024 * 1024)"])
                .status()
                .unwrap();
            assert_eq!(victim_status.signal(), Some(libc::SIGKILL));
            let events = std::fs::read_to_string(victim.events()).unwrap();
            assert!(events.lines().any(|line| line == "oom_kill 1"), "{events}");
            assert!(sibling_child.try_wait().unwrap().is_none());
            // Failed limit setup must not leave an empty cgroup or wrapper.
            let before = std::fs::read_dir(&root).unwrap().count();
            assert!(SessionGroup::prepare_at(
                &root,
                wrappers.path(),
                Uuid::new_v4(),
                Path::new("/bin/true"),
                None,
                Some(0)
            )
            .is_err());
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
            let wrapper = victim.command();
            drop(victim);
            assert!(!wrapper.exists());
            assert!(sibling_child.try_wait().unwrap().is_none());
            drop(sibling);
            assert_eq!(sibling_child.wait().unwrap().signal(), Some(libc::SIGKILL));
        }

        #[test]
        fn only_own_unit_can_be_delegated() {
            assert_eq!(
                service_root("0::/user.slice/app.slice/agent-portal.service\n"),
                Some(PathBuf::from(
                    "/sys/fs/cgroup/user.slice/app.slice/agent-portal.service"
                ))
            );
            for input in [
                "0::/",
                "0::/user.slice",
                "0::/other.service",
                "0::/../agent-portal.service",
                "0::/agent-portal.service/session-foo",
                "1:memory:/agent-portal.service",
            ] {
                assert!(service_root(input).is_none(), "{input}");
            }
        }
        #[test]
        fn wrapper_preserves_arguments_environment_and_join_failure() {
            let dir = tempfile::tempdir().unwrap();
            let wrapper = dir.path().join("wrapper ' spaced.sh");
            let procs = dir.path().join("fake cgroup.procs");
            let binary = dir.path().join("agent ' actual");
            std::os::unix::fs::symlink("/bin/sh", &binary).unwrap();
            write_wrapper(&wrapper, &procs, &binary).unwrap();
            let out = std::process::Command::new(&wrapper)
                .args([
                    "-c",
                    "printf '%s|%s|%s' \"$1\" \"$2\" \"$TEST_VALUE\"",
                    "test",
                    "a b",
                    "$(touch INJECTED)",
                ])
                .env("TEST_VALUE", "unchanged value")
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(out.status.success());
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                "a b|$(touch INJECTED)|unchanged value"
            );
            assert!(std::fs::read_to_string(&procs)
                .unwrap()
                .trim()
                .parse::<u32>()
                .is_ok());
            assert!(!dir.path().join("INJECTED").exists());
            let failed = dir.path().join("failed.sh");
            write_wrapper(
                &failed,
                &dir.path().join("absent/procs"),
                Path::new("/bin/touch"),
            )
            .unwrap();
            let out = std::process::Command::new(&failed)
                .arg(dir.path().join("SHOULD_NOT_RUN"))
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(125));
            assert!(!dir.path().join("SHOULD_NOT_RUN").exists());
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
pub fn initialize() {}

#[cfg(not(target_os = "linux"))]
pub struct SessionGroup;
#[cfg(not(target_os = "linux"))]
impl SessionGroup {
    pub fn prepare(
        _: uuid::Uuid,
        _: &std::path::Path,
        _: Option<u64>,
        _: Option<u64>,
    ) -> anyhow::Result<Option<Self>> {
        Ok(None)
    }
    pub fn command(&self) -> std::path::PathBuf {
        unreachable!("no cgroups on this platform")
    }
    pub fn events(&self) -> std::path::PathBuf {
        unreachable!("no cgroups on this platform")
    }
}
