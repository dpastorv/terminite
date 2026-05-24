//! Structured logging for terminite. Append-only file at
//! `~/.terminite/log/terminite.log`, size-rotated; level-tagged
//! lines. Synchronous writes from the calling thread — correctness
//! over throughput at this volume. No new threads; the standing
//! system-impact discipline.
//!
//! API: `info!()`, `warn!()`, `error!()` macros. `init()` once at
//! startup. Failure to open the log file is silent — terminite still
//! runs, just without observability.
//!
//! Bounded throughout: log file rotates at `MAX_LOG_BYTES`; only
//! `ROTATIONS_KEPT` rotated copies are retained.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotation threshold for the active log file.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// How many rotated copies to keep (terminite.log.1, .2, …).
const ROTATIONS_KEPT: usize = 3;

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

struct LogState {
    file: File,
    path: PathBuf,
}

static STATE: Mutex<Option<LogState>> = Mutex::new(None);

/// Open the log file. Idempotent — calling again is harmless. Failure
/// (no `HOME`, fs error) leaves logging in a no-op state.
pub fn init() {
    let Some(path) = log_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    let Ok(file) = file else { return };
    let mut state = STATE.lock().expect("log state poisoned");
    *state = Some(LogState { file, path });
}

/// Append one line. Format: `YYYY-MM-DDTHH:MM:SSZ [level] message`.
pub fn log(level: Level, msg: &str) {
    let mut guard = STATE.lock().expect("log state poisoned");
    let Some(state) = guard.as_mut() else { return };
    let line = format!("{} [{}] {}\n", iso_timestamp_now(), level.as_str(), msg);
    let _ = state.file.write_all(line.as_bytes());
    // sync_data is heavy; rely on the OS flushing for normal lines.
    // Crash dumps go through `crash` below which calls sync_data.
    // Rotate if oversize. Rare enough that the metadata stat per line
    // is fine; if it ever shows in profiling, gate behind a counter.
    if let Ok(meta) = state.file.metadata() {
        if meta.len() > MAX_LOG_BYTES {
            rotate(state);
        }
    }
}

/// Convenience helpers — preferred over the raw `log` call.
pub fn info(msg: &str) {
    log(Level::Info, msg)
}
#[allow(dead_code)]
pub fn warn(msg: &str) {
    log(Level::Warn, msg)
}
pub fn error(msg: &str) {
    log(Level::Error, msg)
}

/// Where the active log lives. `$TERMINITE_LOG_DIR` overrides the
/// default; otherwise `~/.terminite/log/terminite.log`.
fn log_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TERMINITE_LOG_DIR") {
        return Some(PathBuf::from(dir).join("terminite.log"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/log/terminite.log"))
}

/// Where crash dumps live.
pub fn crash_dir() -> Option<PathBuf> {
    log_path().and_then(|p| p.parent().map(|d| d.join("crashes")))
}

/// Rotate the log file: `.log` → `.log.1`, `.log.1` → `.log.2`, etc.
/// Anything past `ROTATIONS_KEPT` is dropped. Opens a fresh active
/// `.log`.
fn rotate(state: &mut LogState) {
    let base = &state.path;
    // Drop the oldest, shift each rotated file up by one slot.
    for i in (1..=ROTATIONS_KEPT).rev() {
        let src = with_suffix(base, i);
        let dst = with_suffix(base, i + 1);
        if i == ROTATIONS_KEPT {
            let _ = fs::remove_file(&src);
        } else if src.exists() {
            let _ = fs::rename(&src, &dst);
        }
    }
    // Move the current file into the .1 slot.
    let _ = fs::rename(base, with_suffix(base, 1));
    // Re-open a fresh active file. If this fails, future logs are silent.
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(base)
    {
        state.file = file;
    }
}

fn with_suffix(base: &PathBuf, n: usize) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// `YYYY-MM-DDTHH:MM:SSZ` from `SystemTime::now()`. UTC; no
/// timezone library. Hand-rolled Gregorian conversion (Howard Hinnant's
/// civil-from-days algorithm) so we don't pull in a date crate for this.
fn iso_timestamp_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let (y, m, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Human-readable timestamp suitable for a filename: `YYYYMMDD-HHMMSS`.
pub fn filename_timestamp_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let (y, m, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Unix seconds → civil `(year, month, day, hour, minute, second)`.
/// UTC. Howard Hinnant, public domain.
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let seconds_per_day = 86_400i64;
    let days_since_epoch = secs.div_euclid(seconds_per_day);
    let time_of_day = secs.rem_euclid(seconds_per_day);
    let hour = (time_of_day / 3600) as u32;
    let minute = ((time_of_day % 3600) / 60) as u32;
    let second = (time_of_day % 60) as u32;

    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970() {
        let (y, m, d, h, mi, s) = civil_from_unix(0);
        assert_eq!((y, m, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn known_date_2026_05_24() {
        // 2026-05-24T14:23:01Z = 1779632581 unix seconds.
        let (y, m, d, h, mi, s) = civil_from_unix(1779632581);
        assert_eq!((y, m, d, h, mi, s), (2026, 5, 24, 14, 23, 1));
    }

    #[test]
    fn leap_day_works() {
        // 2024-02-29T00:00:00Z = 1709164800.
        let (y, m, d, _, _, _) = civil_from_unix(1709164800);
        assert_eq!((y, m, d), (2024, 2, 29));
    }
}
