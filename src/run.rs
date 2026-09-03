//! Command implementations: the orchestration between the Kodi addon and
//! the TurboNZB engine.
//!
//! Lifecycle of `start`: acquire the single-flight lock, create a queue
//! job from the NZB, download it (cancellable), post-process it, and
//! keep the status file current throughout. `resume` re-runs an existing
//! job (article-level resume). `cancel` SIGTERMs the engine process
//! tracked by a status file; the download stops gracefully and can be
//! resumed later.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::fs;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use turbonzb_core::engine::{Engine, ProgressEvent};
use turbonzb_core::nzb;
use turbonzb_core::postprocess::{
    PostProcessConfig, PostProcessStatus, post_process_with_progress,
};
use turbonzb_core::queue::{JobState, QueueJob, QueueManager};
use turbonzb_index::{NewznabClient, SearchAggregator, SearchQuery, SearchResult};

use crate::config::EngineConfig;
use crate::playable::pick_playable;
use crate::progress::{SpeedWindow, percent};
use crate::status::{Stage, Status, StatusHandle, read_status};

/// Period of in-loop status refreshes while downloading.
const STATUS_TICK: Duration = Duration::from_millis(500);

/// Rolling window for the speed estimate.
const SPEED_WINDOW_MS: u64 = 5_000;

/// Cap on fetched NZB documents (they are small; larger is abuse).
const NZB_SIZE_CAP: usize = 32 * 1024 * 1024;

/// No completed segment for this long = the download is dead, not slow.
const STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// After a stall cancel, the engine must wind down within this window;
/// past it the task is aborted so the addon is never held hostage.
const GRACE_TIMEOUT: Duration = Duration::from_secs(180);

/// Where the NZB document comes from.
#[derive(Debug, Clone)]
pub enum NzbSource {
    /// A local `.nzb` file.
    Path(PathBuf),
    /// An HTTP(S) URL — typically a Newznab `getnzb` link with API key.
    Url(String),
}

impl NzbSource {
    /// A rough title guess for the initial status file (refined later
    /// from NZB metadata or `--name`).
    fn guess_title(&self) -> String {
        match self {
            Self::Path(p) => p.file_stem().map_or_else(
                || "download".to_string(),
                |s| s.to_string_lossy().into_owned(),
            ),
            Self::Url(u) => u
                .rsplit('/')
                .next()
                .and_then(|s| s.strip_suffix(".nzb"))
                .unwrap_or("download")
                .to_string(),
        }
    }
}

/// How a phase (or the whole run) ended.
#[derive(Debug)]
enum Outcome {
    /// Success; the status file already says `Done`.
    Completed,
    /// Failure with a message; job state already reflects it.
    Failed(String),
    /// User cancelled; job left resumable, status says `Cancelled`.
    Cancelled,
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

pub async fn cmd_start(
    config_path: PathBuf,
    nzb: Option<PathBuf>,
    nzb_url: Option<String>,
    status_path: PathBuf,
    name: Option<String>,
) -> Result<ExitCode> {
    let source = match (nzb, nzb_url) {
        (Some(path), None) => NzbSource::Path(path),
        (None, Some(url)) => NzbSource::Url(url),
        (None, None) => bail!("specify exactly one of --nzb or --nzb-url"),
        (Some(_), Some(_)) => bail!("--nzb and --nzb-url are mutually exclusive"),
    };

    // The addon gets a pollable file even if everything below fails.
    let initial_title = name.clone().unwrap_or_else(|| source.guess_title());
    let status = StatusHandle::create(&status_path, Status::new(Stage::Starting, initial_title))?;

    let cfg = match EngineConfig::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => return Ok(fail_status(&status, format!("loading config: {e:#}"))),
    };
    if let Err(e) = prepare_dirs(&cfg).await {
        return Ok(fail_status(&status, format!("{e:#}")));
    }
    let _lock = match crate::proc::EngineLock::acquire(&cfg.data_dir.join("engine.lock")) {
        Ok(lock) => lock,
        Err(e) => return Ok(fail_status(&status, format!("{e:#}"))),
    };
    crate::proc::init_tracing(&cfg.data_dir)?;
    tracing::info!(nzb = ?source, "starting job");

