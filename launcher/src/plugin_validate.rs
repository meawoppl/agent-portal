//! `agent-portal plugin validate <path>`: static checks for a plugin directory.
//!
//! Validation never installs the plugin or runs any of its commands. The
//! manifest is parsed as a raw `toml::Value` (not the launcher's typed
//! `PluginManifest`) so unknown keys can be reported as warnings instead of
//! being silently dropped by serde, and so one malformed field doesn't hide
//! every other finding.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;
use toml::{Table, Value};

use crate::plugin::MANIFEST;
use crate::plugin_detect::DetectRule;

/// Placeholders the launcher expands in manifest command strings.
const KNOWN_PLACEHOLDERS: &[&str] = &[
    "plugin_dir",
    "plugin_home",
    "toolchain_root",
    "toolchain_home",
    "toolchain",
    "cwd",
    "session_id",
    "port",
];

/// Placeholders the plugin docs list but the launcher does not expand yet; they
/// reach the command literally, so they warn rather than fail.
const RESERVED_PLACEHOLDERS: &[&str] = &["artifact_dir", "backend_url"];

/// Agents a `[[skills]].agents` entry may name.
const KNOWN_AGENTS: &[&str] = &["claude", "codex", "muse", "antigravity"];

/// Detect patterns that match nearly every repository.
const BROAD_DETECT_PATTERNS: &[&str] = &[
    "*", "*.md", "*.txt", "*.json", "*.toml", "*.yaml", "*.yml", "*.py", "*.rs", "*.js", "*.ts",
    "Makefile", "README*",
];

