//! Frontend-only surface state for session-owned work panes.
//!
//! Layout code treats every surface through this small shape. Forwards are the
//! first concrete kind, but board/BOM/viewer surfaces can slot in without
//! teaching the session split about iframes.

use serde::{Deserialize, Serialize};
use shared::api::ForwardInfo;
use uuid::Uuid;

use crate::utils;

const SPLIT_STORAGE_PREFIX: &str = "agent_portal.session_surface.split.";
const OPEN_STORAGE_PREFIX: &str = "agent_portal.session_surface.open.";
pub const DEFAULT_SPLIT_PERCENT: f64 = 50.0;
pub const MIN_SPLIT_PERCENT: f64 = 30.0;
pub const MAX_SPLIT_PERCENT: f64 = 70.0;

#[derive(Clone, PartialEq)]
pub enum SessionSurfaceKind {
    Forward(ForwardInfo),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSurfaceMode {
    Split,
    Fullscreen,
}

/// Durable layout preference for an open forward surface.
///
/// The forward URL is deliberately excluded: a restored surface is matched by
/// port against the authoritative forward list and rebuilt from fresh server
/// metadata before an iframe is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardSurfaceMemory {
    pub port: u16,
    pub mode: SessionSurfaceMode,
    pub collapsed: bool,
}

#[derive(Clone, PartialEq)]
pub struct SessionSurface {
    pub id: String,
    pub session_id: Uuid,
    pub title: String,
    pub subtitle: String,
    pub mode: SessionSurfaceMode,
    pub collapsed: bool,
    pub kind: SessionSurfaceKind,
}

impl SessionSurface {
    pub fn from_forward(session_id: Uuid, forward: ForwardInfo, mode: SessionSurfaceMode) -> Self {
        let title = match &forward.process {
            Some(process) => format!("{process} :{}", forward.port),
            None => format!(":{}", forward.port),
        };

        Self {
            id: format!("forward:{}", forward.port),
            session_id,
            title,
            subtitle: forward.url.clone(),
            mode,
            collapsed: false,
            kind: SessionSurfaceKind::Forward(forward),
        }
    }

    pub fn forward(&self) -> Option<&ForwardInfo> {
        match &self.kind {
            SessionSurfaceKind::Forward(forward) => Some(forward),
        }
    }

    pub fn update_forward(&mut self, next: ForwardInfo) {
        let mode = self.mode;
        let collapsed = self.collapsed;
        *self = Self::from_forward(self.session_id, next, mode);
        self.collapsed = collapsed;
    }

    pub fn memory(&self) -> ForwardSurfaceMemory {
        let port = match &self.kind {
            SessionSurfaceKind::Forward(forward) => forward.port,
        };
        ForwardSurfaceMemory {
            port,
            mode: self.mode,
            collapsed: self.collapsed,
        }
    }
}

impl ForwardSurfaceMemory {
    /// Restore only from fresh server metadata. A dead or replaced forward
    /// therefore cannot resurrect a stale iframe URL after a page reload.
    pub fn restore(self, session_id: Uuid, forwards: &[ForwardInfo]) -> Option<SessionSurface> {
        let forward = forwards.iter().find(|forward| forward.port == self.port)?;
        let mut surface = SessionSurface::from_forward(session_id, forward.clone(), self.mode);
        surface.collapsed = self.collapsed;
        Some(surface)
    }
}

pub fn load_open_surface(session_id: Uuid) -> Option<ForwardSurfaceMemory> {
    let raw = utils::storage_get(&open_storage_key(session_id))?;
    serde_json::from_str(&raw).ok()
}

pub fn save_open_surface(surface: &SessionSurface) {
    if let Ok(raw) = serde_json::to_string(&surface.memory()) {
        utils::storage_set(&open_storage_key(surface.session_id), &raw);
    }
}

pub fn clear_open_surface(session_id: Uuid) {
    utils::storage_remove(&open_storage_key(session_id));
}

pub fn clamp_split_percent(value: f64) -> f64 {
    value.clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT)
}

pub fn load_split_percent(session_id: Uuid) -> f64 {
    let key = split_storage_key(session_id);
    utils::storage_get(&key)
        .and_then(|value| value.parse::<f64>().ok())
        .map(clamp_split_percent)
        .unwrap_or(DEFAULT_SPLIT_PERCENT)
}

pub fn save_split_percent(session_id: Uuid, value: f64) {
    let key = split_storage_key(session_id);
    utils::storage_set(&key, &format!("{:.1}", clamp_split_percent(value)));
}

fn split_storage_key(session_id: Uuid) -> String {
    format!("{SPLIT_STORAGE_PREFIX}{session_id}")
}

fn open_storage_key(session_id: Uuid) -> String {
    format!("{OPEN_STORAGE_PREFIX}{session_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(port: u16) -> ForwardInfo {
        ForwardInfo {
            port,
            url: format!("https://example.test/{port}"),
            created_at: "2026-09-22T00:00:00Z".into(),
            public: false,
            listening: Some(true),
            process: Some("test-server".into()),
        }
    }

    #[test]
    fn surface_memory_restores_from_fresh_forward_metadata() {
        let session_id = Uuid::new_v4();
        let memory = ForwardSurfaceMemory {
            port: 3000,
            mode: SessionSurfaceMode::Fullscreen,
            collapsed: true,
        };
        let fresh = forward(3000);

        let restored = memory
            .restore(session_id, std::slice::from_ref(&fresh))
            .unwrap();

        assert_eq!(restored.forward(), Some(&fresh));
        assert_eq!(restored.mode, SessionSurfaceMode::Fullscreen);
        assert!(restored.collapsed);
    }

    #[test]
    fn surface_memory_rejects_a_missing_forward() {
        let memory = ForwardSurfaceMemory {
            port: 3000,
            mode: SessionSurfaceMode::Split,
            collapsed: false,
        };

        assert!(memory.restore(Uuid::new_v4(), &[forward(4000)]).is_none());
    }
}
