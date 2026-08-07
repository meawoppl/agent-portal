//! Single source of truth for the launcher/proxy on-disk config directory.
//!
//! Standardized to `~/.config/agent-portal` on Unix (Linux **and** macOS) and
//! `%APPDATA%\agent-portal` on Windows. This matches the install script, which
//! writes both the binary and `launcher.json` to `~/.config/agent-portal` on
//! every platform (see `backend/src/handlers/downloads.rs`).
//!
//! Previously every call site built this from
//! `ProjectDirs::from("com", "anthropic", "agent-portal")`, whose Unix impl
//! follows the XDG spec and drops the qualifier/organization — so it resolved
//! to `~/.config/agent-portal` on Linux but
//! `~/Library/Application Support/com.anthropic.agent-portal` on macOS. That
//! disagreed with the installer on macOS: the launcher read a directory the
//! installer never wrote to, silently dropping the self-hosted `backend_url`
//! (#1591). Collapsing the path into one helper — and dropping the reverse-DNS
//! identifier that read as vendor attribution — makes the installer, the docs,
//! and the code agree on every platform.

use std::path::{Path, PathBuf};

/// The config directory: `~/.config/agent-portal` on Unix, `%APPDATA%\agent-portal`
/// on Windows. Holds `launcher.json`, the proxy's `config.json`, the machine-id
/// file, `codex_threads.json`, the `buffers/` subdirectory, and (via the
/// installer) the `agent-portal` binary itself.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        // `BaseDirs::config_dir()` is `%APPDATA%` (Roaming) on Windows.
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("agent-portal"))
            .unwrap_or_else(|| PathBuf::from("agent-portal"))
    }
    #[cfg(not(windows))]
    {
        // `~/.config/agent-portal` regardless of `$XDG_CONFIG_HOME`, matching
        // the install script's hard-coded `${HOME}/.config/agent-portal`.
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".config").join("agent-portal"))
            .unwrap_or_else(|| PathBuf::from("/tmp/agent-portal"))
    }
}

/// The pre-#1591 location, still produced by `ProjectDirs` for the migration.
/// On Linux this resolves to the same path as [`config_dir`] (XDG drops the
/// qualifier), so migration is a no-op there; macOS/Windows resolve it to the
/// old per-OS directory that held the live config.
fn legacy_config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "anthropic", "agent-portal")
        .map(|p| p.config_dir().to_path_buf())
}

/// One-time, best-effort migration from the legacy `ProjectDirs` location to
/// [`config_dir`]. Idempotent; safe to call on every startup. It never fails
/// startup — a migration error just means the user may need to run
/// `agent-portal login` again, which is strictly better than crashing.
///
/// The legacy config is authoritative (it is what the running launcher has
/// been using), so overlapping files overwrite the destination — in particular
/// the installer's `launcher.json` stub, which on macOS held only `backend_url`
/// and no auth token. The legacy directory never contains the binary (on macOS
/// it is `Application Support`, and the installer puts the binary under
/// `~/.config`), so overwriting can't clobber the executable.
pub fn migrate_legacy_config_dir() {
    let new = config_dir();
    let Some(legacy) = legacy_config_dir() else {
        return;
    };
    migrate_between(&legacy, &new);
}

/// Testable core of [`migrate_legacy_config_dir`] with explicit paths.
fn migrate_between(legacy: &Path, new: &Path) {
    if !legacy.exists() {
        return;
    }
    // Same directory (the Linux case, or already migrated): nothing to do.
    let same = std::fs::canonicalize(legacy)
        .ok()
        .zip(std::fs::canonicalize(new).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if same {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new) {
        tracing::warn!("config migration: could not create {}: {e}", new.display());
        return;
    }
    let entries = match std::fs::read_dir(legacy) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("config migration: could not read {}: {e}", legacy.display());
            return;
        }
    };
    let mut moved = 0usize;
    for entry in entries.flatten() {
        let from = entry.path();
        let dest = new.join(entry.file_name());
        match move_path(&from, &dest) {
            Ok(()) => moved += 1,
            Err(e) => tracing::warn!(
                "config migration: failed to move {} -> {}: {e}",
                from.display(),
                dest.display()
            ),
        }
    }
    if moved > 0 {
        tracing::info!(
            "Migrated {moved} config entr{} from {} to {} (#1591)",
            if moved == 1 { "y" } else { "ies" },
            legacy.display(),
            new.display()
        );
    }
    // The now-emptied legacy directory is left in place; removing it is
    // unnecessary and risks deleting something we didn't create.
}

/// Move `from` onto `dest`, overwriting any existing destination. Tries a
/// rename first (same-volume — the normal case, both live under `$HOME`) and
/// falls back to a recursive copy + remove across volumes. Handles both files
/// and directories (the `buffers/` subdirectory).
fn move_path(from: &Path, dest: &Path) -> std::io::Result<()> {
    // Clear any destination stub so a rename doesn't fail on a non-empty dir
    // and files are replaced cleanly.
    if dest.exists() {
        if dest.is_dir() {
            std::fs::remove_dir_all(dest)?;
        } else {
            std::fs::remove_file(dest)?;
        }
    }
    match std::fs::rename(from, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device or other rename failure: deep copy, then remove.
            copy_recursive(from, dest)?;
            if from.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            }
        }
    }
}

fn copy_recursive(from: &Path, dest: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, dest).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_is_under_dot_config_on_unix() {
        // On the CI/dev Unix hosts this must land at ~/.config/agent-portal,
        // matching the installer — never Application Support.
        #[cfg(not(windows))]
        {
            let dir = config_dir();
            assert!(
                dir.ends_with(".config/agent-portal"),
                "expected ~/.config/agent-portal, got {}",
                dir.display()
            );
            assert!(
                !dir.to_string_lossy().contains("Application Support"),
                "must not resolve to the macOS ProjectDirs path: {}",
                dir.display()
            );
        }
    }

    #[test]
    fn migration_moves_legacy_config_and_overwrites_stub() {
        let tmp =
            std::env::temp_dir().join(format!("agent-portal-mig-{}-{}", std::process::id(), "a"));
        let legacy = tmp.join("legacy");
        let new = tmp.join("new");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&new).unwrap();

        // Legacy holds the live config (token) + a buffers subdir; new holds
        // the installer stub (backend_url only) and the "binary".
        std::fs::write(
            legacy.join("launcher.json"),
            r#"{"backend_url":"wss://real","auth_token":"tok"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(legacy.join("buffers")).unwrap();
        std::fs::write(legacy.join("buffers").join("s.json"), "[]").unwrap();
        std::fs::write(new.join("launcher.json"), r#"{"backend_url":"wss://real"}"#).unwrap();
        std::fs::write(new.join("agent-portal"), b"ELF").unwrap();

        migrate_between(&legacy, &new);

        // Live config (with token) overwrote the stub; buffers came across; the
        // binary is untouched.
        assert_eq!(
            std::fs::read_to_string(new.join("launcher.json")).unwrap(),
            r#"{"backend_url":"wss://real","auth_token":"tok"}"#
        );
        assert!(new.join("buffers").join("s.json").exists());
        assert_eq!(std::fs::read(new.join("agent-portal")).unwrap(), b"ELF");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn migration_is_noop_when_paths_are_the_same() {
        // The Linux case: legacy and new canonicalize to the same dir.
        let tmp = std::env::temp_dir().join(format!("agent-portal-mig-{}-b", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("launcher.json"), "keep").unwrap();

        migrate_between(&tmp, &tmp);

        assert_eq!(
            std::fs::read_to_string(tmp.join("launcher.json")).unwrap(),
            "keep"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
