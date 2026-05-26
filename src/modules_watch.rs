//! fs-watch on `~/.terminite/modules/` — auto-reloads the module
//! registry when manifests / dirs are added, removed, or edited.
//!
//! Design:
//!   - `notify` (cross-platform; FSEvents on macOS, inotify on Linux).
//!   - Recursive watch on the modules root so changes one level deep
//!     (e.g., touching `module/manifest.toml`) fire too.
//!   - Coalesce: a multi-file drop fires many filesystem events.
//!     The watcher callback ratelimits via a shared `Instant` —
//!     events within DEBOUNCE_MS of the previous send are dropped.
//!     The first event also schedules a trailing send via a tiny
//!     timer thread so we don't miss a late-arriving change.
//!
//! Lifecycle: the returned `ModulesWatcher` holds the live notify
//! watcher; dropping it stops the watch (notify's `Watcher::Drop`
//! joins its own IO thread). One watcher per Renderer; lives the
//! whole session.
//!
//! System-impact bound: one OS-level watch + one debounce thread.
//! No per-event work beyond a Mutex acquire + an event send.

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

const DEBOUNCE_MS: u64 = 250;
const TAIL_FLUSH_MS: u64 = 350;

/// Owns the live notify watcher. Dropping it stops the watch.
pub struct ModulesWatcher {
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    #[allow(dead_code)]
    debounce_state: Arc<Mutex<DebounceState>>,
    shutdown: Arc<AtomicBool>,
}

struct DebounceState {
    last_sent: Option<Instant>,
    pending: bool,
}

impl Drop for ModulesWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

/// Start watching the modules directory. Returns `None` if the dir
/// can't be created or the watcher fails to spawn — terminite still
/// runs, you just need `terminite module reload` to pick up changes.
pub fn start(
    modules_dir: PathBuf,
    proxy: EventLoopProxy<UserEvent>,
) -> Option<ModulesWatcher> {
    if let Err(e) = std::fs::create_dir_all(&modules_dir) {
        crate::logging::warn(&format!(
            "modules_watch: can't create {} — {e}",
            modules_dir.display()
        ));
        return None;
    }

    let debounce_state = Arc::new(Mutex::new(DebounceState {
        last_sent: None,
        pending: false,
    }));
    let shutdown = Arc::new(AtomicBool::new(false));

    let cb_state = debounce_state.clone();
    let cb_proxy = proxy.clone();
    let cb_shutdown = shutdown.clone();
    let mut watcher = match notify::recommended_watcher(
        move |res: notify::Result<Event>| {
            if cb_shutdown.load(Ordering::Acquire) {
                return;
            }
            if res.is_err() {
                return;
            }
            let mut st = match cb_state.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let now = Instant::now();
            let allow = match st.last_sent {
                Some(t) => now.duration_since(t) >= Duration::from_millis(DEBOUNCE_MS),
                None => true,
            };
            if allow {
                st.last_sent = Some(now);
                let _ = cb_proxy.send_event(UserEvent::ModulesChanged);
            } else {
                // Suppress this event but mark pending so the tail
                // flush picks up the late change.
                st.pending = true;
            }
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            crate::logging::warn(&format!("modules_watch: watcher spawn failed: {e}"));
            return None;
        }
    };
    if let Err(e) = watcher.watch(&modules_dir, RecursiveMode::Recursive) {
        crate::logging::warn(&format!(
            "modules_watch: can't watch {} — {e}",
            modules_dir.display()
        ));
        return None;
    }

    // Tail-flush thread: every TAIL_FLUSH_MS, if the debounce
    // suppressed a recent event, send one ModulesChanged so the
    // last change isn't lost. Exits when shutdown is set (the
    // ModulesWatcher's Drop sets it).
    let tail_state = debounce_state.clone();
    let tail_proxy = proxy;
    let tail_shutdown = shutdown.clone();
    thread::spawn(move || {
        while !tail_shutdown.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(TAIL_FLUSH_MS));
            let send = {
                let mut st = match tail_state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if st.pending {
                    st.pending = false;
                    st.last_sent = Some(Instant::now());
                    true
                } else {
                    false
                }
            };
            if send {
                let _ = tail_proxy.send_event(UserEvent::ModulesChanged);
            }
        }
    });

    crate::logging::info(&format!("modules_watch: watching {}", modules_dir.display()));
    Some(ModulesWatcher {
        watcher,
        debounce_state,
        shutdown,
    })
}
