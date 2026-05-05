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
