//! Frontend-only surface state for session-owned work panes.
//!
//! Layout code treats every surface through this small shape. Forwards are the
//! first concrete kind, but board/BOM/viewer surfaces can slot in without
//! teaching the session split about iframes.

use shared::api::ForwardInfo;
use uuid::Uuid;

use crate::utils;

const SPLIT_STORAGE_PREFIX: &str = "agent_portal.session_surface.split.";
pub const DEFAULT_SPLIT_PERCENT: f64 = 50.0;
pub const MIN_SPLIT_PERCENT: f64 = 30.0;
pub const MAX_SPLIT_PERCENT: f64 = 70.0;

#[derive(Clone, PartialEq)]
pub enum SessionSurfaceKind {
    Forward(ForwardInfo),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSurfaceMode {
    Split,
    Fullscreen,
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
