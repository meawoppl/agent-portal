//! Heap-profile capture for memory spikes.
//!
//! On Linux the backend runs on jemalloc with sampled heap profiling: one
//! allocation stack is recorded per ~512 KiB allocated (`malloc_conf` in
//! `main.rs`), at a cost of a few percent. Sampling alone writes nothing.
//!
//! With `PORTAL_HEAP_PROFILE_DIR` set, a watchdog reads jemalloc's live
//! allocated bytes every second. When they reach
//! `PORTAL_HEAP_PROFILE_THRESHOLD_MB` it writes a symbolized pprof profile of
//! the live heap, then another at each further [`STEP_BYTES`] of growth. A
//! spike that a container memory cap is about to kill is therefore captured
//! mid-growth, with the stacks that hold the memory, rather than lost with the
//! process. The trigger re-arms once the heap falls below half the threshold.
//!
//! Inspect a capture with `go tool pprof -top <file>` (or `pprof -top`).

use std::path::{Path, PathBuf};

const DIR_VAR: &str = "PORTAL_HEAP_PROFILE_DIR";
const THRESHOLD_VAR: &str = "PORTAL_HEAP_PROFILE_THRESHOLD_MB";
const DEFAULT_THRESHOLD_MB: u64 = 700;
const MIB: u64 = 1024 * 1024;
/// Growth past the last capture that triggers the next one.
pub const STEP_BYTES: u64 = 256 * MIB;
/// Captures kept in the directory; the oldest are deleted beyond this.
const MAX_PROFILES: usize = 24;
const POLL: std::time::Duration = std::time::Duration::from_secs(1);
const FILE_PREFIX: &str = "heap-";
const FILE_SUFFIX: &str = ".pb.gz";

/// Watchdog settings from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeapProfileConfig {
    pub dir: PathBuf,
    pub threshold_bytes: u64,
}

impl HeapProfileConfig {
    /// `None` when `PORTAL_HEAP_PROFILE_DIR` is unset or empty. A malformed
    /// threshold is a startup error rather than a silently disabled watchdog.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_values(
            std::env::var(DIR_VAR).ok().as_deref(),
            std::env::var(THRESHOLD_VAR).ok().as_deref(),
        )
    }

    fn from_values(dir: Option<&str>, threshold_mb: Option<&str>) -> anyhow::Result<Option<Self>> {
        let Some(dir) = dir.map(str::trim).filter(|d| !d.is_empty()) else {
            return Ok(None);
        };
        let threshold_mb = match threshold_mb.map(str::trim).filter(|t| !t.is_empty()) {
            None => DEFAULT_THRESHOLD_MB,
            Some(raw) => match raw.parse::<u64>() {
                Ok(mb) if mb > 0 => mb,
                _ => anyhow::bail!("{THRESHOLD_VAR} must be a positive whole number of MiB"),
            },
        };
        Ok(Some(Self {
            dir: PathBuf::from(dir),
            threshold_bytes: threshold_mb.saturating_mul(MIB),
        }))
    }
}

/// Decides when a heap reading warrants a capture.
#[derive(Debug)]
struct Trigger {
    threshold: u64,
    next: u64,
}

impl Trigger {
    fn new(threshold: u64) -> Self {
        Self {
            threshold,
            next: threshold,
        }
    }

    /// True when `allocated` should be captured. Each capture raises the bar
    /// by [`STEP_BYTES`] past the reading, so a fast spike yields a short
    /// series rather than one file per second; falling below half the
    /// threshold re-arms it for the next spike.
    fn observe(&mut self, allocated: u64) -> bool {
        if allocated >= self.next {
            self.next = allocated.saturating_add(STEP_BYTES);
            return true;
        }
        if allocated < self.threshold / 2 {
            self.next = self.threshold;
        }
        false
    }
}

/// Start the watchdog when configured. Logs and returns when profiling is
/// unavailable (not Linux, or jemalloc profiling off), so a misconfigured host
/// still serves traffic.
pub fn spawn(config: Option<HeapProfileConfig>) {
    let Some(config) = config else {
        return;
    };
    #[cfg(target_os = "linux")]
    {
        let Some(prof) = jemalloc_pprof::PROF_CTL.as_ref().cloned() else {
            tracing::warn!("{DIR_VAR} is set but jemalloc heap profiling is not enabled");
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&config.dir) {
            tracing::warn!(
                "{DIR_VAR} {} is not usable ({e}); heap profiles disabled",
                config.dir.display()
            );
            return;
        }
        tracing::info!(
            "Heap profile watchdog: capturing to {} from {} MiB",
            config.dir.display(),
            config.threshold_bytes / MIB
        );
        tokio::spawn(watch(config, prof));
    }
    #[cfg(not(target_os = "linux"))]
    tracing::warn!(
        "{DIR_VAR} is set but heap profiling is Linux-only; ignoring {}",
        config.dir.display()
    );
}