    let queue = match QueueManager::open(cfg.data_dir.join("queue.db")).await {
        Ok(queue) => Arc::new(queue),
        Err(e) => return Ok(fail_status(&status, format!("opening queue: {e:#}"))),
    };
    if let Err(e) = queue.recover_interrupted().await {
        return Ok(fail_status(&status, format!("queue recovery: {e:#}")));
    }

    let nzb_bytes = match fetch_nzb(&source).await {
        Ok(bytes) => bytes,
        Err(e) => return Ok(fail_status(&status, format!("{e:#}"))),
    };
    tracing::info!(bytes = nzb_bytes.len(), source = ?source, "nzb fetched");
    let nzb_doc = match nzb::parse(&nzb_bytes) {
        Ok(doc) => doc,
        Err(e) => return Ok(fail_status(&status, format!("parsing NZB: {e}"))),
    };
    if nzb_doc.files.is_empty() || nzb_doc.files.iter().all(|f| f.segment_count == 0) {
        return Ok(fail_status(
            &status,
            "the NZB contains no downloadable files — removed or stubbed by the indexer?"
                .to_string(),
        ));
    }
    let parsed_segments: u32 = nzb_doc.files.iter().map(|f| f.segment_count).sum();
    tracing::info!(
        files = nzb_doc.files.len(),
        total_segments = parsed_segments,
        "nzb parsed"
    );

    let title = name.unwrap_or_else(|| {
        nzb_doc
            .title()
            .map(str::to_string)
            .unwrap_or_else(|| "nzbkodi-download".to_string())
    });
    let release_dir = cfg.download_dir.join(sanitize_release_name(&title));
    if let Err(e) = fs::create_dir_all(&release_dir).await {
        return Ok(fail_status(&status, format!("{e:#}")));
    }

    let job_id = match queue.add_job(&nzb_doc, &release_dir, 0, Some(&title)).await {
        Ok(id) => id,
        Err(e) => return Ok(fail_status(&status, format!("adding job: {e}"))),
    };
    // Encrypted-archive password from NZB metadata, if present.
    if let Some(password) = nzb_doc.passwords().first() {
        if let Err(e) = queue.set_job_archive_password(job_id, Some(password)).await {
            tracing::warn!("storing archive password: {e}");
        }
    }

    status.update(|s| {
        s.job_id = job_id;
        s.title = title;
        s.final_dir = Some(release_dir);
        s.stage = Stage::Downloading;
    });
    tracing::info!(job_id, "job queued");

    run_download_and_post(&cfg, &queue, job_id, &status).await
}

// ---------------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------------

pub async fn cmd_resume(
    config_path: PathBuf,
    job_id: i64,
    status_path: PathBuf,
) -> Result<ExitCode> {
    let status = StatusHandle::create(
        &status_path,
        Status::new(Stage::Starting, format!("job {job_id}")),
    )?;

    let cfg = match EngineConfig::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => return Ok(fail_status(&status, format!("loading config: {e:#}"))),
    };
    if let Err(e) = prepare_dirs(&cfg).await {
        return Ok(fail_status(&status, format!("{e:#}")));
    }
    let _lock = match crate::proc::EngineLock::acquire(&cfg.data_dir.join("engine.lock")) {
        Ok(lock) => lock,
        Err(e) => return Ok(fail_status(&status, format!("{e:#}"))),
    };
    crate::proc::init_tracing(&cfg.data_dir)?;
    tracing::info!(job_id, "resuming job");

    let queue = match QueueManager::open(cfg.data_dir.join("queue.db")).await {
        Ok(queue) => Arc::new(queue),
        Err(e) => return Ok(fail_status(&status, format!("opening queue: {e:#}"))),
    };
    if let Err(e) = queue.recover_interrupted().await {
        return Ok(fail_status(&status, format!("queue recovery: {e:#}")));
    }
    let job: QueueJob = match queue.get_job(job_id).await {
        Ok(job) => job,
        Err(e) => return Ok(fail_status(&status, format!("loading job {job_id}: {e}"))),
    };
    if let Err(e) = queue.reset_failed_segments(job_id).await {
        return Ok(fail_status(
            &status,
            format!("resetting failed segments: {e}"),
        ));
    }

    status.update(|s| {
        s.job_id = job.id;
        s.title = job.name.clone();
        s.final_dir = Some(job.output_dir.clone());
        s.stage = Stage::Downloading;
    });
    run_download_and_post(&cfg, &queue, job_id, &status).await
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// What kind of search to run — see [`build_search_query`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKind {
    /// Free-text query.
    Text(String),
    /// Movie by IMDB id (e.g. `tt0058935`).
    MovieImdb(String),
    /// TV by title + season + episode.
    Tv {
        query: String,
        season: u32,
        episode: u32,
    },
}

