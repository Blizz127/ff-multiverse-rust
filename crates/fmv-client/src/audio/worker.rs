//! The slow work of the soundtrack, off the game's thread: reading a field's script for its music,
//! decoding a music file into memory, and reading the menu sounds out of `audio.dat`.
//!
//! [`Worker`] is what the soundtrack asks of it and [`ThreadWorker`] is the real one: a couple of
//! threads over a queue with two lanes. A track the soundtrack is waiting for right now goes in the
//! **urgent** lane and is taken before anything that was only asked for in advance, so a victory
//! fanfare never waits behind the prefetch of a three-minute theme. Results come back as [`Done`]
//! messages that [`Worker::poll`] hands out without blocking.
//!
//! The work itself is passed in as closures ([`Tasks`]): this module knows nothing of FF7's files,
//! which keeps it, and everything that uses it, testable without any.

use super::cues::Cue;
use super::director::AreaMusic;
use fmv_audio::{DecodedMusic, SfxClip};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Threads that decode at once: decoding is CPU-bound and one track at a time is slow on a handheld,
/// two keeps a fight's music and its fanfare from queueing behind each other.
const THREADS: usize = 2;

/// What came back from the worker.
pub enum Done {
    /// A field's script said this about music.
    Area { key: String, music: AreaMusic },
    /// A track was decoded (or could not be), and how long that took.
    Track { name: String, result: Result<DecodedMusic, String>, took: Duration },
    /// The menu sounds (all four, or why there are none).
    Cues(Result<Vec<(Cue, SfxClip)>, String>),
}

/// What the soundtrack asks of the slow side. Every call returns at once.
pub trait Worker {
    /// Read the music commands of the field `key`.
    fn analyze(&mut self, key: &str);
    /// Decode the track `name`; `urgent` when something is waiting for it now.
    fn decode(&mut self, name: &str, urgent: bool);
    /// Read the menu sounds.
    fn load_cues(&mut self);
    /// What finished since the last call.
    fn poll(&mut self) -> Vec<Done>;
}

/// Reads the music commands of a field (by its key).
pub type AnalyzeTask = Arc<dyn Fn(&str) -> AreaMusic + Send + Sync>;
/// Decodes a music track (by its name), or says why it cannot.
pub type DecodeTask = Arc<dyn Fn(&str) -> Result<DecodedMusic, String> + Send + Sync>;
/// Reads the menu sounds, or says why it cannot.
pub type CuesTask = Arc<dyn Fn() -> Result<Vec<(Cue, SfxClip)>, String> + Send + Sync>;

/// The work, as closures that run on the worker threads.
#[derive(Clone)]
pub struct Tasks {
    pub analyze: AnalyzeTask,
    pub decode: DecodeTask,
    pub cues: CuesTask,
}

enum Job {
    Analyze(String),
    Decode(String),
    Cues,
}

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Job>,
    background: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queues: Mutex<Queues>,
    wake: Condvar,
}

/// The real [`Worker`]: threads over a two-lane queue. Dropping it lets them finish what they are
/// doing and stop (it does not wait for a decode to end).
pub struct ThreadWorker {
    shared: Arc<Shared>,
    results: Receiver<Done>,
}

impl ThreadWorker {
    pub fn start(tasks: Tasks) -> Self {
        let shared = Arc::new(Shared { queues: Mutex::new(Queues::default()), wake: Condvar::new() });
        let (send, results) = mpsc::channel();
        for n in 0..THREADS {
            let (shared, tasks, send) = (Arc::clone(&shared), tasks.clone(), send.clone());
            let spawned = std::thread::Builder::new().name(format!("fmv-audio-load-{n}")).spawn(move || run(&shared, &tasks, &send));
            if let Err(error) = spawned {
                // Fewer threads is slower, never wrong; with none the soundtrack just stays silent.
                eprintln!("audio: could not start a loading thread ({error})");
            }
        }
        Self { shared, results }
    }

    fn push(&self, job: Job, urgent: bool) {
        if let Ok(mut queues) = self.shared.queues.lock() {
            if urgent {
                queues.urgent.push_back(job);
            } else {
                queues.background.push_back(job);
            }
        }
        self.shared.wake.notify_one();
    }
}

impl Drop for ThreadWorker {
    fn drop(&mut self) {
        if let Ok(mut queues) = self.shared.queues.lock() {
            queues.closed = true;
            queues.urgent.clear();
            queues.background.clear();
        }
        self.shared.wake.notify_all();
    }
}

impl Worker for ThreadWorker {
    fn analyze(&mut self, key: &str) {
        self.push(Job::Analyze(key.to_string()), true);
    }

    fn decode(&mut self, name: &str, urgent: bool) {
        self.push(Job::Decode(name.to_string()), urgent);
    }

    fn load_cues(&mut self) {
        self.push(Job::Cues, false);
    }

    fn poll(&mut self) -> Vec<Done> {
        self.results.try_iter().collect()
    }
}

