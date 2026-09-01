//! Choosing the file the addon should hand to Kodi's player.
//!
//! Heuristic: the largest file with a known video extension. This
//! matches reality on Usenet — the main feature dominates the release,
//! and samples/proof files are small.

use std::fs;
use std::path::{Path, PathBuf};

use turbonzb_core::postprocess::PostProcessReport;

/// File extensions we consider playable video.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "3gp", "avi", "flv", "iso", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ogm", "ts",
    "vob", "webm", "wmv",
];

/// Pick the file to play from a finished job's post-process report.
///
/// Prefers files extracted from an archive (largest video file among
/// them); falls back to scanning the final directory (single-file
/// releases that were never archived).
#[must_use]
pub fn pick_playable(report: &PostProcessReport) -> Option<PathBuf> {
    if let Some(unpack) = &report.unpack {
        let candidates: Vec<PathBuf> = unpack
            .extracted_files
            .iter()
            .map(|name| report.final_dir.join(name))
            .collect();
        if let Some(path) = largest_video_file(&candidates) {
            return Some(path);
        }
    }
    pick_playable_in_dir(&report.final_dir)
}

/// Largest video file directly inside `dir` (not recursive).
#[must_use]
pub fn pick_playable_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .collect();
    largest_video_file(&paths)
}

fn largest_video_file(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .filter(|p| is_video_file(p))
        .filter_map(|p| fs::metadata(p).ok().map(|m| (p, m.len())))
        .max_by_key(|(_, len)| *len)
        .map(|(p, _)| p.clone())
}

#[must_use]
pub fn is_video_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    VIDEO_EXTENSIONS.contains(&ext.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, bytes).expect("write file");
    }

    #[test]
    fn picks_largest_video_in_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("sample.mkv"), b"tiny");
        write(&dir.path().join("feature.mkv"), &[0u8; 10_000]);
        write(&dir.path().join("proof.jpg"), b"x");
        let picked = pick_playable_in_dir(dir.path()).expect("a playable");
        assert!(picked.ends_with("feature.mkv"), "got {picked:?}");
    }

    #[test]
    fn extension_case_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("BIG.MKV"), &[0u8; 100]);
        let picked = pick_playable_in_dir(dir.path()).expect("a playable");
        assert!(picked.ends_with("BIG.MKV"), "got {picked:?}");
    }

    #[test]
    fn ignores_directories_and_non_video_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("movie.nfo"), &[0u8; 1_000]);
        write(&dir.path().join("subfolder"), b"");
        assert!(pick_playable_in_dir(dir.path()).is_none());
    }

    #[test]
    fn missing_directory_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert!(pick_playable_in_dir(&missing).is_none());
    }

    #[test]
    fn video_extension_whitelist() {
        assert!(is_video_file(Path::new("a/b/movie.mkv")));
        assert!(is_video_file(Path::new("movie.TS")));
        assert!(!is_video_file(Path::new("movie.rar")));
        assert!(!is_video_file(Path::new("movie")));
        assert!(!is_video_file(Path::new(".mkv")));
    }
}
