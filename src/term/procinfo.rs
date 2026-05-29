//! Process introspection — cwd, display name, pgid, exe path, name cleanup.
//! Split out of the terminal core; pure OS queries.

use super::*;

pub(crate) fn proc_cwd(pid: i32) -> Option<PathBuf> {
    use std::mem::MaybeUninit;
    let mut info: MaybeUninit<libc::proc_vnodepathinfo> = MaybeUninit::uninit();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if n <= 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let raw = info.pvi_cdir.vip_path;
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(raw.as_ptr() as *const u8, raw.len())
    };
    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..nul]).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn proc_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

/// Best display name for a process. Prefers `proc_name` (the macOS `comm`)
/// — for most things that's correct ("zsh", "vim", "claude"). When the
/// comm looks like a version number (claude's bundled binary lives at e.g.
/// `~/.bun/.../claude-code/2.1.146/cli`, so its file name *is* "2.1.146"),
/// we walk the executable path and pick the nearest non-version component.
pub fn process_display_name(pid: i32) -> Option<String> {
    let comm = proc_comm(pid);
    if let Some(name) = comm.as_deref() {
        if !looks_like_version(name) {
            return comm;
        }
    }
    proc_executable_path(pid)
        .and_then(|p| best_name_from_path(&p))
        .or(comm)
}

#[allow(dead_code)]
pub(crate) fn proc_basename(pid: i32) -> Option<String> {
    process_display_name(pid)
}

#[cfg(target_os = "macos")]
pub(crate) fn proc_comm(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    let bytes = &buf[..n as usize];
    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..nul]).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn proc_comm(_pid: i32) -> Option<String> {
    None
}

/// Process group ID of `pid`, via `proc_pidinfo(PROC_PIDTBSDINFO)`. Used to
/// recognise the shell's subshells (which share its PGID) as "still at the
/// prompt" for close-warning purposes.
#[cfg(target_os = "macos")]
pub(crate) fn proc_pgid(pid: i32) -> Option<i32> {
    use std::mem::MaybeUninit;
    let mut info: MaybeUninit<libc::proc_bsdinfo> = MaybeUninit::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if n <= 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(info.pbi_pgid as i32)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn proc_pgid(_pid: i32) -> Option<i32> {
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn proc_executable_path(pid: i32) -> Option<String> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe {
        libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    std::str::from_utf8(&buf[..n as usize])
        .ok()
        .map(|s| s.to_string())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn proc_executable_path(_pid: i32) -> Option<String> {
    None
}

pub(crate) fn looks_like_version(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

pub(crate) fn best_name_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .components()
        .rev()
        .filter_map(|c| c.as_os_str().to_str())
        .find(|s| {
            !s.is_empty()
                && *s != "/"
                && *s != "bin"
                && *s != "cli"
                && !looks_like_version(s)
        })
        .map(strip_version_suffix)
}

/// Strip a trailing `-1.2.3` / `_v1.2.3` style version suffix from a name,
/// so e.g. `"claude-code-2.1.145"` becomes `"claude-code"`. Leaves names
/// without such a suffix untouched.
pub(crate) fn strip_version_suffix(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return s.to_string();
    }
    let mut i = bytes.len();
    while i > 0 {
        let b = bytes[i - 1];
        if b.is_ascii_digit() || b == b'.' {
            i -= 1;
        } else {
            break;
        }
    }
    // Did we eat any version-y trailer at all?
    if i == bytes.len() {
        return s.to_string();
    }
    // Optional v before the digits.
    let mut j = i;
    if j > 0 && (bytes[j - 1] == b'v' || bytes[j - 1] == b'V') {
        j -= 1;
    }
    // Require a `-` or `_` separator immediately before the version.
    if j == 0 || (bytes[j - 1] != b'-' && bytes[j - 1] != b'_') {
        return s.to_string();
    }
    let stem = &s[..j - 1];
    if stem.is_empty() {
        s.to_string()
    } else {
        stem.to_string()
    }
}

/// Diagnostic-only: returns `(comm, executable_path)` for `pid` so we can
/// see what each lookup reports in stderr without exposing the raw libc
/// helpers outside this module.
#[allow(dead_code)]
pub fn process_debug_strings(pid: i32) -> (Option<String>, Option<String>) {
    (proc_comm(pid), proc_executable_path(pid))
}

/// Human-readable cwd:
/// - `~` for HOME exactly
/// - `~/foo/bar` for paths under HOME up to ~40 chars
/// - `.../grandparent/parent/leaf` when the full path gets too long
/// - the bare last component when there's no HOME context
pub(crate) fn display_cwd(cwd: &Path) -> String {
    let s = if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cwd == home {
            return "~".to_string();
        }
        if let Ok(rel) = cwd.strip_prefix(&home) {
            format!("~/{}", rel.display())
        } else {
            cwd.display().to_string()
        }
    } else {
        cwd.display().to_string()
    };
    // Long paths: keep the last three components prefixed with `…/`. Keeps
    // the title scannable in the tab bar without truncating mid-segment.
    const MAX: usize = 40;
    if s.chars().count() <= MAX {
        return s;
    }
    let comps: Vec<&str> = cwd
        .iter()
        .filter_map(|c| c.to_str())
        .filter(|c| !c.is_empty() && *c != "/")
        .collect();
    if comps.len() <= 3 {
        return s;
    }
    let tail = comps[comps.len().saturating_sub(3)..].join("/");
    format!("…/{tail}")
}
