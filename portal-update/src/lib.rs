//! Self-update functionality for Claude Portal binaries.
//!
//! On startup, checks if a newer version is available from GitHub releases
//! and self-updates if necessary. Parameterized by binary name so both
//! `claude-portal` and `agent-portal` can use it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use tracing::info;
#[cfg(windows)]
use tracing::warn;

/// GitHub repository for releases
const GITHUB_REPO: &str = "meawoppl/agent-portal";

/// Result of an update check
#[derive(Debug)]
pub enum UpdateResult {
    /// Binary is up to date
    UpToDate,
    /// Binary was updated, needs relaunch
    Updated,
    /// Update available but not installed (check-only mode)
    UpdateAvailable {
        version: String,
        download_url: String,
    },
}

/// Platform information for selecting the correct binary
#[derive(Debug, Clone)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
    pub binary_name: String,
}

impl Platform {
    /// Detect the current platform for a given binary prefix.
    ///
    /// The `binary_prefix` is the base name of the binary (e.g. "claude-portal"
    /// or "agent-portal"). The platform suffix is appended automatically.
    pub fn current(binary_prefix: &str) -> Self {
        let (os, arch) = if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            ("linux", "x86_64")
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            ("linux", "aarch64")
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            ("darwin", "aarch64")
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
            ("darwin", "x86_64")
        } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
            ("windows", "x86_64")
        } else {
            ("unknown", "unknown")
        };

        let binary_name = match (os, arch) {
            ("linux", "x86_64") => format!("{}-linux-x86_64", binary_prefix),
            ("linux", "aarch64") => format!("{}-linux-aarch64", binary_prefix),
            ("darwin", "aarch64") => format!("{}-darwin-aarch64", binary_prefix),
            ("darwin", "x86_64") => format!("{}-darwin-x86_64", binary_prefix),
            ("windows", "x86_64") => format!("{}-windows-x86_64.exe", binary_prefix),
            _ => binary_prefix.to_string(),
        };

        Platform {
            os,
            arch,
            binary_name,
        }
    }
}

/// GitHub release asset from the API
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    /// SHA256 digest in format "sha256:abc123..."
    digest: Option<String>,
}

/// GitHub release from the API
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: String,
    assets: Vec<GitHubAsset>,
}

/// Compute SHA256 hash of bytes
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    hex::encode(hash)
}

/// Check for updates from GitHub releases.
///
/// `binary_prefix` is the base name (e.g. "claude-portal" or "agent-portal").
/// If `check_only` is true, reports availability without installing.
pub async fn check_for_update(binary_prefix: &str, check_only: bool) -> Result<UpdateResult> {
    let self_path = std::env::current_exe().context("Failed to get current executable path")?;
    let self_bytes = fs::read(&self_path).context("Failed to read current binary")?;
    let self_hash = sha256_hex(&self_bytes);

    info!("Current binary hash: {}", &self_hash[..16]);

    let platform = Platform::current(binary_prefix);
    if platform.os == "unknown" {
        bail!(
            "Unsupported platform: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }

    info!("Checking for updates from GitHub releases...");
    info!("Platform: {} {}", platform.os, platform.arch);

    let client = reqwest::Client::builder()
        .user_agent("agent-portal")
        .build()
        .context("Failed to create HTTP client")?;

    // Get the latest release from GitHub API
    let api_url = format!(
        "https://api.github.com/repos/{}/releases/tags/latest",
        GITHUB_REPO
    );

    let resp = client
        .get(&api_url)
        .send()
        .await
        .context("Failed to fetch GitHub release info")?;

    if !resp.status().is_success() {
        bail!("GitHub API returned {}", resp.status());
    }

    let release: GitHubRelease = resp
        .json()
        .await
        .context("Failed to parse GitHub release JSON")?;
    info!("Latest release: {} ({})", release.name, release.tag_name);

    // Find the asset for our platform
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == platform.binary_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No binary found for platform {} {} in release assets. Available: {:?}",
                platform.os,
                platform.arch,
                release.assets.iter().map(|a| &a.name).collect::<Vec<_>>()
            )
        })?;

    info!("Found asset: {}", asset.name);

    // Check if we can compare hashes without downloading (GitHub provides digest in API)
    if let Some(ref digest) = asset.digest {
        // GitHub digest format: "sha256:abc123..."
        if let Some(remote_hash) = digest.strip_prefix("sha256:") {
            info!("Remote binary hash: {}", &remote_hash[..16]);
            if self_hash == remote_hash {
                info!("Binary is up to date (verified via API)");
                return Ok(UpdateResult::UpToDate);
            }
            info!("Update available (hash mismatch)");
        }
    }

    if check_only {
        return Ok(UpdateResult::UpdateAvailable {
            version: release.name,
            download_url: asset.browser_download_url.clone(),
        });
    }

    // Download the new binary
    info!("Downloading update from GitHub...");
    let resp = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("Failed to download from GitHub")?;

    if !resp.status().is_success() {
        bail!("Download failed: GitHub returned {}", resp.status());
    }

    let new_binary = resp
        .bytes()
        .await
        .context("Failed to read download response")?;
    let new_hash = sha256_hex(&new_binary);

    info!("Downloaded binary hash: {}", &new_hash[..16]);

    // Verify downloaded hash matches what we expected (if we had a digest)
    if let Some(ref digest) = asset.digest {
        if let Some(remote_hash) = digest.strip_prefix("sha256:") {
            if new_hash != remote_hash {
                bail!(
                    "Downloaded binary hash mismatch! Expected {}, got {}",
                    &remote_hash[..16],
                    &new_hash[..16]
                );
            }
        }
    }

    // Final check - maybe we already have this version
    if self_hash == new_hash {
        info!("Binary is up to date");
        return Ok(UpdateResult::UpToDate);
    }

    install_binary(&self_path, &new_binary)?;
    Ok(UpdateResult::Updated)
}

