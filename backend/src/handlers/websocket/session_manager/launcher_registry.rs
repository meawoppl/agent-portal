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
            })
            .collect()
    }

    pub fn launcher_version(&self, launcher_id: Option<Uuid>) -> Option<String> {
        launcher_id.and_then(|id| {
            self.launchers
                .get(&id)
                .map(|launcher| launcher.version.clone())
        })
    }

    pub fn send_to_launcher(&self, launcher_id: &Uuid, msg: ServerToLauncher) -> bool {
        if let Some(launcher) = self.launchers.get(launcher_id) {
            launcher.sender.send(msg).is_ok()
        } else {
            false
        }
    }

    /// Find the launcher running a given session and send StopSession to it.
    /// If the session is paused, fall back to the persisted launcher id so
    /// the launcher can remove its expected-session metadata by directory.
    /// Returns true if the message was sent successfully.
    pub fn stop_session_on_launcher(
        &self,
        session_id: Uuid,
        launcher_id: Option<Uuid>,
        working_directory: Option<String>,
    ) -> bool {
        let msg = || ServerToLauncher::StopSession {
            session_id,
            working_directory: working_directory.clone(),
        };
        for entry in self.launchers.iter() {
            if entry.value().running_sessions.contains(&session_id) {
                return entry.value().sender.send(msg()).is_ok();
            }
        }
        if let Some(launcher_id) = launcher_id {
            return self.send_to_launcher(&launcher_id, msg());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_launcher_connection(user_id: Uuid, hostname: &str) -> LauncherConnection {
        let (sender, _rx) = mpsc::unbounded_channel();
        LauncherConnection {
            sender,
            launcher_name: format!("launcher-{}", hostname),
            hostname: hostname.to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
        }
    }

    #[test]
    fn try_register_launcher_rejects_serial_duplicate() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(id_a, make_launcher_connection(user_id, "host1"))
            .is_ok());

        let err = mgr
            .try_register_launcher(id_b, make_launcher_connection(user_id, "host1"))
            .expect_err("second registration for same (user, host) must be rejected");
        assert_eq!(err, format!("launcher-host1"));

        assert_eq!(mgr.launchers.len(), 1);
        assert!(mgr.launchers.contains_key(&id_a));
        assert!(!mgr.launchers.contains_key(&id_b));
    }

    #[test]
    fn try_register_launcher_allows_different_user_same_host() {
        let mgr = SessionManager::new();
        let user_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_a, "host1"))
            .is_ok());
        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_b, "host1"))
            .is_ok());
        assert_eq!(mgr.launchers.len(), 2);
    }

    #[test]
    fn launcher_version_returns_connected_launcher_version() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let launcher_id = Uuid::new_v4();

        mgr.try_register_launcher(launcher_id, make_launcher_connection(user_id, "host1"))
            .expect("register launcher");

        assert_eq!(
            mgr.launcher_version(Some(launcher_id)),
            Some("test".to_string())
        );
        assert_eq!(mgr.launcher_version(Some(Uuid::new_v4())), None);
        assert_eq!(mgr.launcher_version(None), None);
    }

    #[test]
    fn unregister_launcher_releases_dedup_slot() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(id_a, make_launcher_connection(user_id, "host1"))
            .is_ok());
        mgr.unregister_launcher(&id_a);

        // After unregister the same (user, host) should be claimable again.
        assert!(mgr
            .try_register_launcher(id_b, make_launcher_connection(user_id, "host1"))
            .is_ok());
        assert_eq!(mgr.launchers.len(), 1);
        assert!(mgr.launchers.contains_key(&id_b));
    }

    /// Regression test for #790: concurrent registrations from the same
    /// `(user_id, hostname)` must result in exactly one launcher being
    /// registered. Previously `find_duplicate_launcher` + `register_launcher`
    /// were separate steps, so two simultaneous connections could both pass
    /// the duplicate check and both insert.
    #[tokio::test]
    async fn concurrent_launcher_registrations_dedupe_to_single_entry() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let hostname = "race-host";

        // Spawn 10 concurrent registration attempts for the same (user, host).
        let mut handles = Vec::with_capacity(10);
        for _ in 0..10 {
            let mgr_clone = mgr.clone();
            let host = hostname.to_string();
            handles.push(tokio::spawn(async move {
                let launcher_id = Uuid::new_v4();
                let conn = {
                    let (sender, _rx) = mpsc::unbounded_channel();
                    LauncherConnection {
                        sender,
                        launcher_name: format!("launcher-{}", launcher_id),
                        hostname: host,
                        user_id,
                        running_sessions: Vec::new(),
                        working_directory: None,
                        version: "test".to_string(),
                    }
                };
                mgr_clone
                    .try_register_launcher(launcher_id, conn)
                    .map(|_| launcher_id)
            }));
        }

        let mut successes = 0usize;
        let mut failures = 0usize;
        for h in handles {
            match h.await.expect("task panicked") {
                Ok(_) => successes += 1,
                Err(_) => failures += 1,
            }
        }

        // Exactly one registration must succeed; the other nine must be
        // rejected as duplicates. And the launchers map must hold exactly
        // one entry for this (user, host).
        assert_eq!(successes, 1, "exactly one registration should succeed");
        assert_eq!(failures, 9, "nine registrations should be rejected");
        assert_eq!(
            mgr.launchers.len(),
            1,
            "launchers map must contain exactly one entry after the race"
        );
        let user_count = mgr
            .launchers
            .iter()
            .filter(|e| e.value().user_id == user_id && e.value().hostname == hostname)
            .count();
        assert_eq!(user_count, 1);
    }
}
