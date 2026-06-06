//! Small shared I/O helpers.

use std::io::{BufRead, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Read one line into `buf` (cleared first), reading **at most** `max + 1`
/// bytes from `reader`. Returns the number of bytes read (0 = EOF).
///
/// A documented `MAX_LINE_BYTES` enforced only *after* a plain
/// `read_line` / `lines()` is not a real bound — those buffer until a
/// newline or EOF, so a same-UID client or a malformed module can stream
/// gigabytes with no newline and OOM the host before the check runs. This
/// caps the read up front: if the return value exceeds `max`, the line was
/// over-long and got cut at the limit; the caller should treat that as a
/// protocol violation and disconnect, so the remainder is never buffered.
pub(crate) fn read_capped_line<R: BufRead>(
    reader: &mut R,
    max: usize,
    buf: &mut String,
) -> std::io::Result<usize> {
    buf.clear();
    reader.by_ref().take(max as u64 + 1).read_line(buf)
}

/// Atomically replace `path` with `contents`.
///
/// Writes to a same-directory temp file opened `O_EXCL` (so a pre-planted
/// symlink at a predictable temp name can't redirect the write), fsyncs it,
/// renames over the target, then fsyncs the parent dir so the swap is
/// durable across a crash. An existing file's mode is **preserved**; a new
/// file gets `default_mode` (pass `0o600` for anything private). Same
/// filesystem only — a cross-fs rename isn't atomic and will error.
///
/// This replaces the predictable-temp + `fs::write` + `rename` pattern that
/// (a) didn't preserve mode — a private `0600` rc became world-readable
/// `0644` — and (b) could truncate a user file if the process died mid-write.
pub(crate) fn atomic_write(
    path: impl AsRef<Path>,
    contents: &[u8],
    default_mode: u32,
) -> std::io::Result<()> {
    let path = path.as_ref();
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    // Preserve an existing file's mode; else use the caller's default.
    let mode = std::fs::symlink_metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(default_mode);
    let pid = std::process::id();
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..32u32 {
        let tmp = dir.join(format!(".{base}.terminite-tmp.{pid}.{attempt}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL — refuse to follow an existing temp/symlink
            .mode(mode)
            .open(&tmp)
        {
            Ok(mut f) => {
                // Force the exact mode regardless of umask.
                let _ = f.set_permissions(std::fs::Permissions::from_mode(mode));
                let write_res = f.write_all(contents).and_then(|_| f.sync_all());
                drop(f);
                if let Err(e) = write_res {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                if let Err(e) = std::fs::rename(&tmp, path) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                // Durably record the rename in the directory entry.
                if let Ok(d) = std::fs::File::open(dir) {
                    let _ = d.sync_all();
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last_err = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "atomic_write: no free temp name",
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_preserves_existing_mode() {
        let dir = std::env::temp_dir().join(format!("terminite-aw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rc");
        atomic_write(&f, b"one\n", 0o600).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Rewrite with a 0644 default — the existing 0600 must survive
        // (the reproduced bug flipped a private rc to world-readable).
        atomic_write(&f, b"two\n", 0o644).unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "existing mode must be preserved");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_new_file_uses_default_mode() {
        let dir = std::env::temp_dir().join(format!("terminite-aw2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("new");
        atomic_write(&f, b"hi\n", 0o600).unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
