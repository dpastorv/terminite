//! Small shared I/O helpers.

use std::io::{BufRead, Read};

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