/// Build a [`SearchQuery`] from CLI arguments, validating combinations.
pub fn build_search_query(
    kind: SearchKind,
    limit: u32,
    max_age_days: Option<u32>,
) -> Result<SearchQuery> {
    if limit == 0 || limit > 500 {
        bail!("--limit must be between 1 and 500, got {limit}");
    }
    if let Some(days) = max_age_days {
        if days == 0 {
            bail!("--max-age-days must be positive");
        }
    }
    let mut query = match kind {
        SearchKind::Text(q) => SearchQuery::text(q),
        SearchKind::MovieImdb(imdb) => SearchQuery::movie(imdb),
        SearchKind::Tv {
            query,
            season,
            episode,
        } => SearchQuery::tv(query, season, episode),
    };
    query.limit = limit;
    query.max_age_days = max_age_days;
    Ok(query)
}

/// One search hit, serialized to the addon as JSON.
#[derive(Debug, serde::Serialize)]
struct SearchHit {
    title: String,
    nzb_url: String,
    size: u64,
    post_date: u64,
    age_days: u64,
    category: u32,
    category_name: String,
    grabs: u32,
    files: u32,
    password: String,
    season: Option<u32>,
    episode: Option<u32>,
    /// Indexers that returned this result.
    indexers: Vec<String>,
}

fn search_hit(result: &SearchResult, sources: Vec<String>, now_unix: u64) -> SearchHit {
    SearchHit {
        title: result.title.clone(),
        nzb_url: result.nzb_url.clone(),
        size: result.size,
        post_date: result.post_date,
        age_days: now_unix.saturating_sub(result.post_date) / 86_400,
        category: result.category,
        category_name: result.category_name.clone(),
        grabs: result.grabs,
        files: result.files,
        password: format_password(result.password),
        season: result.tv.as_ref().and_then(|t| t.season),
        episode: result.tv.as_ref().and_then(|t| t.episode),
        indexers: sources,
    }
}

fn format_password(status: turbonzb_index::PasswordStatus) -> String {
    use turbonzb_index::PasswordStatus as P;
    match status {
        P::None => "none",
        P::Rar => "rar",
        P::InnerArchive => "inner-archive",
        P::Unknown => "unknown",
    }
    .to_string()
}

