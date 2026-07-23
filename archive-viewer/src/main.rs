//! `portal-archive` — standalone archive/history viewer (#1288).
//!
//! Reads back the long-term session archive written by the backend's archival
//! sweep (`archive-format` crate) and offers a small CLI over it: `list`,
//! `rollup`, `export`, and `cat`. It links only `archive-format`, never the
//! backend.
//!
//! The whole tool is synchronous, but the S3 backend's store methods block on
//! a captured tokio runtime handle, so the entrypoint is `#[tokio::main]` to
//! guarantee a runtime exists when the store is constructed.

mod cat;
mod export;
mod list;
mod rollup;
mod rows;
mod serve;
mod summarize;
mod table;

#[cfg(test)]
mod fixture_tests;

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use archive_format::{archive_config_from_env, ArchiveBackendConfig, ArchiveConfig, ArchiveStore};
use clap::{Args as ClapArgs, Parser, Subcommand};

use crate::export::Format;
use crate::rollup::GroupBy;
use crate::rows::{collect_rows, filter_and_sort, parse_date_arg, resolve_session, Filters};

#[derive(Parser, Debug)]
#[command(name = "portal-archive")]
#[command(about = "Browse and summarize the long-term session archive")]
#[command(
    after_help = "Target selection: pass --local-root or --s3-bucket, else the \
                  PORTAL_SESSION_ARCHIVE_* environment variables are used.\n  \
                  Source & issues: https://github.com/meawoppl/agent-portal"
)]
struct Args {
    #[command(flatten)]
    target: TargetArgs,

    #[command(subcommand)]
    command: Command,
}

/// Global archive-target selection flags (usable before or after the
/// subcommand).
#[derive(ClapArgs, Debug)]
struct TargetArgs {
    /// Read from a local filesystem archive rooted at this path.
    #[arg(long, global = true, conflicts_with = "s3_bucket")]
    local_root: Option<PathBuf>,

    /// Read from this S3 (or S3-compatible) bucket. Region/credentials/endpoint
    /// come from the standard AWS_* environment variables.
    #[arg(long, global = true)]
    s3_bucket: Option<String>,

    /// Key prefix inside the S3 bucket (requires --s3-bucket).
    #[arg(long, global = true, requires = "s3_bucket")]
    s3_prefix: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List archived sessions (one row each), most-recently-active first.
    List(ListArgs),
    /// Aggregate manifest metrics into a grouped table.
    Rollup(RollupArgs),
    /// Export flattened manifest rows (all fields) as CSV or JSON.
    Export(ExportArgs),
    /// Print a readable transcript digest for one session.
    Cat(CatArgs),
    /// Serve the archive over a loopback-only HTTP API + embedded web viewer.
    ///
    /// SECURITY: no authentication. This is an operator tool over
    /// operator-controlled archive data; it binds to 127.0.0.1 only, by design.
    /// Anyone who can reach the port can read every archived session. Do not
    /// expose it (no port-forward, no reverse proxy, no 0.0.0.0 bind).
    Serve(serve::ServeArgs),
}

#[derive(ClapArgs, Debug)]
struct ListArgs {
    /// Filter by user: an email substring or a session/user UUID prefix.
    #[arg(long)]
    user: Option<String>,
    /// Filter by agent type (e.g. claude, codex).
    #[arg(long)]
    agent: Option<String>,
    /// Only sessions last active on/after this RFC3339 datetime or YYYY-MM-DD.
    #[arg(long)]
    from: Option<String>,
    /// Only sessions last active on/before this RFC3339 datetime or YYYY-MM-DD.
    #[arg(long)]
    to: Option<String>,
    /// Filter by a substring of the session name (case-insensitive).
    #[arg(long)]
    name: Option<String>,
}

