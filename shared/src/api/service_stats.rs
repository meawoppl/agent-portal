//! Resource usage of the portal's running services: the backend process and
//! every launcher daemon connected for the user. Launchers sample themselves
//! (see `portal-sysmon`) and ride the figures on their heartbeat; the backend
//! adds its own row and fans the table out as
//! `ServerToClient::ServiceStatsUpdate`.

use serde::{Deserialize, Serialize};

/// One process's reading of itself and its host.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct SystemSample {
    /// Unix epoch milliseconds when the reading was taken.
    pub sampled_at_ms: u64,
    /// Logical CPU count of the host.
    pub cores: u32,
    /// Whole-host CPU busy share, 0–100.
    pub host_cpu_percent: f32,
    /// The service's own CPU use as a share of one core, 0–100 per core (a
    /// multi-threaded burst can exceed 100).
    pub process_cpu_percent: f32,
    /// Resident set size of the service process.
    pub process_rss_bytes: u64,
    pub host_mem_used_bytes: u64,
    pub host_mem_total_bytes: u64,
    /// 1, 5 and 15 minute load averages; zeros where the OS has none.
    pub load_avg: [f32; 3],
}

/// What kind of portal service a row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// The portal backend serving this client.
    Backend,
    /// A launcher daemon on one of the user's machines.
    Launcher,
}

/// One row of the services monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceStats {
    /// Stable key for trend buffers: `backend` or the launcher UUID.
    pub id: String,
    pub kind: ServiceKind,
    /// Display name (launcher name or `backend`).
    pub name: String,
    pub hostname: String,
    /// Sessions the service is currently running (launchers) or serving
    /// (backend: live proxy connections).
    pub sessions: u32,
    /// The service's reported version (launcher build, or the backend's own).
    pub version: Option<String>,
    /// Latest reading, or `None` while the service has never reported one
    /// (a launcher built before system stats existed never will).
    pub sample: Option<SystemSample>,
}

impl SystemSample {
    /// Host memory in use as a share, 0–1; `None` without a total.
    pub fn mem_fraction(&self) -> Option<f32> {
        (self.host_mem_total_bytes > 0)
            .then(|| self.host_mem_used_bytes as f32 / self.host_mem_total_bytes as f32)
    }
}

/// `1.5 GB`, `512 MB`, `3.0 KB` — short enough for a header chip.
pub fn format_bytes_short(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format_short() {
        assert_eq!(format_bytes_short(512), "512 B");
        assert_eq!(format_bytes_short(1536), "1.5 KB");
        assert_eq!(format_bytes_short(734_003_200), "700 MB");
        assert_eq!(format_bytes_short(17_179_869_184), "16.0 GB");
    }

    #[test]
    fn service_stats_round_trip_and_defaults() {
        let row = ServiceStats {
            id: "backend".into(),
            kind: ServiceKind::Backend,
            name: "backend".into(),
            hostname: "portal".into(),
            sessions: 3,
            version: Some("2.15.17".into()),
            sample: Some(SystemSample {
                sampled_at_ms: 1,
                cores: 8,
                host_cpu_percent: 12.5,
                process_cpu_percent: 3.0,
                process_rss_bytes: 1 << 20,
                host_mem_used_bytes: 4 << 30,
                host_mem_total_bytes: 16 << 30,
                load_avg: [0.5, 0.4, 0.3],
            }),
        };
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.contains("\"kind\":\"backend\""));
        assert_eq!(serde_json::from_str::<ServiceStats>(&json).unwrap(), row);
        assert!((row.sample.unwrap().mem_fraction().unwrap() - 0.25).abs() < 1e-6);
        assert_eq!(SystemSample::default().mem_fraction(), None);
        let silent = ServiceStats {
            sample: None,
            version: None,
            ..row
        };
        let json = serde_json::to_string(&silent).unwrap();
        assert!(json.contains("\"sample\":null"));
        assert_eq!(serde_json::from_str::<ServiceStats>(&json).unwrap(), silent);
    }
}