pub async fn cmd_search(
    config_path: PathBuf,
    kind: SearchKind,
    limit: u32,
    max_age_days: Option<u32>,
) -> Result<ExitCode> {
    let cfg = EngineConfig::load(&config_path)?;
    if cfg.indexers.is_empty() {
        bail!("no indexers configured — add some in the addon settings");
    }
    // One-shot command: log to stderr so the addon can surface per-indexer
    // errors (bad URL, auth, timeouts) instead of a bare empty result.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let query = build_search_query(kind, limit, max_age_days)?;

    let mut aggregator = SearchAggregator::new(30);
    for indexer in &cfg.indexers {
        aggregator.add_provider(Box::new(NewznabClient::new(indexer.clone())));
    }

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let hits: Vec<SearchHit> = aggregator
        .search(&query)
        .await
        .into_iter()
        .map(|aggregated| search_hit(&aggregated.result, aggregated.sources, now_unix))
        .collect();

    println!("{}", serde_json::to_string_pretty(&hits)?);
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// cancel / status / jobs
// ---------------------------------------------------------------------------

pub async fn cmd_cancel(status_path: PathBuf) -> Result<ExitCode> {
    let status = read_status(&status_path)?;
    if status.stage.is_terminal() {
        println!("job already in terminal state: {:?}", status.stage);
        return Ok(ExitCode::SUCCESS);
    }
    if status.pid == std::process::id() {
        bail!("refusing to signal ourselves");
    }
    if !crate::proc::pid_alive(status.pid) {
        println!(
            "engine pid {} is not running — stale status file, nothing to cancel",
            status.pid
        );
        return Ok(ExitCode::SUCCESS);
    }

    crate::proc::send_sigterm(status.pid)?;
    println!("sent SIGTERM to pid {}", status.pid);

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        match read_status(&status_path) {
            Ok(s) if s.stage.is_terminal() => {
                println!("engine stopped: {:?}", s.stage);
                return Ok(ExitCode::SUCCESS);
            }
            _ => {}
        }
        if !crate::proc::pid_alive(status.pid) {
            println!("engine exited");
            return Ok(ExitCode::SUCCESS);
        }
        if Instant::now() >= deadline {
            eprintln!(
                "engine still running after 15s — run cancel again or kill {} manually",
                status.pid
            );
            return Ok(ExitCode::FAILURE);
        }
    }
}

pub async fn cmd_status(status_path: PathBuf) -> Result<ExitCode> {
    let status = read_status(&status_path)?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(ExitCode::SUCCESS)
}

