use shared::ServerToLauncher;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::SessionManager;

pub type LauncherSender = mpsc::UnboundedSender<ServerToLauncher>;

/// A connected launcher daemon.
///
/// `gen` identifies THIS socket: a launcher that reconnects reuses its
/// stable `launcher_id`, so without a generation the old socket's cleanup
/// would remove the new socket's entry (the launcher half of #1256 — the
/// proxy half was fixed earlier with `ProxyConnection.gen`). `cancel`
/// force-closes the socket task when the entry is evicted.
pub struct LauncherConnection {
    pub sender: LauncherSender,
    pub launcher_name: String,
    pub hostname: String,
    pub user_id: Uuid,
    pub running_sessions: Vec<Uuid>,
    pub working_directory: Option<String>,
    pub version: String,
    pub cancel: CancellationToken,
    /// Stamped by `try_register_launcher`; caller-supplied values are ignored.
    pub gen: u64,
}

impl SessionManager {
    /// Atomically register a launcher, rejecting a duplicate `(user_id, hostname)`
    /// pair in the same operation.
    ///
    /// Race-free: the check-and-claim happens under a single `DashMap` shard
    /// lock on the dedup index, so two concurrent connections from the same
    /// `(user_id, hostname)` cannot both pass the check and both insert.
    ///
    /// On success, inserts the connection into `launchers` and returns the
    /// stamped connection generation, which must be passed to
    /// `unregister_launcher` so a stale socket's cleanup can't remove a
    /// newer same-id registration. On a duplicate, returns
    /// `Err(existing_launcher_name)` and does not touch `launchers`.
    pub fn try_register_launcher(
        &self,
        launcher_id: Uuid,
        mut connection: LauncherConnection,
    ) -> Result<u64, String> {
        connection.gen = self.gen_counter.fetch_add(1, Ordering::Relaxed);
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

        let gen = connection.gen;
        info!(
            "Registering launcher: {} ({}) (gen={})",
            connection.launcher_name, launcher_id, gen
        );
        self.launchers.insert(launcher_id, connection);
        Ok(gen)
    }

    /// Unregister a launcher. If `gen` is provided, only removes the entry
    /// when it matches — a reconnect reuses the same `launcher_id`, so the
    /// old socket's cleanup must not remove the new socket's registration.
    /// Pass `None` to force removal.
    pub fn unregister_launcher(&self, launcher_id: &Uuid, gen: Option<u64>) {
        let removed = match gen {
            Some(expected) => self
                .launchers
                .remove_if(launcher_id, |_, conn| conn.gen == expected)
                .map(|(_, conn)| conn),
            None => self.launchers.remove(launcher_id).map(|(_, conn)| conn),
        };
        match removed {
            Some(connection) => {
                info!("Unregistering launcher: {}", launcher_id);
                // Only release the dedup slot if it still points at us — a
                // different launcher_id may have claimed (user_id, hostname)
                // since, and must not be evicted.
                let dedup_key = (connection.user_id, connection.hostname);
                self.launcher_dedup
                    .remove_if(&dedup_key, |_, claimed_by| claimed_by == launcher_id);
            }
            None => {
                if gen.is_some() {
                    info!(
                        "Skipping stale unregister for launcher {} (superseded or already gone)",
                        launcher_id
                    );
                }
            }
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
        match self.launchers.get(launcher_id) {
            Some(launcher) => match launcher.sender.send(msg) {
                Ok(()) => true,
                Err(_) => {
                    // Channel closed while still registered: the socket task
                    // is dead or half-open. Evict (gen-guarded, releases the
                    // dedup slot) and cancel so the transport closes and the
                    // launcher reconnects, instead of every reconcile send
                    // failing silently forever (#1256).
                    let gen = launcher.gen;
                    let cancel = launcher.cancel.clone();
                    drop(launcher);
                    warn!(
                        "Launcher {} channel closed; evicting and closing its socket",
                        launcher_id
                    );
                    self.unregister_launcher(launcher_id, Some(gen));
                    cancel.cancel();
                    false
                }
            },
            None => false,
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
        // Resolve the target id first, then send through the evicting path —
        // a direct send off the iter guard would bypass dead-connection
        // eviction (and eviction must not run under the same shard lock).
        if let Some(id) = self.launcher_running_session(session_id) {
            return self.send_to_launcher(&id, msg());
        }
        if let Some(launcher_id) = launcher_id {
            return self.send_to_launcher(&launcher_id, msg());
        }
        false
    }

    /// The launcher whose last heartbeat reported this session as running.
    fn launcher_running_session(&self, session_id: Uuid) -> Option<Uuid> {
        self.launchers
            .iter()
            .find(|e| e.value().running_sessions.contains(&session_id))
            .map(|e| *e.key())
    }

    /// Stop a running process on its launcher without removing the launcher's
    /// persisted expected-session metadata.
    pub fn pause_session_on_launcher(&self, session_id: Uuid) -> bool {
        match self.launcher_running_session(session_id) {
            Some(id) => self.send_to_launcher(&id, ServerToLauncher::PauseSession { session_id }),
            None => false,
        }
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
            cancel: CancellationToken::new(),
            gen: 0,
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

    /// The launcher half of #1256: a launcher reconnect reuses the same
    /// `launcher_id`, so the OLD socket's cleanup (running late, after the
    /// new socket registered) must not remove the NEW registration.
    #[test]
    fn stale_launcher_unregister_is_noop() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id = Uuid::new_v4();

        let old_gen = mgr
            .try_register_launcher(id, make_launcher_connection(user_id, "host1"))
            .expect("first registration");

        // Same launcher_id reconnects (registry entry replaced, new gen).
        let new_gen = mgr
            .try_register_launcher(id, make_launcher_connection(user_id, "host1"))
            .expect("same-id reconnect must succeed");
        assert_ne!(old_gen, new_gen);

        // Old socket's late cleanup: must be a no-op.
        mgr.unregister_launcher(&id, Some(old_gen));
        assert!(mgr.launchers.contains_key(&id));
        assert_eq!(mgr.launchers.get(&id).unwrap().gen, new_gen);

        // The dedup slot must still be claimed (a different launcher_id
        // for the same (user, host) is still rejected).
        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_id, "host1"))
            .is_err());

        // New socket's own cleanup still works.
        mgr.unregister_launcher(&id, Some(new_gen));
        assert!(!mgr.launchers.contains_key(&id));
    }

    /// A send to a registered launcher whose channel has died must evict the
    /// entry (releasing the dedup slot) and fire its cancel token.
    #[test]
    fn launcher_send_failure_evicts_and_cancels() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id = Uuid::new_v4();

        let (sender, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let conn = LauncherConnection {
            sender,
            launcher_name: "l".to_string(),
            hostname: "host1".to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
            cancel: cancel.clone(),
            gen: 0,
        };
        mgr.try_register_launcher(id, conn).expect("register");
        drop(rx);

        assert!(!mgr.send_to_launcher(
            &id,
            ServerToLauncher::ServerShutdown {
                reason: "test".to_string(),
                reconnect_delay_ms: 0,
            }
        ));
        assert!(!mgr.launchers.contains_key(&id));
        assert!(cancel.is_cancelled());

        // Dedup slot released: same (user, host) can register again.
        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_id, "host1"))
            .is_ok());
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
        mgr.unregister_launcher(&id_a, None);

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
                        cancel: CancellationToken::new(),
                        gen: 0,
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
