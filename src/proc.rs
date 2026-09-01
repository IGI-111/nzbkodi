//! Process-level helpers: the single-flight lock, signal sending, and
//! the file logger.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use nix::fcntl::{Flock, FlockArg};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Advisory lock ensuring one engine process runs at a time.
///
/// The lock is held for the lifetime of the guard (the process), so a
/// crashed engine never leaves a stale lock behind — the kernel drops
/// the flock when the fd closes on exit.
#[derive(Debug)]
pub struct EngineLock {
    /// Held for the process lifetime; unlocks on drop.
    _lock: Flock<File>,
}

impl EngineLock {
    /// Acquire `path` exclusively, non-blocking. Fails if another engine
    /// process is already running.
    pub fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        let mut lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, e)| {
            anyhow::anyhow!("another nzbkodi-engine process is already running ({e})")
        })?;
        // Record the holder for diagnostics (best-effort).
        let _ = lock.write_all(format!("{}\n", std::process::id()).as_bytes());
        Ok(Self { _lock: lock })
    }
}

/// Whether a process with this pid exists (signal 0 probe).
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// Send SIGTERM to a process.
pub fn send_sigterm(pid: u32) -> Result<()> {
    kill(Pid::from_raw(pid as i32), Some(Signal::SIGTERM))
        .with_context(|| format!("signalling pid {pid}"))
}

/// Log to `<data_dir>/engine.log`, honouring `RUST_LOG` (default: info).
///
/// The engine runs detached, so stderr is usually lost — the file is
/// the debug channel.
pub fn init_tracing(data_dir: &Path) -> Result<()> {
    static INIT: OnceLock<()> = OnceLock::new();
    if INIT.set(()).is_err() {
        return Ok(()); // Already initialised this process.
    }

    let path: PathBuf = data_dir.join("engine.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening log file {}", path.display()))?;
    // Unbuffered on purpose: a hung or killed engine must not swallow its
    // last log lines in a lost buffer (cost us a diagnosis once).
    let writer = SharedLogWriter(Arc::new(Mutex::new(file)));

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .init();
    // The engine runs detached with stderr discarded; route panics into
    // the log so failures are diagnosable.
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "engine panic");
    }));
    Ok(())
}

/// A `MakeWriter` that tees into one shared buffered file.
#[derive(Clone, Debug)]
struct SharedLogWriter(Arc<Mutex<File>>);

impl std::io::Write for SharedLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedLogWriter {
    type Writer = SharedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("engine.lock");
        let _first = EngineLock::acquire(&lock_path).expect("first lock");
        let second = EngineLock::acquire(&lock_path);
        assert!(second.is_err(), "second lock must fail");
    }

    #[test]
    fn lock_is_released_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("engine.lock");
        {
            let _held = EngineLock::acquire(&lock_path).expect("lock");
        }
        let _again = EngineLock::acquire(&lock_path).expect("re-lock after drop");
    }

    #[test]
    fn current_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }
}