pub async fn cmd_jobs(config_path: PathBuf) -> Result<ExitCode> {
    let cfg = EngineConfig::load(&config_path)?;
    let queue = QueueManager::open(cfg.data_dir.join("queue.db")).await?;
    let jobs = queue.list_jobs().await?;
    if jobs.is_empty() {
        println!("queue is empty");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<6} {:<12} {:>12} {:>12}  NAME",
        "ID", "STATE", "SEGS", "BYTES"
    );
    for job in jobs {
        println!(
            "{:<6} {:<12} {:>6}/{} {:>12}  {}",
            job.id,
            job.state.as_str(),
            job.segments_done,
            job.total_segments,
            job.downloaded_bytes,
            job.name
        );
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// download + post-process pipeline
// ---------------------------------------------------------------------------

/// Shared driver for `start` and `resume`: download the job, then
/// post-process it, writing terminal status and mapping to an exit code.
async fn run_download_and_post(
    cfg: &EngineConfig,
    queue: &Arc<QueueManager>,
    job_id: i64,
    status: &StatusHandle,
) -> Result<ExitCode> {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (cancel_tx, cancel_rx) = watch::channel(false);
    tokio::spawn(install_signal_handlers(cancel_flag.clone(), cancel_tx));

    let outcome = download_phase(cfg, queue, job_id, status, cancel_flag, cancel_rx).await?;
    let outcome = match outcome {
        Outcome::Completed => postprocess_phase(queue, job_id, status).await?,
        Outcome::Failed(message) => {
            tracing::error!(job_id, "job failed: {message}");
            Outcome::Failed(message)
        }
        Outcome::Cancelled => outcome,
    };

    Ok(match outcome {
        Outcome::Completed => ExitCode::SUCCESS,
        Outcome::Failed(_) => ExitCode::FAILURE,
        Outcome::Cancelled => ExitCode::SUCCESS,
    })
}

/// Drive the download to completion (or cancellation), updating status.
async fn download_phase(
    cfg: &EngineConfig,
    queue: &Arc<QueueManager>,
    job_id: i64,
    status: &StatusHandle,
    cancel_flag: Arc<AtomicBool>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<Outcome> {
    let server = turbonzb_core::nntp::ServerConfig::from(&cfg.nntp);
    let engine = Arc::new(Engine::new(vec![server], cfg.nntp.connections as usize));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let runner = {
        let engine = Arc::clone(&engine);
        let queue = Arc::clone(queue);
        let cancel = Arc::clone(&cancel_flag);
        tokio::spawn(async move { engine.run_job_cancellable(queue, job_id, tx, cancel).await })
    };

    let mut speed = SpeedWindow::new(SPEED_WINDOW_MS);
    let started = Instant::now();
    let (mut bytes_done, mut segments_done, total_bytes, total_segments) =
        match queue.get_job(job_id).await {
            Ok(job) => (
                job.downloaded_bytes,
                job.segments_done,
                job.total_bytes,
                job.total_segments,
            ),
            Err(e) => {
                tracing::warn!("reading job stats: {e}");
                (0, 0, 0, 0)
            }
        };
    let mut finished: Option<(usize, usize)> = None;
    let mut ticker = tokio::time::interval(STATUS_TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut last_progress = Instant::now();
    let mut stalled = false;
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(ProgressEvent::SegmentDone { bytes, .. }) => {
                    speed.record_at(elapsed_ms(started), bytes);
                    // The queue's aggregate columns only refresh on
                    // cancel/finalize, so track progress from events.
                    bytes_done = bytes_done.saturating_add(bytes);
                    segments_done += 1;
                    last_progress = Instant::now();
                }
                Some(ProgressEvent::ArticleError { filename, segment, error }) => {
                    tracing::warn!(filename, segment, "article error: {error}");
                }
                Some(ProgressEvent::JobFinished { completed, failed }) => {
                    finished = Some((completed, failed));
                    break;
                }
                Some(_) => {}
                None => break, // Engine returned; done or cancelled.
            },
            _ = ticker.tick() => {
                refresh_job_stats(
                    status, bytes_done, segments_done, total_bytes, total_segments,
                    &mut speed, started,
                );
                // Watchdog: reads inside the engine carry their own
                // timeouts, but anything that still wedges past this
                // window is dead, not slow — cancel with a reason.
                if last_progress.elapsed() > STALL_TIMEOUT {
                    stalled = true;
                    tracing::error!(
                        since_progress_s = last_progress.elapsed().as_secs(),
                        "download stalled — cancelling job"
                    );
                    cancel_flag.store(true, Ordering::SeqCst);
                    break;
                }
            }
            _ = cancel_rx.changed() => {
                // Signal arrived; the engine observes the shared flag and
                // will return shortly. Keep updating until it does.
            }
        }
    }
    rx.close();

    // One last refresh so the file reflects the final download state.
    refresh_job_stats(
        status,
        bytes_done,
        segments_done,
        total_bytes,
        total_segments,
        &mut speed,
        started,
    );

    // Flatten `Result<Result<(), CoreError>, JoinError>` into a message,
    // bounded by a grace window so a wedged engine cannot hold us forever.
    let mut runner = runner;
    let run_result: Result<(), String> =
        match tokio::time::timeout(GRACE_TIMEOUT, &mut runner).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e.to_string()),
            Ok(Err(e)) => Err(format!("join: {e}")),
            Err(_) => {
                runner.abort();
                Err("engine ignored cancel for 3 minutes — task aborted".to_string())
            }
        };
    let download_ok = matches!(finished, Some((completed, failed)) if failed == 0 && completed > 0);

    if stalled {
        return Ok(Outcome::Failed(
            "download stalled — no segment completed for 10 minutes (dead connections?); \
the job stays resumable from Downloads"
                .to_string(),
        ));
    }

    if cancel_flag.load(Ordering::SeqCst) && !download_ok {
        // Leave the job queued so `resume` can pick it up at the article level.
        if let Err(e) = queue.set_job_state(job_id, JobState::Queued).await {
            tracing::warn!("marking job resumable: {e}");
        }
        tracing::info!("cancelled by signal");
        return Ok(Outcome::Cancelled);
    }

    match (&finished, run_result) {
        (Some((_, failed)), Ok(())) if *failed > 0 => {
            let message = queue
                .get_job(job_id)
                .await
                .ok()
                .and_then(|job| job.error)
                .unwrap_or_else(|| format!("{failed} segment(s) failed"));
            Ok(Outcome::Failed(message))
        }
        (Some((completed, failed)), Ok(())) if *completed == 0 && *failed == 0 => Ok(Outcome::Failed(
            "release contains no downloadable files — the NZB is empty (removed or stubbed by the indexer?)".to_string(),
        )),
        (Some(_), Ok(())) => Ok(Outcome::Completed),
        (_, Err(e)) => Ok(Outcome::Failed(format!("engine: {e}"))),
        (None, Ok(())) => {
            // Engine returned without a JobFinished event. Check the queue:
            // a resume of an already-complete job lands here.
            let job = queue.get_job(job_id).await?;
            if job.state == JobState::Complete
                && job.total_segments > 0
                && job.segments_done >= job.total_segments
            {
                Ok(Outcome::Completed)
            } else {
                Ok(Outcome::Failed(
                    job.error
                        .unwrap_or_else(|| "download interrupted".to_string()),
                ))
            }
        }
    }
}

