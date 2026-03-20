use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    #[serde(default)]
    pub sessions: HashMap<String, SessionAuth>,

    /// Directory -> session ID mapping for resume functionality
    #[serde(default)]
    pub directory_sessions: HashMap<String, DirectorySession>,

    #[serde(default)]
    pub preferences: Preferences,
}

/// Tracks the Claude Code session for a specific directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectorySession {
    /// The Claude Code session UUID
    pub session_id: Uuid,
    /// Human-readable session name
    pub session_name: String,
    /// When the session was first created
    pub created_at: String,
    /// Last time this session was used
    pub last_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAuth {
    pub user_id: String,
    pub auth_token: String,
    pub user_email: Option<String>,
    pub last_used: String,
    #[serde(default)]
    pub backend_url: Option<String>,
    #[serde(default)]
    pub session_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preferences {
    #[serde(default)]
    pub default_backend_url: Option<String>,

    #[serde(default)]
    pub auto_open_browser: bool,
}

/// File lock for atomic config operations
pub struct ConfigLock {
    lock_path: PathBuf,
    _file: File,
}

impl ConfigLock {
    /// Acquire an exclusive lock on the config file
    /// Uses a PID-style lockfile with retry logic
    pub fn acquire(config_path: &Path) -> Result<Self> {
        let lock_path = config_path.with_extension("lock");

        // Ensure parent directory exists before creating lock file
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        let max_attempts = 50; // 5 seconds total with 100ms sleep
        let mut attempts = 0;

        loop {
            // Try to create lock file exclusively
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    // Write our PID to the lock file
                    let pid = std::process::id();
                    writeln!(file, "{}", pid)?;
                    file.flush()?;

                    return Ok(ConfigLock {
                        lock_path,
                        _file: file,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Lock exists, check if the process is still alive
                    if let Ok(mut existing) = File::open(&lock_path) {
                        let mut pid_str = String::new();
                        if existing.read_to_string(&mut pid_str).is_ok() {
                            if let Ok(_pid) = pid_str.trim().parse::<u32>() {
                                // Check if process is still running (Unix-specific)
                                #[cfg(unix)]
                                {
                                    // kill with signal 0 checks if process exists
                                    if unsafe { libc::kill(_pid as i32, 0) } != 0 {
                                        // Process is dead, remove stale lock
                                        let _ = fs::remove_file(&lock_path);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    attempts += 1;
                    if attempts >= max_attempts {
                        anyhow::bail!(
                            "Failed to acquire config lock after {} attempts",
                            max_attempts
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e).context("Failed to create lock file"),
            }
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

impl ProxyConfig {
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = directories::ProjectDirs::from("com", "anthropic", "agent-portal")
            .context("Failed to determine config directory")?
            .config_dir()
            .to_path_buf();

        Ok(config_dir.join("config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path).context("Failed to read config file")?;

        let config: Self =
            serde_json::from_str(&contents).context("Failed to parse config file")?;

        Ok(config)
    }

    /// Atomically save the config with file locking
    /// This prevents race conditions when multiple proxy instances run in the same directory
    pub fn atomic_save(&self) -> Result<()> {
        let path = Self::config_path()?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        // Acquire lock
        let _lock = ConfigLock::acquire(&path)?;

        // Write to temp file first
        let temp_path = path.with_extension("tmp");
        let contents = serde_json::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&temp_path, &contents).context("Failed to write temp config file")?;

        // Atomic rename
        fs::rename(&temp_path, &path).context("Failed to rename config file")?;

        // Lock is released on drop
        Ok(())
    }

    /// Load config with file locking (for read-modify-write operations)
    pub fn load_locked() -> Result<(Self, ConfigLock)> {
        let path = Self::config_path()?;
        let lock = ConfigLock::acquire(&path)?;

        let config = if path.exists() {
            let contents = fs::read_to_string(&path).context("Failed to read config file")?;
            serde_json::from_str(&contents).context("Failed to parse config file")?
        } else {
            Self::default()
        };

        Ok((config, lock))
    }

    /// Save config while holding a lock
    pub fn save_with_lock(&self, _lock: &ConfigLock) -> Result<()> {
        let path = Self::config_path()?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        // Write to temp file first
        let temp_path = path.with_extension("tmp");
        let contents = serde_json::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&temp_path, &contents).context("Failed to write temp config file")?;

        // Atomic rename
        fs::rename(&temp_path, &path).context("Failed to rename config file")?;

        Ok(())
    }

    pub fn get_session_auth(&self, working_dir: &str) -> Option<&SessionAuth> {
        self.sessions.get(working_dir)
    }

    /// Find any valid cached auth token, regardless of working directory.
    /// Returns the most recently used token. Used by shim mode as a fallback
    /// when no token exists for the current working directory.
    pub fn get_any_session_auth(&self) -> Option<&SessionAuth> {
        self.sessions
            .values()
            .max_by(|a, b| a.last_used.cmp(&b.last_used))
    }

    pub fn set_session_auth(&mut self, working_dir: String, auth: SessionAuth) {
        self.sessions.insert(working_dir, auth);
    }

    pub fn remove_session_auth(&mut self, working_dir: &str) -> Option<SessionAuth> {
        self.sessions.remove(working_dir)
    }

    pub fn set_backend_url(&mut self, working_dir: &str, url: &str) {
        if let Some(auth) = self.sessions.get_mut(working_dir) {
            auth.backend_url = Some(url.to_string());
        }
    }

    pub fn set_session_prefix(&mut self, working_dir: &str, prefix: &str) {
        if let Some(auth) = self.sessions.get_mut(working_dir) {
            auth.session_prefix = Some(prefix.to_string());
        }
    }

    pub fn get_backend_url(&self, working_dir: &str) -> Option<&str> {
        self.sessions
            .get(working_dir)
            .and_then(|auth| auth.backend_url.as_deref())
    }

    // =========================================================================
    // Directory Session Methods
    // =========================================================================

    /// Get the saved session for a directory (for resuming)
    pub fn get_directory_session(&self, working_dir: &str) -> Option<&DirectorySession> {
        self.directory_sessions.get(working_dir)
    }

    /// Set the session for a directory
    pub fn set_directory_session(&mut self, working_dir: String, session: DirectorySession) {
        self.directory_sessions.insert(working_dir, session);
    }

    /// Update the last_used timestamp for a directory session
    pub fn touch_directory_session(&mut self, working_dir: &str) {
        if let Some(session) = self.directory_sessions.get_mut(working_dir) {
            session.last_used = chrono::Utc::now().to_rfc3339();
        }
    }

    /// Create a new directory session
    pub fn create_directory_session(session_id: Uuid, session_name: String) -> DirectorySession {
        let now = chrono::Utc::now().to_rfc3339();
        DirectorySession {
            session_id,
            session_name,
            created_at: now.clone(),
            last_used: now,
        }
    }
}