const TOP_LEVEL_KEYS: &[&str] = &[
    "schema_version",
    "name",
    "display_name",
    "description",
    "homepage",
    "license",
    "compat",
    "install",
    "surface",
    "commands",
    "toolchains",
    "capabilities",
    "skills",
    "prompts",
    "detect",
    "mcp",
];
const COMPAT_KEYS: &[&str] = &["agent_portal", "platforms"];
const INSTALL_KEYS: &[&str] = &["setup", "doctor"];
const SURFACE_KEYS: &[&str] = &[
    "kind",
    "default_title",
    "default_width_percent",
    "health_path",
    "start",
    "stop",
    "ready_url",
    "env",
];
const COMMAND_KEYS: &[&str] = &["name", "description", "run", "accepts_args"];
const TOOLCHAIN_KEYS: &[&str] = &["name", "description", "home", "install", "doctor", "env"];
const SKILL_KEYS: &[&str] = &["name", "path", "agents"];
const PROMPT_KEYS: &[&str] = &["name", "path"];
const DETECT_KEYS: &[&str] = &["name", "any"];
const MCP_KEYS: &[&str] = &["name", "command", "args", "env"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Issue {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub ok: bool,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

impl Report {
    fn error(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.errors.push(Issue {
            field: field.into(),
            message: message.into(),
        });
    }

    fn warn(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(Issue {
            field: field.into(),
            message: message.into(),
        });
    }

    /// Print `error:`/`warning:` lines and a summary line.
    pub fn print(&self) {
        for issue in &self.errors {
            println!("error: {}: {}", issue.field, issue.message);
        }
        for issue in &self.warnings {
            println!("warning: {}: {}", issue.field, issue.message);
        }
        println!("{}", self.summary());
    }

    pub fn summary(&self) -> String {
        format!(
            "{}: {} error{}, {} warning{}",
            if self.ok { "OK" } else { "FAILED" },
            self.errors.len(),
            if self.errors.len() == 1 { "" } else { "s" },
            self.warnings.len(),
            if self.warnings.len() == 1 { "" } else { "s" },
        )
    }
}

/// CLI entry point. Exits non-zero only when the report has errors.
pub fn validate(path: &Path, json: bool) -> Result<()> {
    let report = validate_dir(path);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Validating {}", path.join(MANIFEST).display());
        report.print();
    }
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Read and parse `agent-portal-plugin.toml` as a raw table.
pub(crate) fn read_manifest(root: &Path) -> Result<Table> {
    let path = root.join(MANIFEST);
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    body.parse::<Table>()
        .with_context(|| format!("failed to parse {}", path.display()))
}

/// Validate a plugin directory without installing or running it.
pub(crate) fn validate_dir(root: &Path) -> Report {
    let mut report = Report::default();
    match read_manifest(root) {
        Ok(manifest) => check_manifest(root, &manifest, &mut report),
        Err(err) => report.error(MANIFEST, format!("{err:#}")),
    }
    report.ok = report.errors.is_empty();
    report
}

fn check_manifest(root: &Path, manifest: &Table, report: &mut Report) {
    unknown_keys(manifest, TOP_LEVEL_KEYS, "", report);

    match manifest.get("schema_version") {
        None => report.warn("schema_version", "missing; set `schema_version = 1`"),
        Some(Value::Integer(1)) => {}
        Some(other) => report.error(
            "schema_version",
            format!("unsupported schema_version {other}; expected 1"),
        ),
    }

    check_name(root, manifest, report);
    for key in ["display_name", "description"] {
        opt_str(manifest, key, key, report);
    }
    for key in ["homepage", "license"] {
        if opt_str(manifest, key, key, report).is_none() && !manifest.contains_key(key) {
            report.warn(key, format!("missing `{key}`"));
        }
    }

    match opt_table(manifest, "compat", "compat", report) {
        Some(compat) => {
            unknown_keys(compat, COMPAT_KEYS, "compat", report);
            opt_str(compat, "agent_portal", "compat.agent_portal", report);
            opt_str_array(compat, "platforms", "compat.platforms", report);
        }
        None => report.warn("compat", "missing [compat] section"),
    }

    match opt_table(manifest, "install", "install", report) {
        Some(install) => {
            unknown_keys(install, INSTALL_KEYS, "install", report);
            for key in ["setup", "doctor"] {
                let field = format!("install.{key}");
                if let Some(cmd) = opt_str(install, key, &field, report) {
                    check_placeholders(cmd, &field, report);
                }
            }
            if !install.contains_key("doctor") {
                report.warn("install.doctor", "missing `doctor` command");
            }
        }
        None => report.warn("install.doctor", "missing `doctor` command"),
    }

    if let Some(surface) = opt_table(manifest, "surface", "surface", report) {
        check_surface(surface, report);
    }
    check_commands(manifest, report);
    check_toolchains(manifest, report);
    check_skills(root, manifest, report);
    check_prompts(root, manifest, report);
    check_detect(manifest, report);
    check_mcp(manifest, report);

    if let Some(value) = manifest.get("capabilities") {
        if !value.is_table() {
            report.error("capabilities", "must be a table");
        }
    }
}

fn check_name(root: &Path, manifest: &Table, report: &mut Report) {
    let Some(name) = opt_str(manifest, "name", "name", report) else {
        if !manifest.contains_key("name") {
            report.error("name", "missing `name`");
        }
        return;
    };
    if !valid_plugin_name(name) {
        report.error(
            "name",
            format!("invalid name `{name}`; use lowercase letters, digits, and hyphens"),
        );
        return;
    }
    let dir_name = root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    if let Some(dir_name) = dir_name {
        if dir_name != name {
            report.warn(
                "name",
                format!(
                    "manifest name `{name}` differs from directory `{dir_name}`; \
                     the install directory will be `{name}`"
                ),
            );
        }
    }
}

fn valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_surface(surface: &Table, report: &mut Report) {
    unknown_keys(surface, SURFACE_KEYS, "surface", report);
    if let Some(kind) = opt_str(surface, "kind", "surface.kind", report) {
        if kind != "http" {
            report.warn(
                "surface.kind",
                format!("unknown surface kind `{kind}`; only `http` is supported"),
            );
        }
    }
    opt_str(surface, "default_title", "surface.default_title", report);
    match surface.get("start") {
        None => report.error("surface.start", "a [surface] needs a `start` command"),
        Some(_) => {
            if let Some(start) = opt_str(surface, "start", "surface.start", report) {
                if start.trim().is_empty() {
                    report.error("surface.start", "`start` is empty");
                }
                check_placeholders(start, "surface.start", report);
            }
        }
    }
    for key in ["stop", "ready_url"] {
        let field = format!("surface.{key}");
        if let Some(cmd) = opt_str(surface, key, &field, report) {
            check_placeholders(cmd, &field, report);
        }
    }
    match surface.get("health_path") {
        None => report.warn(
            "surface.health_path",
            "missing `health_path`; readiness falls back to `/`",
        ),
        Some(_) => {
            if let Some(path) = opt_str(surface, "health_path", "surface.health_path", report) {
                if !path.starts_with('/') {
                    report.error("surface.health_path", "must start with `/`");
                }
            }
        }
    }
    match surface.get("default_width_percent") {
        None => {}
        Some(Value::Integer(width)) if (10..=90).contains(width) => {}
        Some(Value::Integer(width)) => report.error(
            "surface.default_width_percent",
            format!("{width} is outside 10..=90"),
        ),
        Some(_) => report.error("surface.default_width_percent", "must be an integer"),
    }
    if let Some(env) = opt_table(surface, "env", "surface.env", report) {
        check_env(env, "surface.env", report);
    }
}

fn check_commands(manifest: &Table, report: &mut Report) {
    let mut seen = BTreeSet::new();
    for (i, entry) in array_of_tables(manifest, "commands", report) {
        let field = format!("commands[{i}]");
        unknown_keys(entry, COMMAND_KEYS, &field, report);
        if let Some(name) = required_str(entry, "name", &field, report) {
            if !seen.insert(name.to_string()) {
                report.error(
                    format!("{field}.name"),
                    format!("duplicate command `{name}`"),
                );
            }
        }
        opt_str(
            entry,
            "description",
            &format!("{field}.description"),
            report,
        );
        if let Some(run) = required_str(entry, "run", &field, report) {
            if run.trim().is_empty() {
                report.error(format!("{field}.run"), "`run` is empty");
            }
            check_placeholders(run, &format!("{field}.run"), report);
        }
        if let Some(value) = entry.get("accepts_args") {
            if !value.is_bool() {
                report.error(format!("{field}.accepts_args"), "must be a boolean");
            }
        }
    }
}

fn check_toolchains(manifest: &Table, report: &mut Report) {
    for (i, entry) in array_of_tables(manifest, "toolchains", report) {
        let field = format!("toolchains[{i}]");
        unknown_keys(entry, TOOLCHAIN_KEYS, &field, report);
        required_str(entry, "name", &field, report);
        for key in ["description", "home"] {
            opt_str(entry, key, &format!("{field}.{key}"), report);
        }
        for key in ["install", "doctor"] {
            let key_field = format!("{field}.{key}");
            if let Some(cmd) = opt_str(entry, key, &key_field, report) {
                check_placeholders(cmd, &key_field, report);
            }
        }
        if let Some(env) = opt_table(entry, "env", &format!("{field}.env"), report) {
            check_env(env, &format!("{field}.env"), report);
        }
    }
}

fn check_skills(root: &Path, manifest: &Table, report: &mut Report) {
    let mut seen = BTreeSet::new();
    for (i, entry) in array_of_tables(manifest, "skills", report) {
        let field = format!("skills[{i}]");
        unknown_keys(entry, SKILL_KEYS, &field, report);
        if let Some(name) = required_str(entry, "name", &field, report) {
            if !seen.insert(name.to_string()) {
                report.error(format!("{field}.name"), format!("duplicate skill `{name}`"));
            }
        }
        if let Some(agents) = opt_str_array(entry, "agents", &format!("{field}.agents"), report) {
            for agent in agents {
                if !KNOWN_AGENTS.contains(&agent) {
                    report.warn(
                        format!("{field}.agents"),
                        format!(
                            "unknown agent `{agent}`; expected one of {}",
                            KNOWN_AGENTS.join(", ")
                        ),
                    );
                }
            }
        }
        let Some(path) = plugin_file(root, entry, &field, report) else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            report.error(format!("{field}.path"), "skill file is not readable UTF-8");
            continue;
        };
        let frontmatter = parse_frontmatter(&contents);
        for key in ["name", "description"] {
            if !frontmatter.as_ref().is_some_and(|fm| fm.contains_key(key)) {
                report.warn(
                    format!("{field}.path"),
                    format!("SKILL.md frontmatter has no `{key}`"),
                );
            }
        }
    }
}

fn check_prompts(root: &Path, manifest: &Table, report: &mut Report) {
    for (i, entry) in array_of_tables(manifest, "prompts", report) {
        let field = format!("prompts[{i}]");
        unknown_keys(entry, PROMPT_KEYS, &field, report);
        required_str(entry, "name", &field, report);
        plugin_file(root, entry, &field, report);
    }
}

fn check_detect(manifest: &Table, report: &mut Report) {
    for (i, entry) in array_of_tables(manifest, "detect", report) {
        let field = format!("detect[{i}]");
        unknown_keys(entry, DETECT_KEYS, &field, report);
        required_str(entry, "name", &field, report);
        let any_field = format!("{field}.any");
        match opt_str_array(entry, "any", &any_field, report) {
            None if !entry.contains_key("any") => {
                report.error(any_field, "missing `any` patterns; this rule never matches")
            }
            None => {}
            Some(patterns) if patterns.is_empty() => {
                report.error(any_field, "`any` is empty; this rule never matches")
            }
            Some(patterns) => {
                for pattern in patterns {
                    check_detect_pattern(pattern, &any_field, report);
                }
            }
        }
    }
}

fn check_detect_pattern(pattern: &str, field: &str, report: &mut Report) {
    if pattern.contains('/') {
        report.warn(
            field,
            format!("`{pattern}` never matches: patterns match file names, not paths"),
        );
        return;
    }
    let inner = pattern
        .strip_prefix('*')
        .or_else(|| pattern.strip_suffix('*'))
        .unwrap_or(pattern);
    if pattern != "*" && (inner.contains(['*', '?', '[']) || pattern.contains(['?', '['])) {
        report.warn(
            field,
            format!(
                "`{pattern}` only matches literally: detection supports a single \
                 leading or trailing `*`"
            ),
        );
    }
    let literal_less = pattern.chars().all(|c| matches!(c, '*' | '?' | '.'));
    if BROAD_DETECT_PATTERNS.contains(&pattern) || literal_less {
        report.warn(
            field,
            format!("`{pattern}` is overly broad and will match most repositories"),
        );
    }
}

fn check_mcp(manifest: &Table, report: &mut Report) {
    for (i, entry) in array_of_tables(manifest, "mcp", report) {
        let field = format!("mcp[{i}]");
        unknown_keys(entry, MCP_KEYS, &field, report);
        required_str(entry, "name", &field, report);
        if let Some(cmd) = required_str(entry, "command", &field, report) {
            check_placeholders(cmd, &format!("{field}.command"), report);
        }
        let args_field = format!("{field}.args");
        for arg in opt_str_array(entry, "args", &args_field, report).unwrap_or_default() {
            check_placeholders(arg, &args_field, report);
        }
        if let Some(env) = opt_table(entry, "env", &format!("{field}.env"), report) {
            check_env(env, &format!("{field}.env"), report);
        }
    }
}

fn check_env(env: &Table, field: &str, report: &mut Report) {
    for (key, value) in env {
        let key_field = format!("{field}.{key}");
        match value.as_str() {
            Some(value) => check_placeholders(value, &key_field, report),
            None => report.error(key_field, "must be a string"),
        }
    }
}

/// Every `{name}` placeholder in a command string. `${VAR}` shell expansions
/// and braces that don't wrap a lowercase identifier are left alone.
fn placeholders(command: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = command;
    let mut offset = 0;
    while let Some(open) = rest.find('{') {
        let start = offset + open;
        let after = &command[start + 1..];
        let Some(close) = after.find('}') else { break };
        let inner = &after[..close];
        let is_ident = inner
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            && inner
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        let shell_var = start > 0 && command.as_bytes()[start - 1] == b'$';
        if is_ident && !shell_var {
            found.push(inner);
        }
        offset = start + 1;
        rest = &command[offset..];
    }
    found
}

fn check_placeholders(command: &str, field: &str, report: &mut Report) {
    for name in placeholders(command) {
        if KNOWN_PLACEHOLDERS.contains(&name) {
            continue;
        }
        if RESERVED_PLACEHOLDERS.contains(&name) {
            report.warn(
                field,
                format!(
                    "`{{{name}}}` is not expanded by this launcher yet and is passed literally"
                ),
            );
        } else {
            report.error(
                field,
                format!(
                    "unknown placeholder `{{{name}}}`; known: {}",
                    KNOWN_PLACEHOLDERS
                        .iter()
                        .map(|p| format!("{{{p}}}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            );
        }
    }
}

/// Resolve and check an entry's `path` as a file inside the plugin directory.
fn plugin_file(root: &Path, entry: &Table, field: &str, report: &mut Report) -> Option<PathBuf> {
    let rel = required_str(entry, "path", field, report)?;
    let path_field = format!("{field}.path");
    let rel_path = Path::new(rel);
    if rel_path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        report.error(
            path_field,
            format!("`{rel}` must be a relative path inside the plugin"),
        );
        return None;
    }
    let path = root.join(rel_path);
    if !path.is_file() {
        report.error(path_field, format!("file `{rel}` does not exist"));
        return None;
    }
    Some(path)
}

/// `key: value` pairs from a leading `---` YAML frontmatter block, or `None`
/// when the file has none. Values are trimmed and unquoted; nested YAML is not
/// interpreted.
pub(crate) fn parse_frontmatter(contents: &str) -> Option<BTreeMap<String, String>> {
    let mut lines = contents.trim_start_matches('\u{feff}').lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut map = BTreeMap::new();
    for line in lines {
        if line.trim() == "---" {
            return Some(map);
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            if !value.is_empty() {
                map.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    None
}

/// `[[detect]]` rules as the detection module consumes them. Malformed entries
/// are skipped; `validate_dir` reports them.
pub(crate) fn detect_rules(manifest: &Table) -> Vec<DetectRule> {
    let Some(entries) = manifest.get("detect").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(Value::as_table)
        .filter_map(|entry| {
            Some(DetectRule {
                name: entry.get("name")?.as_str()?.to_string(),
                any: entry
                    .get("any")
                    .and_then(Value::as_array)
                    .map(|any| {
                        any.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn unknown_keys(table: &Table, known: &[&str], prefix: &str, report: &mut Report) {
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            let field = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            report.warn(field, format!("unknown key `{key}`"));
        }
    }
}

fn opt_str<'a>(table: &'a Table, key: &str, field: &str, report: &mut Report) -> Option<&'a str> {
    match table.get(key)? {
        Value::String(value) => Some(value),
        _ => {
            report.error(field, "must be a string");
            None
        }
    }
}

fn required_str<'a>(
    table: &'a Table,
    key: &str,
    prefix: &str,
    report: &mut Report,
) -> Option<&'a str> {
    let field = format!("{prefix}.{key}");
    if !table.contains_key(key) {
        report.error(field, format!("missing `{key}`"));
        return None;
    }
    opt_str(table, key, &field, report)
}

fn opt_table<'a>(
    table: &'a Table,
    key: &str,
    field: &str,
    report: &mut Report,
) -> Option<&'a Table> {
    match table.get(key)? {
        Value::Table(value) => Some(value),
        _ => {
            report.error(field, "must be a table");
            None
        }
    }
}

fn opt_str_array<'a>(
    table: &'a Table,
    key: &str,
    field: &str,
    report: &mut Report,
) -> Option<Vec<&'a str>> {
    let values = match table.get(key)? {
        Value::Array(values) => values,
        _ => {
            report.error(field, "must be an array of strings");
            return None;
        }
    };
    let strings: Vec<&str> = values.iter().filter_map(Value::as_str).collect();
    if strings.len() != values.len() {
        report.error(field, "must be an array of strings");
        return None;
    }
    Some(strings)
}

/// `[[key]]` entries with their indices; reports a non-array or non-table.
fn array_of_tables<'a>(
    manifest: &'a Table,
    key: &str,
    report: &mut Report,
) -> Vec<(usize, &'a Table)> {
    let Some(value) = manifest.get(key) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        report.error(key, format!("must be an array of tables (`[[{key}]]`)"));
        return Vec::new();
    };
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| match entry.as_table() {
            Some(table) => Some((i, table)),
            None => {
                report.error(format!("{key}[{i}]"), "must be a table");
                None
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    pub(crate) const GOOD_MANIFEST: &str = r#"
schema_version = 1
name = "demo"
display_name = "Demo"
description = "A demo plugin."
homepage = "https://example.com/demo"
license = "MIT"

[compat]
agent_portal = ">=2.15.0"
platforms = ["linux", "macos"]

[install]
setup = "bin/demo setup --prefix {toolchain_root}"
doctor = "bin/demo doctor --json"

[surface]
kind = "http"
default_title = "Demo"
default_width_percent = 40
health_path = "/healthz"
start = "bin/demo serve --port {port} --session {session_id} --cwd {cwd}"
stop = "bin/demo stop"
ready_url = "http://127.0.0.1:{port}/"

[surface.env]
DEMO_MODE = "portal"

[[commands]]
name = "check"
description = "Run checks."
run = "bin/demo check --cwd {cwd} --json"

[[commands]]
name = "run"
description = "Run an arbitrary demo subcommand."
run = "bin/demo"
accepts_args = true

[[skills]]
name = "workflow"
path = "skills/workflow/SKILL.md"
agents = ["claude", "codex"]

[[prompts]]
name = "bringup"
path = "prompts/bringup.md"

[[detect]]
name = "demo"
any = ["*.demo", "demo.lock"]

[capabilities]
domain = "testing"
"#;

    pub(crate) const SKILL: &str =
        "---\nname: workflow\ndescription: Do the demo workflow.\n---\n\n# Workflow\n";

    /// A plugin directory named `demo` containing `manifest` plus the skill
    /// and prompt files `GOOD_MANIFEST` references.
    pub(crate) fn plugin_dir(manifest: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("demo");
        fs::create_dir_all(root.join("skills/workflow")).unwrap();
        fs::create_dir_all(root.join("prompts")).unwrap();
        fs::write(root.join(MANIFEST), manifest).unwrap();
        fs::write(root.join("skills/workflow/SKILL.md"), SKILL).unwrap();
        fs::write(root.join("prompts/bringup.md"), "Bring the board up.\n").unwrap();
        (tmp, root)
    }

    fn validate_manifest(manifest: &str) -> Report {
        let (_tmp, root) = plugin_dir(manifest);
        validate_dir(&root)
    }

    fn has(issues: &[Issue], field: &str, needle: &str) -> bool {
        issues
            .iter()
            .any(|i| i.field == field && i.message.contains(needle))
    }

    #[test]
    fn good_manifest_passes_clean() {
        let report = validate_manifest(GOOD_MANIFEST);
        assert!(report.ok, "{report:?}");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn missing_and_unparsable_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let report = validate_dir(tmp.path());
        assert!(!report.ok);
        assert!(has(&report.errors, MANIFEST, "failed to read"));

        let report = validate_manifest("name = ");
        assert!(has(&report.errors, MANIFEST, "failed to parse"));
    }

    #[test]
    fn bad_and_missing_name() {
        let report =
            validate_manifest(&GOOD_MANIFEST.replace("name = \"demo\"", "name = \"Demo_X\""));
        assert!(has(&report.errors, "name", "invalid name"));

        let report = validate_manifest(&GOOD_MANIFEST.replace("name = \"demo\"\n", ""));
        assert!(has(&report.errors, "name", "missing"));
    }

    #[test]
    fn missing_skill_and_prompt_files() {
        let (_tmp, root) = plugin_dir(GOOD_MANIFEST);
        fs::remove_file(root.join("skills/workflow/SKILL.md")).unwrap();
        fs::remove_file(root.join("prompts/bringup.md")).unwrap();
        let report = validate_dir(&root);
        assert!(has(&report.errors, "skills[0].path", "does not exist"));
        assert!(has(&report.errors, "prompts[0].path", "does not exist"));
    }

    #[test]
    fn path_escaping_plugin_is_rejected() {
        let report =
            validate_manifest(&GOOD_MANIFEST.replace("prompts/bringup.md", "../outside.md"));
        assert!(has(&report.errors, "prompts[0].path", "inside the plugin"));
    }

    #[test]
    fn skill_without_frontmatter_warns() {
        let (_tmp, root) = plugin_dir(GOOD_MANIFEST);
        fs::write(root.join("skills/workflow/SKILL.md"), "# Workflow\n").unwrap();
        let report = validate_dir(&root);
        assert!(report.ok);
        assert!(has(&report.warnings, "skills[0].path", "no `name`"));
        assert!(has(&report.warnings, "skills[0].path", "no `description`"));
    }

    #[test]
    fn duplicate_names() {
        let manifest = format!(
            "{GOOD_MANIFEST}\n[[commands]]\nname = \"check\"\nrun = \"x\"\n\n\
             [[skills]]\nname = \"workflow\"\npath = \"skills/workflow/SKILL.md\"\n"
        );
        let report = validate_manifest(&manifest);
        assert!(has(&report.errors, "commands[2].name", "duplicate command"));
        assert!(has(&report.errors, "skills[1].name", "duplicate skill"));
    }

    #[test]
    fn empty_run_and_missing_surface_start() {
        let manifest = GOOD_MANIFEST
            .replace("run = \"bin/demo\"", "run = \"  \"")
            .replace(
                "start = \"bin/demo serve --port {port} --session {session_id} --cwd {cwd}\"\n",
                "",
            );
        let report = validate_manifest(&manifest);
        assert!(has(&report.errors, "commands[1].run", "empty"));
        assert!(has(&report.errors, "surface.start", "needs a `start`"));
    }

    #[test]
    fn width_out_of_range() {
        for width in ["5", "95"] {
            let report = validate_manifest(&GOOD_MANIFEST.replace(
                "default_width_percent = 40",
                &format!("default_width_percent = {width}"),
            ));
            assert!(has(
                &report.errors,
                "surface.default_width_percent",
                "outside"
            ));
        }
    }

    #[test]
    fn empty_detect_any() {
        let report = validate_manifest(
            &GOOD_MANIFEST.replace("any = [\"*.demo\", \"demo.lock\"]", "any = []"),
        );
        assert!(has(&report.errors, "detect[0].any", "empty"));
    }

    #[test]
    fn unknown_placeholder_is_error_shell_vars_are_not() {
        let report = validate_manifest(&GOOD_MANIFEST.replace(
            "bin/demo check --cwd {cwd} --json",
            "bin/demo check --cwd {cwd} --out {outdir} --home ${HOME} --x ${foo} {artifact_dir}",
        ));
        assert!(has(&report.errors, "commands[0].run", "`{outdir}`"));
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(has(&report.warnings, "commands[0].run", "`{artifact_dir}`"));
    }

    #[test]
    fn unknown_keys_and_missing_metadata_warn() {
        let manifest = GOOD_MANIFEST
            .replace("homepage = \"https://example.com/demo\"\n", "")
            .replace("license = \"MIT\"\n", "")
            .replace("doctor = \"bin/demo doctor --json\"\n", "")
            .replace("health_path = \"/healthz\"\n", "")
            .replace("[capabilities]", "colour = \"red\"\n\n[capabilities]")
            .replace(
                "[compat]\nagent_portal = \">=2.15.0\"\nplatforms = [\"linux\", \"macos\"]\n",
                "",
            );
        let report = validate_manifest(&manifest);
        assert!(report.ok, "{:?}", report.errors);
        for field in [
            "homepage",
            "license",
            "compat",
            "install.doctor",
            "surface.health_path",
            "detect[0].colour",
        ] {
            assert!(
                report.warnings.iter().any(|w| w.field == field),
                "missing warning for {field}: {:?}",
                report.warnings
            );
        }
    }

    #[test]
    fn unknown_agent_warns() {
        let report = validate_manifest(
            &GOOD_MANIFEST.replace("agents = [\"claude\", \"codex\"]", "agents = [\"gpt\"]"),
        );
        assert!(has(
            &report.warnings,
            "skills[0].agents",
            "unknown agent `gpt`"
        ));
    }

    #[test]
    fn broad_detect_patterns_warn() {
        for pattern in ["*", "*.md", "*.json", "Makefile", "README*", "*.*", "**"] {
            let report = validate_manifest(&GOOD_MANIFEST.replace(
                "any = [\"*.demo\", \"demo.lock\"]",
                &format!("any = [\"{pattern}\"]"),
            ));
            assert!(
                has(&report.warnings, "detect[0].any", "overly broad"),
                "{pattern}: {:?}",
                report.warnings
            );
        }
        let report = validate_manifest(
            &GOOD_MANIFEST.replace("any = [\"*.demo\", \"demo.lock\"]", "any = [\"main/*.c\"]"),
        );
        assert!(has(&report.warnings, "detect[0].any", "never matches"));
    }

    #[test]
    fn placeholder_scanner() {
        assert_eq!(
            placeholders("a {cwd} ${HOME} ${x} {Bad} {} awk '{print $1}' {port}"),
            vec!["cwd", "port"]
        );
    }

    #[test]
    fn frontmatter_parsing() {
        let fm = parse_frontmatter(SKILL).unwrap();
        assert_eq!(fm["name"], "workflow");
        assert_eq!(fm["description"], "Do the demo workflow.");
        assert!(parse_frontmatter("# no frontmatter").is_none());
        assert!(parse_frontmatter("---\nname: x\n").is_none());
    }
}
