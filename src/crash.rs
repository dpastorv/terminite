//! Crash dump management — last-crash.log pointer, recent crash detection.
//! Shared between main.rs (panic hook) and renderer (crash notice dialog).

use std::path::PathBuf;
use std::time::Duration;

use crate::logging;

/// Also updates `last-crash.log` so the most recent crash is always at a
/// canonical path for the CLI and the next-launch notice.
pub fn write_crash_dump(payload: &str, location: &str, backtrace: &str) {
    let dir = logging::crash_dir();
    let Some(dir) = dir else { return };

    let filename = format!("{}.txt", logging::filename_timestamp_now());
    let path = dir.join(&filename);
    let body = format!(
        "terminite crash dump\nversion: {}\nlocation: {}\nmessage: {}\n\nbacktrace:\n{}\n",
        env!("CARGO_PKG_VERSION"),
        location,
        payload,
        backtrace,
    );
    let _ = std::fs::write(&path, &body);
    // Update the canonical pointer so `terminite last-crash` and the
    // next-launch notice always find the right file. Atomic rename.
    let last_path = dir.join("last-crash.log");
    let tmp = last_path.with_extension("tmp");
    let _ = std::fs::write(&tmp, &body);
    let _ = std::fs::rename(tmp, &last_path);
    trim_crash_dumps(&dir);
}

/// Return the path to `last-crash.log` (in the crash dir), if it exists.
pub fn last_crash_path() -> Option<PathBuf> {
    logging::crash_dir().map(|d| d.join("last-crash.log"))
}

/// Check for a recent crash dump and return its path + message, if within
/// `max_age`. Used by the next-launch notice dialog.
pub fn recent_crash(max_age: Duration) -> Option<(PathBuf, String)> {
    let path = last_crash_path()?;
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    if modified.elapsed().ok()? > max_age {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    // Extract just the message line for a short notice.
    let msg = body
        .lines()
        .find(|l| l.starts_with("message:"))
        .map(|l| l.trim_start_matches("message:").trim().to_string())
        .unwrap_or_else(|| "panic".to_string());
    Some((path, msg))
}

/// Keep at most `MAX_CRASH_DUMPS`; drop oldest by mtime.
fn trim_crash_dumps(dir: &std::path::Path) {
    const MAX_CRASH_DUMPS: usize = 20;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let m = e.metadata().ok()?.modified().ok()?;
            Some((p, m))
        })
        .collect();
    if files.len() <= MAX_CRASH_DUMPS {
        return;
    }
    files.sort_by_key(|(_, m)| *m);
    for (p, _) in &files[..files.len() - MAX_CRASH_DUMPS] {
        let _ = std::fs::remove_file(p);
    }
}
