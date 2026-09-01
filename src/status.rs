//! The status file: the only channel between the engine and the addon.
//!
//! The addon spawns the engine detached and polls the status file it
//! pointed the engine at. Writes are atomic (temp file + rename) so a
//! reader never sees torn JSON, and a dead engine is detectable by the
//! addon as "stage is non-terminal but the recorded pid is gone".

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Status schema version. Bump on incompatible changes; readers reject
/// files from a newer engine with a clear error.
pub const STATUS_VERSION: u32 = 1;

/// Minimum time between throttled status writes.
const MIN_WRITE_INTERVAL: Duration = Duration::from_millis(250);

/// Coarse lifecycle of a job, from the addon's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Process is starting up; no job state yet.
    Starting,
    /// Segments are coming off the NNTP server.
    Downloading,
    /// PAR2 verification is running (sub-phase of post-processing).
    Verifying,
    /// Archives are being unpacked (sub-phase of post-processing).
    Extracting,
    /// Terminal success. `playable_path`/`final_dir` are set.
    Done,
    /// Terminal failure. `error` says why.
    Failed,
    /// Terminal: user cancelled; the job can be resumed later.
    Cancelled,
}

impl Stage {
    /// Terminal stages leave a stable file for the addon to read later.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Stage::Done | Stage::Failed | Stage::Cancelled)
    }
}

/// Live state of one engine run, mirrored to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub version: u32,
    pub pid: u32,
    #[serde(default)]
    pub job_id: i64,
    pub title: String,
    pub stage: Stage,
    #[serde(default)]
    pub segments_done: u32,
    #[serde(default)]
    pub segments_total: u32,
    #[serde(default)]
    pub bytes_done: u64,
    #[serde(default)]
    pub bytes_total: u64,
    #[serde(default)]
    pub percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_percent: Option<f64>,
    #[serde(default)]
    pub speed_bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playable_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
}

impl Status {
    /// A fresh non-terminal status for a starting engine.
    #[must_use]
    pub fn new(stage: Stage, title: impl Into<String>) -> Self {
        Self {
            version: STATUS_VERSION,
            pid: process::id(),
            job_id: 0,
            title: title.into(),
            stage,
            segments_done: 0,
            segments_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            percent: 0.0,
            verify_percent: None,
            speed_bps: 0,
            playable_path: None,
            final_dir: None,
            error: None,
            updated_at: unix_now(),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Write a status file, replacing anything already there.
///
/// Exposed to tests and the CLI's `status` command.
pub fn read_status(path: &Path) -> Result<Status> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading status file {}", path.display()))?;
    let status: Status = serde_json::from_str(&raw)
        .with_context(|| format!("parsing status file {}", path.display()))?;
    if status.version > STATUS_VERSION {
        bail!(
            "status file {} was written by a newer engine (version {} > {})",
            path.display(),
            status.version,
            STATUS_VERSION
        );
    }
    Ok(status)
}

/// Shared, cheaply cloneable handle for writing the status file.
///
/// Updates are throttled, but stage changes and terminal states are
/// always written through immediately.
#[derive(Debug, Clone)]
pub struct StatusHandle {
    inner: Arc<HandleInner>,
}

#[derive(Debug)]
struct HandleInner {
    path: PathBuf,
    state: Mutex<WriteState>,
}

#[derive(Debug)]
struct WriteState {
    status: Status,
    last_write: Option<Instant>,
    last_written_stage: Stage,
}

impl StatusHandle {
    /// Create the status file with `initial`, overwriting any previous
    /// content, and write it out immediately.
    pub fn create(path: impl Into<PathBuf>, initial: Status) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let handle = Self {
            inner: Arc::new(HandleInner {
                path: path.clone(),
                state: Mutex::new(WriteState {
                    status: initial,
                    last_write: None,
                    // Force the first write regardless of stage.
                    last_written_stage: Stage::Failed,
                }),
            }),
        };
        handle.flush()?;
        Ok(handle)
    }

    /// Apply a mutation, then write if the throttle allows it (stage
    /// changes and terminal states always write).
    ///
    /// Write errors are logged, not propagated: the download is more
    /// important than the progress bar. Use [`StatusHandle::flush`] when
    /// a write must succeed.
    pub fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut guard = self.lock();
        f(&mut guard.status);
        let stage_changed = guard.status.stage != guard.last_written_stage;
        let terminal = guard.status.stage.is_terminal();
        let due = guard
            .last_write
            .is_none_or(|t| t.elapsed() >= MIN_WRITE_INTERVAL);
        if stage_changed || terminal || due {
            if let Err(e) = self.write_locked(&mut guard) {
                tracing::warn!("status write: {e:#}");
            }
        }
    }