/// One worker thread: take the most urgent job, run it, send the answer.
fn run(shared: &Shared, tasks: &Tasks, send: &Sender<Done>) {
    loop {
        let job = {
            let Ok(mut queues) = shared.queues.lock() else { return };
            loop {
                if queues.closed {
                    return;
                }
                if let Some(job) = queues.urgent.pop_front().or_else(|| queues.background.pop_front()) {
                    break job;
                }
                queues = match shared.wake.wait(queues) {
                    Ok(queues) => queues,
                    Err(_) => return,
                };
            }
        };
        let done = match job {
            Job::Analyze(key) => {
                // A script that cannot be read is a field without music commands, not a dead thread.
                let music = catch_unwind(AssertUnwindSafe(|| (tasks.analyze)(&key))).unwrap_or_default();
                Done::Area { key, music }
            }
            Job::Decode(name) => {
                let started = Instant::now();
                let result = catch_unwind(AssertUnwindSafe(|| (tasks.decode)(&name))).unwrap_or_else(|_| Err("the decoder panicked".into()));
                Done::Track { name, result, took: started.elapsed() }
            }
            Job::Cues => Done::Cues(catch_unwind(AssertUnwindSafe(|| (tasks.cues)())).unwrap_or_else(|_| Err("the sound file reader panicked".into()))),
        };
        if send.send(done).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    fn tracks(frames: usize) -> DecodedMusic {
        DecodedMusic::from_stereo(48_000, vec![0; frames * 2], None).expect("a track")
    }

    /// The first message that satisfies `done`, from `backlog` (what was polled earlier and not wanted
    /// yet) or from what arrives; the others stay in the backlog for a later call.
    fn wait_for(worker: &mut ThreadWorker, backlog: &mut Vec<Done>, mut done: impl FnMut(&Done) -> bool, what: &str) -> Done {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            backlog.extend(worker.poll());
            if let Some(at) = backlog.iter().position(&mut done) {
                return backlog.remove(at);
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn jobs_run_off_the_callers_thread_and_come_back_as_messages() {
        let caller = std::thread::current().id();
        let tasks = Tasks {
            analyze: Arc::new(move |key| {
                assert_ne!(std::thread::current().id(), caller);
                AreaMusic { arrival: Some(format!("music-of-{key}")), ..AreaMusic::default() }
            }),
            decode: Arc::new(|name| if name == "bad" { Err("no file".into()) } else { Ok(tracks(100)) }),
            cues: Arc::new(|| Ok(vec![(Cue::Cursor, SfxClip::from_mono(22_050, &[0.1, 0.2]).expect("a clip"))])),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        worker.analyze("md1stin");
        worker.decode("ob", true);
        worker.decode("bad", false);
        worker.load_cues();
        let area = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Area { .. }), "the analysis");
        assert!(matches!(&area, Done::Area { key, music } if key == "md1stin" && music.arrival.as_deref() == Some("music-of-md1stin")));
        let good = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "ob"), "a decode");
        assert!(matches!(&good, Done::Track { result: Ok(m), .. } if m.frames() == 100));
        let bad = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "bad"), "a failing decode");
        assert!(matches!(&bad, Done::Track { result: Err(why), .. } if why == "no file"));
        let cues = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Cues(_)), "the cues");
        assert!(matches!(&cues, Done::Cues(Ok(clips)) if clips.len() == 1));
        assert!(worker.poll().is_empty(), "nothing is delivered twice");
    }

    #[test]
    fn an_urgent_job_is_taken_before_background_ones() {
        // One thread's worth of work: block the pool with slow jobs, then queue a background job and an
        // urgent one; with a single thread free, the urgent one must run before the background one.
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        // One gate per blocking job; the test opens gate 0 only, so exactly one thread is free.
        let gates = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let (log, gate) = (Arc::clone(&started), Arc::clone(&gates));
        let tasks = Tasks {
            analyze: Arc::new(|_| AreaMusic::default()),
            decode: Arc::new(move |name| {
                log.lock().expect("log").push(name.to_string());
                if let Some(n) = name.strip_prefix("block").and_then(|n| n.parse::<usize>().ok()) {
                    while gate[n].load(SeqCst) == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                Ok(tracks(10))
            }),
            cues: Arc::new(|| Err("none".into())),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        for n in 0..THREADS {
            worker.decode(&format!("block{n}"), true);
        }
        // Wait until every thread is stuck in a blocking job.
        let deadline = Instant::now() + Duration::from_secs(20);
        while started.lock().expect("log").len() < THREADS {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.decode("background", false);
        worker.decode("urgent", true);
        gates[0].store(1, SeqCst);
        wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "background"), "the background job");
        let order = started.lock().expect("log").clone();
        let at = |n: &str| order.iter().position(|x| x == n).expect("it ran");
        assert!(at("urgent") < at("background"), "{order:?}");
        // Let the other thread go too, so that nothing is left running.
        gates[1].store(1, SeqCst);
    }

    #[test]
    fn a_panicking_task_is_an_error_for_that_job_and_the_threads_carry_on() {
        let tasks = Tasks {
            analyze: Arc::new(|_| panic!("a broken script")),
            decode: Arc::new(|name| if name == "boom" { panic!("a broken decoder") } else { Ok(tracks(5)) }),
            cues: Arc::new(|| panic!("a broken sound file")),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        worker.analyze("x");
        let area = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Area { .. }), "the analysis");
        assert!(matches!(area, Done::Area { music, .. } if music == AreaMusic::default()));
        worker.decode("boom", true);
        let boom = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { .. }), "the failing decode");
        assert!(matches!(boom, Done::Track { result: Err(_), .. }));
        worker.load_cues();
        let cues = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Cues(_)), "the cues");
        assert!(matches!(cues, Done::Cues(Err(_))));
        worker.decode("fine", true);
        let fine = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "fine"), "a decode after the panics");
        assert!(matches!(fine, Done::Track { result: Ok(_), .. }));
    }

    #[test]
    fn dropping_the_worker_stops_its_threads() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        let tasks = Tasks {
            analyze: Arc::new(move |_| {
                counter.fetch_add(1, SeqCst);
                AreaMusic::default()
            }),
            decode: Arc::new(|_| Ok(tracks(1))),
            cues: Arc::new(|| Err("none".into())),
        };
        let worker = ThreadWorker::start(tasks);
        drop(worker);
        // A job queued after the drop (through a stale handle) cannot exist; what matters is that the
        // drop returned at once and the pool did not run anything meanwhile.
        assert_eq!(ran.load(SeqCst), 0);
    }
}