/// Install a new binary by atomically replacing the current executable
fn install_binary(self_path: &std::path::Path, new_binary: &[u8]) -> Result<()> {
    info!("Installing update...");

    // Atomic replacement: write to temp file, then rename
    let temp_path = self_path.with_extension("tmp");
    fs::write(&temp_path, new_binary).context("Failed to write temporary file")?;

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_path, perms)?;
    }

    // On Windows, we can't replace a running executable directly
    #[cfg(windows)]
    {
        // Try to rename the current binary to .old first
        let old_path = self_path.with_extension("old.exe");
        let _ = fs::remove_file(&old_path); // Remove any existing .old file

        match fs::rename(self_path, &old_path) {
            Ok(_) => {
                // Successfully moved current binary, now rename temp to current
                if let Err(e) = fs::rename(&temp_path, self_path) {
                    // Try to restore the old binary
                    let _ = fs::rename(&old_path, self_path);
                    bail!("Failed to install update: {}", e);
                }
                // Clean up old binary
                let _ = fs::remove_file(&old_path);
                info!("Update installed successfully");
                return Ok(());
            }
            Err(_) => {
                // Binary is locked - save as pending update
                let pending_path = self_path.with_extension("new.exe");
                fs::rename(&temp_path, &pending_path).context("Failed to save pending update")?;
                info!(
                    "Update saved to {}. It will be applied on next startup.",
                    pending_path.display()
                );
                bail!("Update pending - will be applied on next startup");
            }
        }
    }

    #[cfg(not(windows))]
    {
        // Atomic rename on Unix
        fs::rename(&temp_path, self_path).context("Failed to replace binary")?;
        info!("Update installed successfully");
        Ok(())
    }
}

/// Check for and apply pending updates (Windows only)
///
/// On Windows, if the binary was locked during an update, we save the new
/// version as `.new.exe`. This function checks for and applies that update.
pub fn apply_pending_update() -> Result<bool> {
    #[cfg(windows)]
    {
        let self_path = std::env::current_exe().context("Failed to get current executable path")?;
        let pending_path = self_path.with_extension("new.exe");
        if pending_path.exists() {
            info!("Found pending update at {}", pending_path.display());

            // Try to replace the current binary
            let old_path = self_path.with_extension("old.exe");
            let _ = fs::remove_file(&old_path);

            // Try to move current to old
            match fs::rename(&self_path, &old_path) {
                Ok(_) => {
                    // Move pending to current
                    if let Err(e) = fs::rename(&pending_path, &self_path) {
                        // Restore old binary
                        let _ = fs::rename(&old_path, &self_path);
                        warn!("Failed to apply pending update: {}", e);
                        return Ok(false);
                    }
                    // Clean up
                    let _ = fs::remove_file(&old_path);
                    info!("Pending update applied successfully");
                    return Ok(true);
                }
                Err(e) => {
                    warn!("Cannot apply pending update (binary still locked?): {}", e);
                    return Ok(false);
                }
            }
        }
    }

    // No pending update or not Windows
    Ok(false)
}