#[cfg(target_os = "linux")]
async fn watch(
    config: HeapProfileConfig,
    prof: std::sync::Arc<tokio::sync::Mutex<jemalloc_pprof::JemallocProfCtl>>,
) {
    let mut trigger = Trigger::new(config.threshold_bytes);
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let Some(allocated) = allocated_bytes() else {
            continue;
        };
        if !trigger.observe(allocated) {
            continue;
        }
        let mib = allocated / MIB;
        match capture(&config.dir, &prof, mib).await {
            Ok(path) => tracing::warn!(
                "{}: live heap {mib} MiB; wrote {}",
                crate::markers::HEAP_PROFILE_CAPTURED,
                path.display()
            ),
            Err(e) => tracing::warn!(
                "{}: live heap {mib} MiB; profile not written: {e}",
                crate::markers::HEAP_PROFILE_CAPTURED
            ),
        }
    }
}

/// Bytes currently allocated by the application, per jemalloc.
#[cfg(target_os = "linux")]
fn allocated_bytes() -> Option<u64> {
    // jemalloc caches its statistics; advancing the epoch refreshes them.
    tikv_jemalloc_ctl::epoch::advance().ok()?;
    tikv_jemalloc_ctl::stats::allocated::read()
        .ok()
        .map(|b| b as u64)
}

#[cfg(target_os = "linux")]
async fn capture(
    dir: &Path,
    prof: &std::sync::Arc<tokio::sync::Mutex<jemalloc_pprof::JemallocProfCtl>>,
    mib: u64,
) -> anyhow::Result<PathBuf> {
    let path = dir.join(profile_name(chrono::Utc::now(), mib));
    let mut ctl = prof.clone().lock_owned().await;
    let dir = dir.to_path_buf();
    let target = path.clone();
    // Dumping and symbolizing is blocking work; keep it off the runtime.
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let pprof = ctl.dump_pprof()?;
        std::fs::write(&target, pprof)?;
        prune(&dir, MAX_PROFILES)?;
        Ok(())
    })
    .await??;
    Ok(path)
}

/// Names sort chronologically, which `prune` relies on.
fn profile_name(at: chrono::DateTime<chrono::Utc>, mib: u64) -> String {
    format!(
        "{FILE_PREFIX}{}-{mib}mib{FILE_SUFFIX}",
        at.format("%Y%m%dT%H%M%SZ")
    )
}

/// Delete the oldest captures beyond `keep`. Only files this module names
/// are considered, so a shared directory's other contents are untouched.
fn prune(dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut profiles: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(FILE_PREFIX) && n.ends_with(FILE_SUFFIX))
        })
        .collect();
    profiles.sort();
    let excess = profiles.len().saturating_sub(keep);
    for old in &profiles[..excess] {
        std::fs::remove_file(old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_off_without_a_directory() {
        assert_eq!(
            HeapProfileConfig::from_values(None, Some("100")).unwrap(),
            None
        );
        assert_eq!(
            HeapProfileConfig::from_values(Some("  "), None).unwrap(),
            None
        );
    }

    #[test]
    fn config_defaults_and_rejects_bad_thresholds() {
        let config = HeapProfileConfig::from_values(Some("/data/heap"), None)
            .unwrap()
            .unwrap();
        assert_eq!(config.dir, PathBuf::from("/data/heap"));
        assert_eq!(config.threshold_bytes, DEFAULT_THRESHOLD_MB * MIB);
        let custom = HeapProfileConfig::from_values(Some("/d"), Some("300"))
            .unwrap()
            .unwrap();
        assert_eq!(custom.threshold_bytes, 300 * MIB);
        for bad in ["0", "-5", "1.5", "lots"] {
            assert!(
                HeapProfileConfig::from_values(Some("/d"), Some(bad)).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn trigger_captures_a_spike_as_a_short_series_and_rearms() {
        let mut t = Trigger::new(700 * MIB);
        assert!(!t.observe(150 * MIB));
        assert!(t.observe(700 * MIB));
        // Within one step of the last capture: no new file every second.
        assert!(!t.observe(800 * MIB));
        assert!(t.observe(1100 * MIB));
        assert!(!t.observe(1200 * MIB));
        // Still elevated but shrinking: stays quiet, does not re-arm.
        assert!(!t.observe(500 * MIB));
        assert!(!t.observe(700 * MIB));
        // Back to normal re-arms the original threshold.
        assert!(!t.observe(200 * MIB));
        assert!(t.observe(720 * MIB));
    }

    #[test]
    fn names_sort_chronologically() {
        use chrono::TimeZone;
        let earlier = profile_name(
            chrono::Utc.with_ymd_and_hms(2026, 11, 15, 9, 5, 0).unwrap(),
            2000,
        );
        let later = profile_name(
            chrono::Utc
                .with_ymd_and_hms(2026, 11, 15, 10, 0, 0)
                .unwrap(),
            700,
        );
        assert_eq!(earlier, "heap-20261115T090500Z-2000mib.pb.gz");
        assert!(earlier < later);
    }

    #[test]
    fn prune_keeps_the_newest_captures_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(
                dir.path()
                    .join(format!("heap-2026111{i}T000000Z-1mib.pb.gz")),
                b"x",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"keep").unwrap();
        prune(dir.path(), 2).unwrap();
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "heap-20261113T000000Z-1mib.pb.gz",
                "heap-20261114T000000Z-1mib.pb.gz",
                "notes.txt"
            ]
        );
    }
}
