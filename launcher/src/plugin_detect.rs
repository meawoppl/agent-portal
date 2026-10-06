//! Plugin `[[detect]]` matching.
//!
//! This mirrors the backend's plugin inventory detection
//! (`backend/src/handlers/plugins.rs`) exactly, so `agent-portal plugin
//! preview --cwd` answers "would Portal suggest this plugin here?" the same way
//! the dashboard does. Keep the two in lockstep: a pattern is matched against
//! **file names only** (never paths), using `*` (any name), a leading `*`
//! (suffix match), a trailing `*` (prefix match), or an exact name. The walk
//! skips dot-directories, stops descending past [`MAX_SCAN_DEPTH`], and gives
//! up (no match) after [`MAX_SCAN_ENTRIES`] entries.

use std::{fs, path::Path};

const MAX_SCAN_DEPTH: usize = 4;
const MAX_SCAN_ENTRIES: usize = 4096;

/// One `[[detect]]` entry from a plugin manifest.
#[derive(Debug, Clone)]
pub(crate) struct DetectRule {
    pub name: String,
    pub any: Vec<String>,
}

/// The reason the first matching rule fires for `root`, or `None` when no rule
/// matches. Rules with an empty `any` never match.
pub(crate) fn detect_reason(root: &Path, detects: &[DetectRule]) -> Option<String> {
    for detect in detects {
        if detect.any.is_empty() {
            continue;
        }
        if directory_matches(root, &detect.any) {
            return Some(format!(
                "Detected {} files ({})",
                detect.name,
                detect.any.join(", ")
            ));
        }
    }
    None
}

fn directory_matches(root: &Path, patterns: &[String]) -> bool {
    if path_matches(root, patterns) {
        return true;
    }
    if !root.is_dir() {
        return false;
    }

    let mut seen = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_SCAN_ENTRIES {
                return false;
            }
            let path = entry.path();
            if path_matches(&path, patterns) {
                return true;
            }
            if depth < MAX_SCAN_DEPTH && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with('.') {
                    stack.push((path, depth + 1));
                }
            }
        }
    }
    false
}

fn path_matches(path: &Path, patterns: &[String]) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    patterns
        .iter()
        .any(|pattern| file_name_matches(file_name, pattern))
}

fn file_name_matches(file_name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return file_name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return file_name.starts_with(prefix);
    }
    file_name == pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, any: &[&str]) -> DetectRule {
        DetectRule {
            name: name.to_string(),
            any: any.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    #[test]
    fn file_name_patterns() {
        assert!(file_name_matches("board.kicad_pcb", "*.kicad_pcb"));
        assert!(file_name_matches("README.md", "README*"));
        assert!(file_name_matches("sdkconfig", "sdkconfig"));
        assert!(file_name_matches("anything", "*"));
        assert!(!file_name_matches("board.kicad_sch", "*.kicad_pcb"));
        // Patterns are matched against file names only, never paths.
        assert!(!file_name_matches("main.c", "main/*.c"));
    }

    #[test]
    fn matches_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("hw/boards/rev2/top.kicad_pcb"));
        let reason = detect_reason(dir.path(), &[rule("kicad", &["*.kicad_pcb"])]).unwrap();
        assert_eq!(reason, "Detected kicad files (*.kicad_pcb)");
    }

    #[test]
    fn first_matching_rule_wins_and_empty_any_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("top.sv"));
        let rules = [
            rule("empty", &[]),
            rule("kicad", &["*.kicad_pcb"]),
            rule("fpga", &["*.v", "*.sv"]),
        ];
        assert_eq!(
            detect_reason(dir.path(), &rules).as_deref(),
            Some("Detected fpga files (*.v, *.sv)")
        );
    }

    #[test]
    fn respects_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        // Root children are depth 0, so files up to depth MAX_SCAN_DEPTH match.
        touch(&dir.path().join("a/b/c/d/ok.kicad_pcb"));
        assert!(detect_reason(dir.path(), &[rule("k", &["*.kicad_pcb"])]).is_some());

        let deep = tempfile::tempdir().unwrap();
        touch(&deep.path().join("a/b/c/d/e/deep.kicad_pcb"));
        assert!(detect_reason(deep.path(), &[rule("k", &["*.kicad_pcb"])]).is_none());
    }

    #[test]
    fn skips_dot_directories() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join(".git/hidden.kicad_pcb"));
        assert!(detect_reason(dir.path(), &[rule("k", &["*.kicad_pcb"])]).is_none());
    }

    #[test]
    fn no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("src/main.rs"));
        assert!(detect_reason(dir.path(), &[rule("k", &["*.kicad_pcb"])]).is_none());
    }
}
