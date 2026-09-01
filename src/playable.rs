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

/// Largest video file under `dir` (recursive to a bounded depth —
/// archives commonly extract into a release subfolder).
#[must_use]
pub fn pick_playable_in_dir(dir: &Path) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_files(dir, 0, 3, &mut paths);
    largest_video_file(&paths)
}

fn collect_files(dir: &Path, depth: u32, max_depth: u32, out: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, depth + 1, max_depth, out);
        } else {
            out.push(path);
        }
    }
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
    fn finds_video_nested_in_release_subfolder() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Obfuscated archive volumes at the root (not video) ...
        write(&dir.path().join("W9pLZXE.7z.001"), b"junk");
        write(&dir.path().join("W9pLZXE.7z.014"), b"junk");
        // ... and the real content in a release subfolder.
        let sub = dir.path().join("Mr.Robot.S01E01.720p-NTb");
        write(&sub.join("Mr_Robot.740p.mkv"), &vec![0u8; 10_000]);
        write(&sub.join("Mr_Robot.nfo"), b"nfo");
        let picked = pick_playable_in_dir(dir.path()).expect("must find nested video");
        assert!(picked.ends_with("Mr_Robot.740p.mkv"), "got {picked:?}");
    }

    #[test]
    fn nested_video_found_via_report_extracted_files() {
        use turbonzb_core::postprocess::{PostProcessReport, PostProcessStatus};
        use turbonzb_core::unpack::UnpackReport;
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("Release");
        write(&sub.join("video.mkv"), &vec![0u8; 10_000]);
        let report = PostProcessReport {
            verify: None,
            unpack: Some(UnpackReport {
                extracted_files: vec!["Release/video.mkv".to_string()],
                total_bytes: 10_000,
                was_encrypted: false,
            }),
            status: PostProcessStatus::Complete,
            final_dir: dir.path().to_path_buf(),
        };
        let picked = pick_playable(&report).expect("must find it");
        assert!(picked.ends_with("video.mkv"));
    }

    #[test]
    fn finds_video_two_levels_deep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let deep = dir.path().join("a/b/c");
        write(&deep.join("deep.mkv"), &[0u8; 100]);
        assert!(pick_playable_in_dir(dir.path()).is_some());
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
