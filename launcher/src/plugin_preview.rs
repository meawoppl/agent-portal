//! `agent-portal plugin preview <path>`: render what Portal will show for a
//! plugin directory — its card, skills, prompts, commands, dock metadata,
//! context cost, and (with `--cwd`) whether it would be suggested there —
//! without installing or running it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use toml::{Table, Value};

use crate::plugin_detect::detect_reason;
use crate::plugin_validate::{
    detect_rules, parse_frontmatter, read_manifest, validate_dir, Report,
};

#[derive(Debug, Serialize)]
struct Preview {
    name: Option<String>,
    display_name: String,
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    source: String,
    skills: Vec<ContextFile>,
    prompts: Vec<ContextFile>,
    commands: Vec<CommandPreview>,
    surface: Option<SurfacePreview>,
    context_bytes: u64,
    context_tokens: u64,
    detection: Option<Detection>,
    validation: Report,
}

#[derive(Debug, Serialize)]
struct ContextFile {
    name: String,
    path: String,
    description: Option<String>,
    agents: Vec<String>,
    bytes: Option<u64>,
    tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CommandPreview {
    name: String,
    description: Option<String>,
    accepts_args: bool,
}

#[derive(Debug, Serialize)]
struct SurfacePreview {
    title: String,
    width_percent: Option<i64>,
    health_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct Detection {
    cwd: String,
    matched: bool,
    reason: String,
}

/// CLI entry point.
pub fn preview(path: &Path, cwd: Option<&Path>, json: bool) -> Result<()> {
    let preview = build_preview(path, cwd)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        print_preview(&preview);
    }
    Ok(())
}

fn build_preview(root: &Path, cwd: Option<&Path>) -> Result<Preview> {
    let manifest = read_manifest(root)?;
    let source = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string();
    let name = str_field(&manifest, "name");

    let skills: Vec<ContextFile> = tables(&manifest, "skills")
        .map(|entry| context_file(root, entry, true))
        .collect();
    let prompts: Vec<ContextFile> = tables(&manifest, "prompts")
        .map(|entry| context_file(root, entry, false))
        .collect();
    let context_bytes: u64 = skills
        .iter()
        .chain(&prompts)
        .filter_map(|file| file.bytes)
        .sum();

    let commands = tables(&manifest, "commands")
        .map(|entry| CommandPreview {
            name: str_field(entry, "name").unwrap_or_default(),
            description: str_field(entry, "description"),
            accepts_args: entry
                .get("accepts_args")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect();

    let display_name = str_field(&manifest, "display_name")
        .or_else(|| name.clone())
        .unwrap_or_else(|| "(unnamed plugin)".to_string());
    let surface = manifest
        .get("surface")
        .and_then(Value::as_table)
        .map(|surface| SurfacePreview {
            title: str_field(surface, "default_title").unwrap_or_else(|| display_name.clone()),
            width_percent: surface
                .get("default_width_percent")
                .and_then(Value::as_integer),
            health_path: str_field(surface, "health_path"),
        });

    let detection = match cwd {
        Some(cwd) => Some(detection(&manifest, cwd)?),
        None => None,
    };

    Ok(Preview {
        name,
        display_name,
        description: str_field(&manifest, "description"),
        homepage: str_field(&manifest, "homepage"),
        license: str_field(&manifest, "license"),
        source,
        skills,
        prompts,
        commands,
        surface,
        context_bytes,
        context_tokens: estimate_tokens(context_bytes),
        detection,
        validation: validate_dir(root),
    })
}

fn detection(manifest: &Table, cwd: &Path) -> Result<Detection> {
    let cwd: PathBuf = cwd
        .canonicalize()
        .with_context(|| format!("cannot read --cwd {}", cwd.display()))?;
    let rules = detect_rules(manifest);
    let (matched, reason) = if rules.is_empty() {
        (
            true,
            "no detect rules: offered in every project".to_string(),
        )
    } else {
        match detect_reason(&cwd, &rules) {
            Some(reason) => (true, reason),
            None => (
                false,
                format!(
                    "no detect rule matched ({})",
                    rules
                        .iter()
                        .map(|rule| format!("{}: {}", rule.name, rule.any.join(", ")))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        }
    };
    Ok(Detection {
        cwd: cwd.display().to_string(),
        matched,
        reason,
    })
}

fn context_file(root: &Path, entry: &Table, is_skill: bool) -> ContextFile {
    let rel = str_field(entry, "path").unwrap_or_default();
    let contents = (!rel.is_empty())
        .then(|| std::fs::read_to_string(root.join(&rel)).ok())
        .flatten();
    let bytes = contents.as_ref().map(|c| c.len() as u64);
    let description = if is_skill {
        contents.as_deref().and_then(skill_description)
    } else {
        None
    };
    ContextFile {
        name: str_field(entry, "name").unwrap_or_default(),
        path: rel,
        description,
        agents: entry
            .get("agents")
            .and_then(Value::as_array)
            .map(|agents| {
                agents
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        bytes,
        tokens: bytes.map(estimate_tokens),
    }
}

/// The frontmatter `description`, or else the first non-empty body line with
/// any heading markers stripped.
fn skill_description(contents: &str) -> Option<String> {
    if let Some(description) =
        parse_frontmatter(contents).and_then(|fm| fm.get("description").cloned())
    {
        return Some(description);
    }
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "---")
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .filter(|line| !line.is_empty())
}

fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

fn print_preview(p: &Preview) {
    println!("{}", p.display_name);
    if let Some(name) = &p.name {
        println!("  name:        {name}");
    }
    println!(
        "  description: {}",
        p.description.as_deref().unwrap_or("(none)")
    );
    println!(
        "  homepage:    {}",
        p.homepage.as_deref().unwrap_or("(none)")
    );
    println!(
        "  license:     {}",
        p.license.as_deref().unwrap_or("(none)")
    );
    println!("  source:      {}", p.source);

    println!();
    print_context_files("Skills", &p.skills);
    print_context_files("Prompts", &p.prompts);

    if p.commands.is_empty() {
        println!("Commands: none");
    } else {
        println!("Commands:");
        for command in &p.commands {
            println!(
                "  {}{} - {}",
                command.name,
                if command.accepts_args {
                    " [args...]"
                } else {
                    ""
                },
                command.description.as_deref().unwrap_or("(no description)")
            );
        }
    }

    match &p.surface {
        Some(surface) => {
            println!("Surface (dock):");
            println!("  title:       {}", surface.title);
            println!(
                "  width:       {}",
                surface
                    .width_percent
                    .map(|w| format!("{w}%"))
                    .unwrap_or_else(|| "default".to_string())
            );
            println!(
                "  health path: {}",
                surface.health_path.as_deref().unwrap_or("(none; uses /)")
            );
        }
        None => println!("Surface: none"),
    }

    println!();
    println!(
        "Context estimate: {} bytes, ~{} tokens (skills + prompts)",
        p.context_bytes, p.context_tokens
    );

    if let Some(detection) = &p.detection {
        println!(
            "Detection in {}: {} - {}",
            detection.cwd,
            if detection.matched {
                "suggested"
            } else {
                "not suggested"
            },
            detection.reason
        );
    }

    println!();
    println!("Validation:");
    p.validation.print();
}

fn print_context_files(label: &str, files: &[ContextFile]) {
    if files.is_empty() {
        println!("{label}: none");
        return;
    }
    println!("{label}:");
    for file in files {
        let size = match (file.bytes, file.tokens) {
            (Some(bytes), Some(tokens)) => format!("{bytes} bytes, ~{tokens} tokens"),
            _ => "missing file".to_string(),
        };
        let agents = if file.agents.is_empty() {
            String::new()
        } else {
            format!(" [{}]", file.agents.join(", "))
        };
        println!("  {} ({size}){agents}", file.name);
        if let Some(description) = &file.description {
            println!("    {description}");
        }
    }
}

fn str_field(table: &Table, key: &str) -> Option<String> {
    table.get(key).and_then(Value::as_str).map(str::to_string)
}

fn tables<'a>(manifest: &'a Table, key: &str) -> impl Iterator<Item = &'a Table> {
    manifest
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_validate::tests::{plugin_dir, GOOD_MANIFEST, SKILL};

    #[test]
    fn preview_reports_card_sizes_and_commands() {
        let (_tmp, root) = plugin_dir(GOOD_MANIFEST);
        let p = build_preview(&root, None).unwrap();
        assert_eq!(p.display_name, "Demo");
        assert_eq!(p.skills.len(), 1);
        assert_eq!(
            p.skills[0].description.as_deref(),
            Some("Do the demo workflow.")
        );
        assert_eq!(p.skills[0].bytes, Some(SKILL.len() as u64));
        assert_eq!(p.skills[0].tokens, Some((SKILL.len() as u64).div_ceil(4)));
        let prompt_len = "Bring the board up.\n".len() as u64;
        assert_eq!(p.context_bytes, SKILL.len() as u64 + prompt_len);
        assert_eq!(p.context_tokens, p.context_bytes.div_ceil(4));
        assert!(!p.commands[0].accepts_args);
        assert!(p.commands[1].accepts_args);
        let surface = p.surface.unwrap();
        assert_eq!(surface.width_percent, Some(40));
        assert_eq!(surface.health_path.as_deref(), Some("/healthz"));
        assert!(p.validation.ok);
        assert!(p.detection.is_none());
    }

    #[test]
    fn preview_detection_against_cwd() {
        let (_tmp, root) = plugin_dir(GOOD_MANIFEST);
        let project = tempfile::tempdir().unwrap();
        let p = build_preview(&root, Some(project.path())).unwrap();
        let detection = p.detection.unwrap();
        assert!(!detection.matched);
        assert!(detection.reason.contains("no detect rule matched"));

        std::fs::create_dir_all(project.path().join("sub")).unwrap();
        std::fs::write(project.path().join("sub/x.demo"), "").unwrap();
        let detection = build_preview(&root, Some(project.path()))
            .unwrap()
            .detection
            .unwrap();
        assert!(detection.matched);
        assert_eq!(detection.reason, "Detected demo files (*.demo, demo.lock)");
    }

    #[test]
    fn preview_without_detect_rules_is_offered_everywhere() {
        let manifest = GOOD_MANIFEST.replace(
            "[[detect]]\nname = \"demo\"\nany = [\"*.demo\", \"demo.lock\"]\n",
            "",
        );
        let (_tmp, root) = plugin_dir(&manifest);
        let project = tempfile::tempdir().unwrap();
        let detection = build_preview(&root, Some(project.path()))
            .unwrap()
            .detection
            .unwrap();
        assert!(detection.matched);
        assert_eq!(
            detection.reason,
            "no detect rules: offered in every project"
        );
    }
}