#[derive(ClapArgs, Debug)]
struct RollupArgs {
    /// What to group rows by.
    #[arg(long, value_enum, default_value_t = GroupBy::User)]
    group_by: GroupBy,
    /// Only sessions last active on/after this RFC3339 datetime or YYYY-MM-DD.
    #[arg(long)]
    from: Option<String>,
    /// Only sessions last active on/before this RFC3339 datetime or YYYY-MM-DD.
    #[arg(long)]
    to: Option<String>,
}

#[derive(ClapArgs, Debug)]
struct ExportArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Csv)]
    format: Format,
    /// Write to this file instead of stdout.
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
struct CatArgs {
    /// Session id or unique short prefix.
    session: String,
    /// Dump the raw NDJSON transcript instead of the digest.
    #[arg(long)]
    raw: bool,
}

#[tokio::main]
async fn main() {
    // Keep CLI errors to a single clean stderr line (no anyhow backtrace).
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
    let config = resolve_config(&args.target)?;
    let store = ArchiveStore::from_config(&config)
        .map_err(|e| anyhow!("failed to open archive store: {e}"))?;

    match args.command {
        Command::List(a) => run_list(&store, a),
        Command::Rollup(a) => run_rollup(&store, a),
        Command::Export(a) => run_export(&store, a),
        Command::Cat(a) => run_cat(&store, a),
        Command::Serve(a) => serve::run(store, a).await,
    }
}

/// Resolve the archive target from flags, falling back to the environment.
fn resolve_config(target: &TargetArgs) -> Result<ArchiveConfig> {
    if let Some(root) = &target.local_root {
        return Ok(ArchiveConfig {
            backend: ArchiveBackendConfig::Local { root: root.clone() },
            transcripts: true,
            media: true,
        });
    }
    if let Some(bucket) = &target.s3_bucket {
        return Ok(ArchiveConfig {
            backend: ArchiveBackendConfig::S3 {
                bucket: bucket.clone(),
                prefix: target.s3_prefix.clone(),
            },
            transcripts: true,
            media: true,
        });
    }
    match archive_config_from_env().map_err(|e| anyhow!(e))? {
        Some(config) => Ok(config),
        None => Err(anyhow!(
            "no archive target: pass --local-root <path> or --s3-bucket <bucket>, \
             or set the PORTAL_SESSION_ARCHIVE_* environment variables"
        )),
    }
}

fn build_filters(
    user: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<Filters> {
    Ok(Filters {
        user,
        agent,
        name,
        from: from
            .as_deref()
            .map(|s| parse_date_arg(s, false))
            .transpose()?,
        to: to.as_deref().map(|s| parse_date_arg(s, true)).transpose()?,
    })
}

fn run_list(store: &ArchiveStore, a: ListArgs) -> Result<()> {
    let filters = build_filters(a.user, a.agent, a.name, a.from, a.to)?;
    let rows = filter_and_sort(collect_rows(store)?, &filters);
    println!("{}", list::render(&rows));
    Ok(())
}

fn run_rollup(store: &ArchiveStore, a: RollupArgs) -> Result<()> {
    let filters = build_filters(None, None, None, a.from, a.to)?;
    let rows = filter_and_sort(collect_rows(store)?, &filters);
    println!("{}", rollup::render(&rows, a.group_by));
    Ok(())
}

fn run_export(store: &ArchiveStore, a: ExportArgs) -> Result<()> {
    // Export mirrors `list`'s default ordering but applies no filters — it is
    // a full dump of every archived manifest row.
    let rows = filter_and_sort(collect_rows(store)?, &Filters::default());
    let rendered = export::render(&rows, a.format)?;
    match a.output {
        Some(path) => {
            std::fs::write(&path, rendered)
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!("Wrote {} rows to {}", rows.len(), path.display());
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

fn run_cat(store: &ArchiveStore, a: CatArgs) -> Result<()> {
    let rows = collect_rows(store)?;
    let row = resolve_session(&a.session, &rows)?;
    let out = cat::run(store, row.user_id, row.manifest.session_id, a.raw)?;
    println!("{out}");
    Ok(())
}
