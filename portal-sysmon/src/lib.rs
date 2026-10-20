//! Resource sampling for portal services (the backend and every launcher).
//!
//! One [`SystemMonitor`] per process; call [`SystemMonitor::sample`] on a
//! timer. CPU figures are the usage since the previous sample, so the first
//! call reports zero and the caller should discard or expect that. Everything
//! native lives here so `shared` stays WASM-clean.

use std::time::{SystemTime, UNIX_EPOCH};

use shared::SystemSample;
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System,
};

pub struct SystemMonitor {
    system: System,
    pid: Option<Pid>,
    hostname: String,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let pid = sysinfo::get_current_pid().ok();
        if let Some(pid) = pid {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }
        Self {
            system,
            pid,
            hostname: System::host_name().unwrap_or_else(|| "unknown".to_string()),
        }
    }

    /// Host name as the OS reports it.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Refresh and read: host CPU (all cores, 0–100), own-process CPU (0–100
    /// of one core, so can exceed 100 on multi-threaded work), RSS, host
    /// memory and the 1/5/15 minute load averages (0 on platforms without).
    pub fn sample(&mut self) -> SystemSample {
        self.system.refresh_cpu_usage();
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let mut process_cpu = 0.0;
        let mut rss = 0;
        if let Some(pid) = self.pid {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
            if let Some(p) = self.system.process(pid) {
                process_cpu = p.cpu_usage();
                rss = p.memory();
            }
        }
        let load = System::load_average();
        SystemSample {
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            cores: self.system.cpus().len() as u32,
            host_cpu_percent: self.system.global_cpu_usage(),
            process_cpu_percent: process_cpu,
            process_rss_bytes: rss,
            host_mem_used_bytes: self.system.used_memory(),
            host_mem_total_bytes: self.system.total_memory(),
            load_avg: [load.one as f32, load.five as f32, load.fifteen as f32],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_sane() {
        let mut m = SystemMonitor::new();
        let s = m.sample();
        assert!(s.cores > 0);
        assert!(s.host_mem_total_bytes > 0);
        assert!(s.host_mem_used_bytes <= s.host_mem_total_bytes);
        assert!((0.0..=100.0).contains(&s.host_cpu_percent));
        assert!(
            s.process_rss_bytes > 0,
            "a running test has resident memory"
        );
        assert!(s.sampled_at_ms > 0);
        assert!(!m.hostname().is_empty());
    }
}
