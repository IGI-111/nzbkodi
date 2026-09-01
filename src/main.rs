//! nzbkodi-engine: the on-demand Usenet download engine behind the nzbkodi
//! Kodi addon.
//!
//! The addon spawns this binary detached, hands it a config file (NNTP
//! server, download dir, data dir), an NZB (path or indexer URL), and a
//! status file path, then polls the status file until it reaches a
//! terminal stage. The status file is the only channel — no daemon, no
//! RPC, no port. `resume` continues an interrupted job at the article
//! level; `cancel` SIGTERMs the tracked engine process.

#![forbid(unsafe_code)]
#![warn(rust_2018_idioms)]

mod config;
mod playable;
mod proc;
mod progress;
mod run;
mod status;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "nzbkodi-engine",
    version,
    about = "On-demand Usenet download engine for the nzbkodi Kodi addon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add a job and run it to completion (download + PAR2 verify + unpack).
    Start {
        /// Engine config JSON (NNTP server, download dir, data dir).
        #[arg(long)]
        config: PathBuf,
        /// Path to a local .nzb file (exclusive with --nzb-url).
        #[arg(long)]
        nzb: Option<PathBuf>,
        /// URL of an .nzb (exclusive with --nzb).
        #[arg(long)]
        nzb_url: Option<String>,
        /// Status file the addon will poll.
        #[arg(long)]
        status: PathBuf,
        /// Display name override (e.g. the TMDB title).
        #[arg(long)]
        name: Option<String>,
    },
    /// Search all configured indexers and print merged JSON hits.
    Search {
        /// Engine config JSON.
        #[arg(long)]
        config: PathBuf,
        /// Free-text query (exclusive with --imdb).
        #[arg(long)]
        query: Option<String>,
        /// IMDB id for a movie search, e.g. tt0058935 (exclusive with --query).
        #[arg(long)]
        imdb: Option<String>,
        /// Season for a TV search (with --query + --episode).
        #[arg(long, requires = "query", requires = "episode")]
        season: Option<u32>,
        /// Episode for a TV search (with --query + --season).
        #[arg(long, requires = "query", requires = "season")]
        episode: Option<u32>,
        /// Maximum results per indexer.
        #[arg(long, default_value = "100")]
        limit: u32,
        /// Only results newer than this many days.
        #[arg(long)]
        max_age_days: Option<u32>,
    },
    /// Resume (or retry) an existing queue job.
    Resume {
        /// Engine config JSON.
        #[arg(long)]
        config: PathBuf,
        /// Job id (the status file's `job_id`).
        #[arg(long)]
        job: i64,
        /// Status file the addon will poll.
        #[arg(long)]
        status: PathBuf,
    },
    /// Politely stop the engine process tracked by a status file.
    Cancel {
        /// Status file to read the engine pid from.
        #[arg(long)]
        status: PathBuf,
    },
    /// Print a status file's contents.
    Status {
        /// Status file to print.
        #[arg(long)]
        status: PathBuf,
    },
    /// List queue jobs from the engine data dir.
    Jobs {
        /// Engine config JSON.
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Search {
            config,
            query,
            imdb,
            season,
            episode,
            limit,
            max_age_days,
        } => {
            use run::SearchKind;
            let kind = match (query, imdb, season, episode) {
                (Some(q), None, None, None) => SearchKind::Text(q),
                (None, Some(imdb), None, None) => SearchKind::MovieImdb(imdb),
                (Some(q), None, Some(season), Some(episode)) => SearchKind::Tv {
                    query: q,
                    season,
                    episode,
                },
                _ => {
                    eprintln!(
                        "error: pick one of --query, --imdb, or --query + --season + --episode"
                    );
                    return ExitCode::FAILURE;
                }
            };
            run::cmd_search(config, kind, limit, max_age_days).await
        }
        Command::Start {
            config,
            nzb,
            nzb_url,
            status,
            name,
        } => run::cmd_start(config, nzb, nzb_url, status, name).await,
        Command::Resume {
            config,
            job,
            status,
        } => run::cmd_resume(config, job, status).await,
        Command::Cancel { status } => run::cmd_cancel(status).await,
        Command::Status { status } => run::cmd_status(status).await,
        Command::Jobs { config } => run::cmd_jobs(config).await,
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
