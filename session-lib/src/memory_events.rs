//! Kernel-backed OOM attribution. SIGKILL/exit 137 alone is not evidence of OOM.
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct MemoryEvents {
    path: PathBuf,
    killed: Option<u64>,
    next_poll: Instant,
}

impl MemoryEvents {
    pub(crate) fn new(path: PathBuf) -> Self {
        let killed = read_kills(&path);
        Self {
            path,
            killed,
            next_poll: Instant::now(),
        }
    }

    pub(crate) fn poll(&mut self, force: bool) -> Option<serde_json::Value> {
        if !force && Instant::now() < self.next_poll {
            return None;
        }
        self.next_poll = Instant::now() + Duration::from_secs(1);
        let current = read_kills(&self.path)?;
        let previous = self.killed.replace(current)?;
        let count = current.saturating_sub(previous);
        (count > 0).then(|| shared::PortalMessage::text(format!(
            "Out of memory: the kernel killed {count} process(es) in this session's cgroup. A build or agent process may have stopped."
        )).to_json())
    }
}

fn read_kills(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "oom_kill")
                .then(|| fields.next()?.parse().ok())
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_new_kernel_kills_emit_not_oom_attempts_or_counter_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        std::fs::write(&path, "oom 10\noom_kill 2\n").unwrap();
        let mut monitor = MemoryEvents::new(path.clone());
        assert!(monitor.poll(true).is_none());
        std::fs::write(&path, "oom 20\noom_kill 2\n").unwrap();
        assert!(monitor.poll(true).is_none());
        std::fs::write(&path, "oom 21\noom_kill 4\n").unwrap();
        assert!(monitor
            .poll(true)
            .unwrap()
            .to_string()
            .contains("killed 2 process"));
        assert!(monitor.poll(true).is_none());
        std::fs::write(&path, "oom 0\noom_kill 0\n").unwrap();
        assert!(monitor.poll(true).is_none());
        std::fs::write(&path, "bad").unwrap();
        assert!(monitor.poll(true).is_none());
    }
}
