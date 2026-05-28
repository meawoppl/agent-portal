use shared::ServerToLauncher;
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

use super::SessionManager;

pub type LauncherSender = mpsc::UnboundedSender<ServerToLauncher>;

/// A connected launcher daemon
pub struct LauncherConnection {
    pub sender: LauncherSender,
    pub launcher_name: String,
    pub hostname: String,
    pub user_id: Uuid,
    pub running_sessions: Vec<Uuid>,
    pub working_directory: Option<String>,
    pub version: String,
    /// SHA256 hash of the launcher's current auth token (for revocation on renewal)
    pub token_hash: Option<String>,
    /// When the launcher's auth token expires
    pub token_expires_at: Option<chrono::NaiveDateTime>,
}

impl SessionManager {
    /// Atomically register a launcher, rejecting a duplicate `(user_id, hostname)`
    /// pair in the same operation.
    ///
    /// Race-free: the check-and-claim happens under a single `DashMap` shard
    /// lock on the dedup index, so two concurrent connections from the same
    /// `(user_id, hostname)` cannot both pass the check and both insert.
    ///
    /// On success, returns `Ok(())` and inserts the connection into
    /// `launchers`. On a duplicate, returns `Err(existing_launcher_name)` and
    /// does not touch `launchers`.
    pub fn try_register_launcher(
        &self,
        launcher_id: Uuid,
        connection: LauncherConnection,
    ) -> Result<(), String> {
        let dedup_key = (connection.user_id, connection.hostname.clone());

        // Use `entry().or_insert_with` so the duplicate check and the
        // reservation are atomic under one shard lock.
        let entry = self.launcher_dedup.entry(dedup_key).or_insert(launcher_id);
        let claimed_by = *entry.value();

        if claimed_by != launcher_id {
            // Someone else won the race for this (user_id, hostname).
            let existing_name = self
                .launchers
                .get(&claimed_by)
                .map(|c| c.launcher_name.clone())
                .unwrap_or_else(|| claimed_by.to_string());
            return Err(existing_name);
        }
        // We hold the reservation; drop the shard guard before mutating
        // `launchers` so other lookups on this shard aren't blocked.
        drop(entry);

        info!(
            "Registering launcher: {} ({})",
            connection.launcher_name, launcher_id
        );
        self.launchers.insert(launcher_id, connection);
        Ok(())
    }

    pub fn unregister_launcher(&self, launcher_id: &Uuid) {
        info!("Unregistering launcher: {}", launcher_id);
        if let Some((_, connection)) = self.launchers.remove(launcher_id) {
            // Only release the dedup slot if it still points at us — a
            // newer-generation registration for the same (user_id, hostname)
            // would have replaced this entry's value and must not be evicted.
            let dedup_key = (connection.user_id, connection.hostname);
            self.launcher_dedup
                .remove_if(&dedup_key, |_, claimed_by| claimed_by == launcher_id);
        }
    }

    pub fn get_launchers_for_user(&self, user_id: &Uuid) -> Vec<shared::LauncherInfo> {
        self.launchers
            .iter()
            .filter(|entry| entry.value().user_id == *user_id)
            .map(|entry| shared::LauncherInfo {
                launcher_id: *entry.key(),
                launcher_name: entry.value().launcher_name.clone(),
                hostname: entry.value().hostname.clone(),
                connected: true,
                running_sessions: entry.value().running_sessions.len() as u32,
                working_directory: entry.value().working_directory.clone(),
                version: entry.value().version.clone(),
                token_expires_at: entry
                    .value()
                    .token_expires_at
                    .map(|dt| dt.and_utc().to_rfc3339()),
            })
            .collect()
    }

    pub fn send_to_launcher(&self, launcher_id: &Uuid, msg: ServerToLauncher) -> bool {
        if let Some(launcher) = self.launchers.get(launcher_id) {
            launcher.sender.send(msg).is_ok()
        } else {
            false
        }
    }

    /// Find the launcher running a given session and send StopSession to it.
    /// Returns true if the message was sent successfully.
    pub fn stop_session_on_launcher(&self, session_id: Uuid) -> bool {
        for entry in self.launchers.iter() {
            if entry.value().running_sessions.contains(&session_id) {
                return entry
                    .value()
                    .sender
                    .send(ServerToLauncher::StopSession { session_id })
                    .is_ok();
            }
        }
        false
    }

    /// Stop a running process on its launcher without removing the launcher's
    /// persisted expected-session metadata.
    pub fn pause_session_on_launcher(&self, session_id: Uuid) -> bool {
        for entry in self.launchers.iter() {
            if entry.value().running_sessions.contains(&session_id) {
                return entry
                    .value()
                    .sender
                    .send(ServerToLauncher::PauseSession { session_id })
                    .is_ok();
            }
        }
        false
    }
}