/// Post-process (PAR2 verify + unpack) a downloaded job, then write the
/// terminal status: `Done` with a playable path, or `Failed`.
async fn postprocess_phase(
    queue: &Arc<QueueManager>,
    job_id: i64,
    status: &StatusHandle,
) -> Result<Outcome> {
    let job: QueueJob = queue.get_job(job_id).await?;
    let dir = job.output_dir.clone();

    status.update(|s| {
        s.stage = Stage::Extracting;
        s.percent = 100.0;
        s.speed_bps = 0;
        s.verify_percent = None;
    });

    // Verify progress comes back on a blocking worker thread.
    let cb_status = status.clone();
    let on_verify: Box<dyn FnMut(u64, u64) + Send> = Box::new(move |done, total| {
        let p = percent(done, total);
        cb_status.update(|s| {
            s.verify_percent = Some(p);
            s.stage = if p >= 100.0 {
                Stage::Extracting
            } else {
                Stage::Verifying
            };
        });
    });

    let pp_config = PostProcessConfig {
        download_dir: dir,
        completed_dir: job.output_dir.clone(),
        category: None,
        cleanup_archives: true,
        archive_password: job.archive_password.clone(),
        skip_verify: false,
    };

    tracing::info!("post-processing");
    match post_process_with_progress(pp_config, Some(on_verify)).await {
        Ok(report) => match report.status {
            PostProcessStatus::Complete
            | PostProcessStatus::UnpackedWithoutVerify
            | PostProcessStatus::NoArchives => {
                let playable = pick_playable(&report);
                status.update(|s| {
                    s.verify_percent = None;
                    s.final_dir = Some(report.final_dir.clone());
                    s.playable_path = playable;
                    s.percent = 100.0;
                    s.stage = Stage::Done;
                });
                status.flush()?;
                tracing::info!(final_dir = %report.final_dir.display(), "job complete");
                Ok(Outcome::Completed)
            }
            PostProcessStatus::Damaged {
                healthy,
                damaged,
                missing,
            } => {
                let message = format!(
                    "release damaged: {damaged} damaged, {missing} missing of {} files — \
                     PAR2 auto-repair could not reconstruct them (insufficient recovery data)",
                    healthy + damaged + missing
                );
                mark_failed(queue, job_id, status, message).await
            }
            PostProcessStatus::UnpackFailed(e) => {
                mark_failed(queue, job_id, status, format!("unpack failed: {e}")).await
            }
        },
        Err(e) => mark_failed(queue, job_id, status, format!("post-processing: {e}")).await,
    }
}

/// Record a failure in both the queue and the status file.
async fn mark_failed(
    queue: &QueueManager,
    job_id: i64,
    status: &StatusHandle,
    message: String,
) -> Result<Outcome> {
    if let Err(e) = queue.set_job_state(job_id, JobState::Failed).await {
        tracing::warn!("marking job failed: {e}");
    }
    if let Err(e) = queue.set_job_error(job_id, Some(&message)).await {
        tracing::warn!("storing job error: {e}");
    }
    status.update(|s| {
        s.stage = Stage::Failed;
        s.error = Some(message.clone());
        s.speed_bps = 0;
        s.verify_percent = None;
    });
    status.flush()?;
    Ok(Outcome::Failed(message))
}