    /// Write the status out now, regardless of the throttle.
    pub fn flush(&self) -> Result<()> {
        let mut guard = self.lock();
        self.write_locked(&mut guard)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WriteState> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Caller holds the lock; performs the atomic write.
    fn write_locked(&self, guard: &mut std::sync::MutexGuard<'_, WriteState>) -> Result<()> {
        guard.status.updated_at = unix_now();
        write_atomic(&self.inner.path, &guard.status)?;
        guard.last_write = Some(Instant::now());
        guard.last_written_stage = guard.status.stage;
        Ok(())
    }
}

/// Serialize to a sibling temp file, then rename over the target.
fn write_atomic(path: &Path, status: &Status) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(status)?;
    let file_name = path.file_name().map_or_else(
        || "status.json".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let tmp = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.tmp"));
    fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("status.json");
        (dir, path)
    }

    #[test]
    fn roundtrip_preserves_fields() {
        let (_dir, path) = tmp_dir();
        let handle = StatusHandle::create(&path, Status::new(Stage::Downloading, "Some Release"))
            .expect("create");
        handle.update(|s| {
            s.job_id = 42;
            s.segments_done = 10;
            s.segments_total = 25;
            s.percent = 40.0;
            s.speed_bps = 1_000_000;
            s.final_dir = Some(PathBuf::from("/downloads/Some Release"));
        });
        handle.flush().expect("flush");

        let read = read_status(&path).expect("read");
        assert_eq!(read.version, STATUS_VERSION);
        assert_eq!(read.job_id, 42);
        assert_eq!(read.stage, Stage::Downloading);
        assert_eq!(read.title, "Some Release");
        assert_eq!(read.segments_done, 10);
        assert_eq!(read.segments_total, 25);
        assert_eq!(read.speed_bps, 1_000_000);
        assert_eq!(
            read.final_dir,
            Some(PathBuf::from("/downloads/Some Release"))
        );
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp() {
        let (_dir, path) = tmp_dir();
        let handle =
            StatusHandle::create(&path, Status::new(Stage::Starting, "t")).expect("create");
        handle.flush().expect("flush");
        assert!(path.is_file());
        let parent = path.parent().expect("parent");
        let strays: Vec<_> = std::fs::read_dir(parent)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray temp files: {strays:?}");
    }

    #[test]
    fn terminal_stage_writes_through_throttle() {
        let (_dir, path) = tmp_dir();
        let handle =
            StatusHandle::create(&path, Status::new(Stage::Starting, "t")).expect("create");
        // Immediately (within the throttle window) mark terminal.
        handle.update(|s| s.stage = Stage::Done);
        let read = read_status(&path).expect("read");
        assert_eq!(read.stage, Stage::Done);
    }

    #[test]
    fn newer_version_is_rejected() {
        let (dir, path) = tmp_dir();
        let mut status = Status::new(Stage::Starting, "t");
        status.version = STATUS_VERSION + 1;
        std::fs::write(&path, serde_json::to_string_pretty(&status).expect("ser")).expect("write");
        let err = read_status(&path).expect_err("must reject newer version");
        assert!(err.to_string().contains("newer engine"), "got: {err:#}");
        drop(dir);
    }

    #[test]
    fn stage_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Stage::Extracting).expect("ser"),
            "\"extracting\""
        );
    }

    #[test]
    fn terminal_stages() {
        assert!(!Stage::Downloading.is_terminal());
        assert!(Stage::Done.is_terminal());
        assert!(Stage::Failed.is_terminal());
        assert!(Stage::Cancelled.is_terminal());
    }

    #[test]
    fn create_makes_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a/b/c/status.json");
        StatusHandle::create(&nested, Status::new(Stage::Starting, "t")).expect("create");
        assert!(nested.is_file());
    }

    #[test]
    fn read_missing_file_is_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = read_status(&dir.path().join("nope.json")).expect_err("missing");
        assert!(
            err.to_string().contains("reading status file"),
            "got: {err:#}"
        );
    }

    #[test]
    fn status_handle_is_debug() {
        let (_dir, path) = tmp_dir();
        let handle = StatusHandle::create(&path, Status::new(Stage::Starting, "t")).expect("c");
        let _ = format!("{handle:?}");
    }
}
