//! Log lines of code that runs under the world lock, written after the lock is released.
//!
//! The world mutex is the one lock every connection task and the tick loop share. Writing a log
//! line can block for as long as the reader of stderr likes (a full pipe to a stalled `journald`,
//! a paused terminal), and a task that blocks while it holds the mutex stops the whole server. So
//! code inside `World` never calls `warn!` and friends: it records the line with
//! `DeferredLogs::{warn, info, debug}` and [`with_locked`] writes the lines out once the lock is
//! gone. Everything outside `World` that wants to log while it also uses the world does the same
//! thing by construction: the world is only reachable through `Shared::with_world`.

use std::sync::{Mutex, PoisonError};

use tracing::{debug, info, warn};

use crate::world::World;

/// A hold of the lock records at most this many lines; the rest are counted, not kept (a bug that
/// logs in a loop must not become unbounded memory, and a flood of lines is not wanted anyway).
const MAX_LINES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Warn,
    Info,
    Debug,
}

/// One recorded line.
#[derive(Debug)]
pub(crate) struct Line {
    level: Level,
    text: String,
}

/// The lines recorded since they were last taken.
#[derive(Debug, Default)]
pub(crate) struct DeferredLogs {
    lines: Vec<Line>,
    dropped: usize,
}

impl DeferredLogs {
    fn push(&mut self, level: Level, text: String) {
        if self.lines.len() < MAX_LINES {
            self.lines.push(Line { level, text });
        } else {
            self.dropped += 1;
        }
    }

    pub fn warn(&mut self, text: String) {
        self.push(Level::Warn, text);
    }

    pub fn info(&mut self, text: String) {
        self.push(Level::Info, text);
    }

    pub fn debug(&mut self, text: String) {
        self.push(Level::Debug, text);
    }

    /// Hand over what was recorded (and forget it).
    pub fn take(&mut self) -> Vec<Line> {
        let mut lines = std::mem::take(&mut self.lines);
        let dropped = std::mem::take(&mut self.dropped);
        if dropped > 0 {
            lines.push(Line { level: Level::Warn, text: format!("{dropped} more log line(s) from one world operation were not kept") });
        }
        lines
    }
}

/// Write the lines out. Call it with no lock held.
pub(crate) fn emit(lines: Vec<Line>) {
    for Line { level, text } in lines {
        match level {
            Level::Warn => warn!(target: "fmv_server::world", "{text}"),
            Level::Info => info!(target: "fmv_server::world", "{text}"),
            Level::Debug => debug!(target: "fmv_server::world", "{text}"),
        }
    }
}

/// Run `f` with the world locked, release the lock, THEN write the lines `f` logged. A poisoned
/// lock is recovered rather than cascading a panic into every connection.
pub(crate) fn with_locked<R>(world: &Mutex<World>, f: impl FnOnce(&mut World) -> R) -> R {
    let (result, lines) = {
        let mut world = world.lock().unwrap_or_else(PoisonError::into_inner);
        let result = f(&mut world);
        (result, world.take_logs())
    };
    emit(lines);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use std::io::Write;
    use std::sync::Arc;

    fn world() -> Mutex<World> {
        let area = FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).expect("fixture area");
        Mutex::new(World::new(area).expect("world"))
    }

    /// A log sink that records, for every write, whether the world lock was free at that moment.
    #[derive(Clone)]
    struct Probe {
        world: Arc<Mutex<World>>,
        seen: Arc<Mutex<Vec<(String, bool)>>>,
    }

    impl Write for Probe {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let free = self.world.try_lock().is_ok();
            self.seen.lock().unwrap().push((String::from_utf8_lossy(bytes).into_owned(), free));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Probe {
        type Writer = Probe;
        fn make_writer(&'a self) -> Probe {
            self.clone()
        }
    }

    #[test]
    fn lines_logged_under_the_world_lock_are_written_after_it_is_released() {
        let world = Arc::new(world());
        let probe = Probe { world: Arc::clone(&world), seen: Arc::default() };
        let subscriber = tracing_subscriber::fmt()
            .with_writer(probe.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let answer = tracing::subscriber::with_default(subscriber, || {
            with_locked(&world, |w| {
                w.defer_warn("a slow client was dropped");
                w.defer_info("a battle started");
                w.defer_debug("a queue closed");
                42
            })
        });
        assert_eq!(answer, 42, "the closure's result comes back");
        let seen = probe.seen.lock().unwrap();
        let text: String = seen.iter().map(|(t, _)| t.as_str()).collect();
        for needle in ["WARN", "a slow client was dropped", "INFO", "a battle started", "DEBUG", "a queue closed", "fmv_server::world"] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|(line, free)| *free || line.is_empty()),
            "a line was written while the world lock was held: {seen:?}"
        );
        // Nothing is left behind for the next hold to write twice.
        assert!(world.lock().unwrap().take_logs().is_empty());
    }

    /// The guarantee is structural: nothing under the world lock writes a log line itself. A new
    /// `warn!` in the world would silently bring the stalled-log-sink hazard back, so this fails
    /// instead (record through `World::logs`, as the existing lines do).
    #[test]
    fn the_world_modules_never_log_directly() {
        // Every file of the world module, found on disk so that a module added later is covered too.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src.join("world.rs")];
        for entry in std::fs::read_dir(src.join("world")).expect("src/world exists") {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
        assert!(files.len() >= 3, "found {files:?}");
        for file in files {
            let source = std::fs::read_to_string(&file).expect("a readable source file");
            for needle in ["error!(", "warn!(", "info!(", "debug!(", "trace!(", "tracing::", "eprintln!(", "println!("] {
                assert!(
                    !source.contains(needle),
                    "{} must not log under the world lock ({needle}): record it with World::logs",
                    file.display()
                );
            }
        }
    }

    #[test]
    fn a_hold_that_logs_in_a_loop_cannot_grow_without_bound() {
        let mut logs = DeferredLogs::default();
        for i in 0..10_000 {
            logs.warn(format!("line {i}"));
        }
        let lines = logs.take();
        assert_eq!(lines.len(), MAX_LINES + 1);
        assert!(lines.last().unwrap().text.contains("9744 more"), "{:?}", lines.last());
        assert!(logs.take().is_empty());
    }
}