/// First signal: graceful cancel. Second signal: immediate exit.
async fn install_signal_handlers(cancel_flag: Arc<AtomicBool>, cancel_tx: watch::Sender<bool>) {
    let mut term = match signal(SignalKind::terminate()) {
        Ok(sig) => sig,
        Err(e) => {
            tracing::warn!("installing SIGTERM handler: {e}");
            return;
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(sig) => sig,
        Err(e) => {
            tracing::warn!("installing SIGINT handler: {e}");
            return;
        }
    };

    loop {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        if cancel_flag.swap(true, Ordering::SeqCst) {
            tracing::warn!("second signal received — exiting immediately");
            std::process::exit(130);
        }
        tracing::info!("shutdown signal received — cancelling download");
        let _ = cancel_tx.send(true);
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Write a `Failed` status and return the failure exit code.
fn fail_status(status: &StatusHandle, message: String) -> ExitCode {
    eprintln!("error: {message}");
    status.update(|s| {
        s.stage = Stage::Failed;
        s.error = Some(message.clone());
        s.speed_bps = 0;
    });
    if let Err(e) = status.flush() {
        eprintln!("error writing status file: {e:#}");
    }
    ExitCode::FAILURE
}

/// Push live stats into the status file.
fn refresh_job_stats(
    status: &StatusHandle,
    bytes_done: u64,
    segments_done: u32,
    total_bytes: u64,
    total_segments: u32,
    speed: &mut SpeedWindow,
    started: Instant,
) {
    status.update(|s| {
        s.segments_done = segments_done;
        s.segments_total = total_segments;
        s.bytes_done = bytes_done;
        s.bytes_total = total_bytes;
        s.percent = percent(bytes_done, total_bytes);
        s.speed_bps = speed.bps_at(elapsed_ms(started));
    });
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Fetch the NZB document from a path or URL (bounded).
async fn fetch_nzb(source: &NzbSource) -> Result<Vec<u8>> {
    match source {
        NzbSource::Path(path) => fs::read(path)
            .await
            .with_context(|| format!("reading {}", path.display())),
        NzbSource::Url(url) => {
            let client = reqwest::Client::builder()
                // Some indexers (e.g. NZBgeek) reject UA-less requests on
                // `t=get` with error 109 — identify ourselves like any other
                // downloader client.
                .user_agent(concat!("nzbkodi-engine/", env!("CARGO_PKG_VERSION")))
                // Indexer CDNs can be slow from some regions; a multi-MB NZB
                // needs more than the old 30s.
                .timeout(Duration::from_secs(120))
                .build()?;
            let mut response = client
                .get(url.clone())
                .send()
                .await
                .with_context(|| format!("fetching {url}"))?
                .error_for_status()
                .with_context(|| format!("fetching {url}"))?;
            if let Some(len) = response.content_length() {
                if len as usize > NZB_SIZE_CAP {
                    bail!("NZB at {url} is too large ({len} bytes)");
                }
            }
            let mut body = Vec::with_capacity(64 * 1024);
            while let Some(chunk) = response
                .chunk()
                .await
                .with_context(|| format!("reading {url}"))?
            {
                if body.len() + chunk.len() > NZB_SIZE_CAP {
                    bail!("NZB at {url} exceeds {NZB_SIZE_CAP} bytes");
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        }
    }
}

/// Make a release title safe to use as a single directory name.
fn sanitize_release_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || c == '/' || c == '\\' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches('.');
    let truncated: String = trimmed.chars().take(120).collect();
    if truncated.is_empty() {
        "nzbkodi-download".to_string()
    } else {
        truncated
    }
}

async fn prepare_dirs(cfg: &EngineConfig) -> Result<()> {
    fs::create_dir_all(&cfg.download_dir)
        .await
        .with_context(|| format!("creating {}", cfg.download_dir.display()))?;
    fs::create_dir_all(&cfg.data_dir)
        .await
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_separators_and_control_chars() {
        assert_eq!(
            sanitize_release_name("Movie: The/Best \\2024"),
            "Movie: The Best 2024"
        );
        assert_eq!(
            sanitize_release_name("  trailing dots... "),
            "trailing dots"
        );
        assert_eq!(
            sanitize_release_name(".leading.and.trailing."),
            "leading.and.trailing"
        );
        assert_eq!(sanitize_release_name("a\u{0}b"), "a b");
    }

    #[test]
    fn sanitize_has_a_length_cap() {
        let long = "x".repeat(500);
        assert_eq!(sanitize_release_name(&long).chars().count(), 120);
    }

    #[test]
    fn sanitize_empty_becomes_fallback() {
        assert_eq!(sanitize_release_name(""), "nzbkodi-download");
        assert_eq!(sanitize_release_name("   ...  "), "nzbkodi-download");
        assert_eq!(sanitize_release_name("/"), "nzbkodi-download");
    }

    #[test]
    fn guess_title_from_path_and_url() {
        assert_eq!(
            NzbSource::Path(PathBuf::from("/tmp/Some.Release.1080p.nzb")).guess_title(),
            "Some.Release.1080p"
        );
        assert_eq!(
            NzbSource::Url("https://indexer/api?t=getnzb&apikey=xyz".to_string()).guess_title(),
            "download"
        );
        assert_eq!(
            NzbSource::Url("https://indexer/get/some-release.nzb".to_string()).guess_title(),
            "some-release"
        );
    }

    #[test]
    fn search_query_text() {
        let q =
            build_search_query(SearchKind::Text("dune part two".into()), 50, None).expect("query");
        assert_eq!(q.q.as_deref(), Some("dune part two"));
        assert_eq!(q.limit, 50);
        assert_eq!(q.max_age_days, None);
    }

    #[test]
    fn search_query_tv() {
        let q = build_search_query(
            SearchKind::Tv {
                query: "severance".into(),
                season: 2,
                episode: 4,
            },
            100,
            Some(30),
        )
        .expect("query");
        assert_eq!(q.season, Some(2));
        assert_eq!(q.episode, Some(4));
        assert_eq!(q.max_age_days, Some(30));
    }

    #[test]
    fn search_query_movie_imdb() {
        let q = build_search_query(SearchKind::MovieImdb("tt0058935".into()), 100, None)
            .expect("query");
        assert_eq!(q.imdb_id.as_deref(), Some("tt0058935"));
    }

    #[test]
    fn search_query_limit_and_age_bounds() {
        assert!(build_search_query(SearchKind::Text("x".into()), 0, None).is_err());
        assert!(build_search_query(SearchKind::Text("x".into()), 501, None).is_err());
        assert!(build_search_query(SearchKind::Text("x".into()), 100, Some(0)).is_err());
    }

    #[test]
    fn search_hit_serializes_for_the_addon() {
        let result = SearchResult {
            title: "Some.Show.S01E02.1080p".into(),
            guid: "g".into(),
            nzb_url: "https://indexer/get.nzb?apikey=k".into(),
            size: 1_400_000_000,
            post_date: 1_700_000_000,
            category: 5000,
            category_name: "TV > HD".into(),
            grabs: 10,
            files: 30,
            password: turbonzb_index::PasswordStatus::None,
            indexer: "indexer-a".into(),
            tv: Some(turbonzb_index::TvInfo {
                season: Some(1),
                episode: Some(2),
                rage_id: None,
                tvdb_id: None,
                tvmaze_id: None,
                title: None,
                air_date: None,
            }),
            movie: None,
        };
        let hit = search_hit(
            &result,
            vec!["indexer-a".into(), "indexer-b".into()],
            1_700_000_000 + 5 * 86_400,
        );
        let json = serde_json::to_string(&hit).expect("ser");
        assert!(json.contains("\"age_days\":5"), "got: {json}");
        assert!(json.contains("\"season\":1"), "got: {json}");
        assert!(json.contains("\"password\":\"none\""), "got: {json}");
        assert!(json.contains("indexer-b"), "got: {json}");
    }
}
