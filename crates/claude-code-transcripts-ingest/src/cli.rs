use std::path::PathBuf;

use clap::{Parser, Subcommand};

fn default_input_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".claude").join("projects")
}

fn default_output_db() -> PathBuf {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local").join("share")
        });
    data_home.join("cct").join("transcripts.duckdb")
}

#[derive(Parser, Debug)]
#[command(
    name = "cct",
    version,
    about = "Claude Code transcript tools — ingest JSONL transcripts into DuckDB and serve the viewer UI"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Ingest Claude Code JSONL transcripts into a DuckDB database
    Ingest(IngestArgs),
    /// Serve the transcript viewer web UI
    Serve(ServeArgs),
    /// Show DB path, size, and entry counts
    Info(InfoArgs),
    /// Update cct to the latest GitHub release (or a specific version)
    Update(UpdateArgs),
    /// Render a human-readable report from the DB
    Report(ReportArgs),
    /// Dump structured JSON for downstream tooling
    Extract(ExtractArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct IngestArgs {
    /// Input directory to scan recursively for .jsonl files.
    #[arg(short = 'i', long = "input-dir", default_value_os_t = default_input_dir())]
    pub input_dir: PathBuf,

    /// Worker thread count. 0 = number of logical CPUs.
    #[arg(short = 'j', long = "jobs", default_value_t = 0)]
    pub jobs: usize,

    /// Output DuckDB filename. Overwritten on every run.
    #[arg(short = 'o', long = "output", default_value_os_t = default_output_db())]
    pub output: PathBuf,

    /// TOML file overriding/extending the seeded model_pricing table.
    #[arg(long = "pricing")]
    pub pricing: Option<PathBuf>,

    /// Disable per-second progress reporting on stderr.
    #[arg(long = "no-progress")]
    pub no_progress: bool,
}

#[derive(Parser, Debug)]
pub struct ServeArgs {
    /// DuckDB database file to serve.
    #[arg(long = "db", default_value_os_t = default_output_db())]
    pub db: PathBuf,

    /// Port to listen on.
    #[arg(long = "port", default_value_t = 8766)]
    pub port: u16,

    /// Window (days) for the cost-decomposition flamegraph. Computed at startup
    /// and refreshed when the DB mtime changes.
    #[arg(long = "decomp-days", default_value_t = 30)]
    pub decomp_days: i32,
}

#[derive(Parser, Debug)]
pub struct InfoArgs {
    /// DuckDB database file to inspect.
    #[arg(long = "db", default_value_os_t = default_output_db())]
    pub db: PathBuf,
}

#[derive(Parser, Debug)]
pub struct UpdateArgs {
    /// Specific release version to install (e.g. 0.2.0 or v0.2.0). Defaults to latest.
    #[arg(long = "version")]
    pub version: Option<String>,

    /// Skip the interactive confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

#[derive(Parser, Debug)]
pub struct ReportArgs {
    #[command(subcommand)]
    pub kind: ReportKind,
}

#[derive(Subcommand, Debug)]
pub enum ReportKind {
    /// API token usage and cost breakdown by model
    Usage(UsageArgs),
}

#[derive(Parser, Debug)]
pub struct UsageArgs {
    /// DuckDB database file to query.
    #[arg(long = "db", default_value_os_t = default_output_db())]
    pub db: PathBuf,

    /// Project directory to filter on. Defaults to the current working directory.
    #[arg(long = "project")]
    pub project: Option<PathBuf>,

    /// Scan all projects (overrides --project and --no-subdirs).
    #[arg(long = "all", conflicts_with_all = ["project", "no_subdirs"])]
    pub all: bool,

    /// Match only the project's exact cwd (skip worktrees / subdirectory cwds).
    #[arg(long = "no-subdirs")]
    pub no_subdirs: bool,

    /// Include events on or after this date (YYYY-MM-DD, UTC).
    #[arg(long = "from")]
    pub from: Option<String>,

    /// Include events on or before this date (YYYY-MM-DD, UTC, inclusive).
    #[arg(long = "to")]
    pub to: Option<String>,

    /// Emit machine-readable JSON instead of formatted text.
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Parser, Debug)]
pub struct ExtractArgs {
    #[command(subcommand)]
    pub kind: ExtractKind,
}

#[derive(Subcommand, Debug)]
pub enum ExtractKind {
    /// Per-turn session metadata (tools, skills, errors, tokens, subagents)
    Sessions(ExtractSessionsArgs),
}

#[derive(Parser, Debug)]
pub struct ExtractSessionsArgs {
    /// DuckDB database file to query.
    #[arg(long = "db", default_value_os_t = default_output_db())]
    pub db: PathBuf,

    /// Project directory to filter on. Defaults to the current working directory.
    #[arg(long = "project")]
    pub project: Option<PathBuf>,

    /// Scan all projects (overrides --project and --no-subdirs).
    #[arg(long = "all", conflicts_with_all = ["project", "no_subdirs"])]
    pub all: bool,

    /// Match only the project's exact cwd (skip worktrees / subdirectory cwds).
    #[arg(long = "no-subdirs")]
    pub no_subdirs: bool,

    /// Filter to a specific session id (or unique prefix).
    #[arg(long = "session")]
    pub session: Option<String>,

    /// Include sessions starting on or after this date (YYYY-MM-DD, UTC).
    #[arg(long = "from")]
    pub from: Option<String>,

    /// Include sessions starting on or before this date (YYYY-MM-DD, UTC, inclusive).
    #[arg(long = "to")]
    pub to: Option<String>,
}
