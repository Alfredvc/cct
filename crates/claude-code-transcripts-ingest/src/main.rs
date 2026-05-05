use clap::Parser;

use claude_code_transcripts_ingest::cli::{Cli, Command};
use claude_code_transcripts_ingest::{extract, info, report, run, serve, update, version_check};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    version_check::maybe_spawn_check(&cli.command);
    // Banner is strictly cache-only: the spawn above hasn't finished by
    // the time we get here, so this print sees the PREVIOUS run's fetch.
    // Today's fetch lands in the cache for tomorrow's invocation.
    version_check::maybe_print_banner(&cli.command);
    match cli.command {
        Command::Ingest(args) => run::run(args),
        Command::Serve(args) => serve::run(args).await,
        Command::Info(args) => info::run(args),
        Command::Update(args) => update::run(args),
        Command::Report(args) => report::run(args),
        Command::Extract(args) => extract::run(args),
    }
}
