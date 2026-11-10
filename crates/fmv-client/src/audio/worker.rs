//! The slow work of the soundtrack, off the game's thread: reading a field's script for its music,
//! decoding a music file into memory, reading the menu sounds and the sounds of a battle out of `audio.dat` (only
//! the clips asked for: the file is 70 MB), and reading what a field's scripts play for the player's story (the plan,
//! and only the clips it names).
//!
//! [`Worker`] is what the soundtrack asks of it and [`ThreadWorker`] is the real one: a couple of
//! threads over a queue with two lanes. A track the soundtrack is waiting for right now goes in the
//! **urgent** lane and is taken before anything that was only asked for in advance, so a victory
//! fanfare never waits behind the prefetch of a three-minute theme (the sounds of a battle that is starting go in
//! it too: the first blow lands half a second after the fight begins). Results come back as [`Done`]
//! messages that [`Worker::poll`] hands out without blocking.
//!
//! The work itself is passed in as closures ([`Tasks`]): this module knows nothing of FF7's files,
//! which keeps it, and everything that uses it, testable without any.

use super::cues::Cue;
use super::director::AreaMusic;
use super::field::FieldRead;
use crate::mods::VoiceCue;
use fmv_audio::{DecodedMusic, SfxClip};
use std::collections::{HashSet, VecDeque};
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
    /// The sounds of a battle that were asked for (by the game's sound id), and how long reading them took.
    Sounds { read: SoundsRead, took: Duration },
    /// What a field's scripts play for a story (the plan and the clips it names), or why that could not be read, and how
    /// long it took. `story` is the progression blob it was made for.
    Field { key: String, story: Vec<u8>, read: Result<Box<FieldRead>, String>, took: Duration },
    /// The voice clip an ask wanted (`Ok(None)`: it has none, which is most windows), or why it could not be read; `seq` is
    /// the ask, so that an answer to an ask that has been replaced is thrown away.
    Voice { seq: u64, result: Result<Option<VoiceClip>, String> },
    /// The sound layer's ambience for a battle of this formation (`None`: it has none, or it does not decode).
    BattleAmbience { formation: u16, ambience: Option<Box<super::field::ModAmbience>> },
}

/// What the voice layer is asked for: the clip of one window (or of one choice of a question), and which ask it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceRequest {
    pub seq: u64,
    pub cue: VoiceCue,
}

/// A voice clip ready to play: its path in the mod archive (for the log) and the mixer clip, which ends in a silent loop region so
/// that the engine's loop handle can stop it (see [`super::Live`]'s voice).
#[derive(Debug)]
pub struct VoiceClip {
    pub name: String,
    pub clip: SfxClip,
    /// How long the voice itself is (the clip also holds the silent tail).
    pub length: Duration,
}

/// What reading a list of sound ids gave: the clips that exist and, for each id that has none, why not (an empty slot, a
/// clip that does not decode, no sound files at all).
#[derive(Default)]
pub struct SoundsRead {
    pub clips: Vec<(u16, SfxClip)>,
    pub missing: Vec<(u16, String)>,
    /// For an id whose sound-layer section names several clips: all of them, in the section's order (the first is the one in
    /// `clips`). They are played in turn, as FFNx's `sequential` counter does, so a repeated sound does not sound the same.
    pub variants: Vec<(u16, Vec<SfxClip>)>,
}

/// What the soundtrack asks of the slow side. Every call returns at once.
pub trait Worker {
    /// Read the music commands of the field `key`.
    fn analyze(&mut self, key: &str);
    /// Decode the track `name`; `urgent` when something is waiting for it now.
    fn decode(&mut self, name: &str, urgent: bool);
    /// Read the menu sounds.
    fn load_cues(&mut self);
    /// Read the clips of these sound ids (a battle that starts: what its fights can need), before anything in advance.
    fn load_sounds(&mut self, ids: Vec<u16>);
    /// Read what the scripts of the field `key` play for the progression `story`, and the clips that plan names which are not
    /// in `have` (the ids the caller holds already), before anything in advance.
    fn scan_field(&mut self, key: &str, story: Vec<u8>, have: Vec<u16>);
    /// Read the voice clip a window asks for, before anything in advance (a voice is wanted the moment its window opens). A worker
    /// without a voice layer does nothing.
    fn load_voice(&mut self, _request: VoiceRequest) {}
    /// Read the sound layer's ambience for a battle of formation `formation`, before anything in advance (a fight that starts
    /// wants it now). A worker without a sound layer does nothing.
    fn load_battle_ambience(&mut self, _formation: u16) {}
    /// What finished since the last call.
    fn poll(&mut self) -> Vec<Done>;
}

/// Reads the music commands of a field (by its key).
pub type AnalyzeTask = Arc<dyn Fn(&str) -> AreaMusic + Send + Sync>;
/// Decodes a music track (by its name), or says why it cannot.
pub type DecodeTask = Arc<dyn Fn(&str) -> Result<DecodedMusic, String> + Send + Sync>;
/// Reads the menu sounds, or says why it cannot.
pub type CuesTask = Arc<dyn Fn() -> Result<Vec<(Cue, SfxClip)>, String> + Send + Sync>;

/// Reads the clips of some of the game's sound ids.
pub type SoundsTask = Arc<dyn Fn(&[u16]) -> SoundsRead + Send + Sync>;
/// Reads what the scripts of a field play for a story (the progression blob), with the clips of the ids not in the list.
pub type FieldTask = Arc<dyn Fn(&str, &[u8], &[u16]) -> Result<FieldRead, String> + Send + Sync>;
/// Reads the clip a voice cue asks for: `Ok(None)` when there is none (most windows have no voice), why not when it cannot.
pub type VoiceTask = Arc<dyn Fn(&VoiceCue) -> Result<Option<VoiceClip>, String> + Send + Sync>;

/// Reads the sound layer's ambience of a battle (by its formation), `None` when it has none.
pub type BattleAmbienceTask = Arc<dyn Fn(u16) -> Option<super::field::ModAmbience> + Send + Sync>;

/// The voice task of a soundtrack without a voice layer: there is never a clip.
#[cfg(test)]
pub fn no_voice() -> VoiceTask {
    Arc::new(|_| Ok(None))
}

/// The work, as closures that run on the worker threads.
#[derive(Clone)]
pub struct Tasks {
    pub analyze: AnalyzeTask,
    pub decode: DecodeTask,
    pub cues: CuesTask,
    pub sounds: SoundsTask,
    pub field: FieldTask,
    pub voice: VoiceTask,
    /// The sound layer's battle ambience (`None`: no layer).
    pub battle_ambience: Option<BattleAmbienceTask>,
}

enum Job {
    Analyze(String),
    Decode(String),
    Cues,
    Sounds(Vec<u16>),
    Field { key: String, story: Vec<u8>, have: Vec<u16> },
    Voice(VoiceRequest),
    BattleAmbience(u16),
}

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Job>,
    background: VecDeque<Job>,
    /// Tracks a thread is decoding right now (asking for one of those again would decode it twice).
    running: HashSet<String>,
    closed: bool,
}

impl Queues {
    /// Whether a decode of `name` is waiting (in either lane) or running.
    fn has_decode(&self, name: &str) -> bool {
        let is = |j: &Job| matches!(j, Job::Decode(n) if n == name);
        self.running.contains(name) || self.urgent.iter().any(is) || self.background.iter().any(is)
    }
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
            if let Job::Decode(name) = &job {
                // A track that is already being decoded or waiting is not queued twice. A request that is
                // urgent moves a waiting one from the background lane to the front: a track asked for in
                // advance and wanted now must not stay behind the rest of the advance work.
                if queues.has_decode(name) {
                    if urgent {
                        let at = queues.background.iter().position(|j| matches!(j, Job::Decode(n) if n == name));
                        if let Some(waiting) = at.and_then(|at| queues.background.remove(at)) {
                            queues.urgent.push_back(waiting);
                        }
                    }
                    return;
                }
            }
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

    fn load_sounds(&mut self, ids: Vec<u16>) {
        self.push(Job::Sounds(ids), true);
    }

    fn scan_field(&mut self, key: &str, story: Vec<u8>, have: Vec<u16>) {
        self.push(Job::Field { key: key.to_string(), story, have }, true);
    }

    fn load_battle_ambience(&mut self, formation: u16) {
        self.push(Job::BattleAmbience(formation), true);
    }

    fn load_voice(&mut self, request: VoiceRequest) {
        self.push(Job::Voice(request), true);
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
                    if let Job::Decode(name) = &job {
                        queues.running.insert(name.clone());
                    }
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
                if let Ok(mut queues) = shared.queues.lock() {
                    queues.running.remove(&name);
                }
                Done::Track { name, result, took: started.elapsed() }
            }
            Job::Cues => Done::Cues(catch_unwind(AssertUnwindSafe(|| (tasks.cues)())).unwrap_or_else(|_| Err("the sound file reader panicked".into()))),
            Job::Sounds(ids) => {
                let started = Instant::now();
                let read = catch_unwind(AssertUnwindSafe(|| (tasks.sounds)(&ids)))
                    .unwrap_or_else(|_| SoundsRead { missing: ids.iter().map(|id| (*id, "the sound file reader panicked".to_string())).collect(), ..SoundsRead::default() });
                Done::Sounds { read, took: started.elapsed() }
            }
            Job::Field { key, story, have } => {
                let started = Instant::now();
                let read = catch_unwind(AssertUnwindSafe(|| (tasks.field)(&key, &story, &have))).unwrap_or_else(|_| Err("the field reader panicked".to_string()));
                Done::Field { key, story, read: read.map(Box::new), took: started.elapsed() }
            }
            Job::BattleAmbience(formation) => {
                let ambience = tasks.battle_ambience.as_ref().and_then(|task| catch_unwind(AssertUnwindSafe(|| task(formation))).ok().flatten());
                Done::BattleAmbience { formation, ambience: ambience.map(Box::new) }
            }
            Job::Voice(request) => {
                let result = catch_unwind(AssertUnwindSafe(|| (tasks.voice)(&request.cue))).unwrap_or_else(|_| Err("the voice reader panicked".to_string()));
                Done::Voice { seq: request.seq, result }
            }
        };
        if send.send(done).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::field::FieldPlan;
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
            voice: no_voice(),
            battle_ambience: None,
            analyze: Arc::new(move |key| {
                assert_ne!(std::thread::current().id(), caller);
                AreaMusic { arrival: Some(format!("music-of-{key}")), ..AreaMusic::default() }
            }),
            decode: Arc::new(|name| if name == "bad" { Err("no file".into()) } else { Ok(tracks(100)) }),
            cues: Arc::new(|| Ok(vec![(Cue::Cursor, SfxClip::from_mono(22_050, &[0.1, 0.2]).expect("a clip"))])),
            sounds: Arc::new(|ids| SoundsRead {
                clips: ids.iter().filter(|id| **id != 99).map(|id| (*id, SfxClip::from_mono(22_050, &[0.1, 0.2]).expect("a clip"))).collect(),
                missing: ids.iter().filter(|id| **id == 99).map(|id| (*id, "an empty slot".to_string())).collect(),
                variants: Vec::new(),
            }),
            field: Arc::new(move |key, story, have| {
                assert_ne!(std::thread::current().id(), caller);
                if key == "nowhere" {
                    return Err("no such field".to_string());
                }
                // the plan carries the key and the story it was asked for; the clips are those not held
                let plan = FieldPlan { key: key.to_string(), story: story.to_vec(), ..FieldPlan::default() };
                let clips = (1..=3u16).filter(|id| !have.contains(id)).map(|id| (id, SfxClip::from_mono(22_050, &[0.1, 0.2]).expect("a clip"))).collect();
                Ok(FieldRead { plan, clips: SoundsRead { clips, ..SoundsRead::default() }, ambience: None })
            }),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        worker.analyze("md1stin");
        worker.decode("ob", true);
        worker.decode("bad", false);
        worker.load_cues();
        worker.load_sounds(vec![5, 99, 18]);
        worker.scan_field("nmkin_2", vec![1, 15, 0], vec![2]);
        worker.scan_field("nowhere", Vec::new(), Vec::new());
        let area = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Area { .. }), "the analysis");
        assert!(matches!(&area, Done::Area { key, music } if key == "md1stin" && music.arrival.as_deref() == Some("music-of-md1stin")));
        let good = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "ob"), "a decode");
        assert!(matches!(&good, Done::Track { result: Ok(m), .. } if m.frames() == 100));
        let bad = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "bad"), "a failing decode");
        assert!(matches!(&bad, Done::Track { result: Err(why), .. } if why == "no file"));
        let cues = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Cues(_)), "the cues");
        assert!(matches!(&cues, Done::Cues(Ok(clips)) if clips.len() == 1));
        let sounds = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Sounds { .. }), "the battle sounds");
        let Done::Sounds { read, .. } = sounds else { unreachable!("waited for it") };
        assert_eq!(read.clips.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [5, 18]);
        assert_eq!(read.missing, [(99, "an empty slot".to_string())]);
        // a field's plan comes back with the story it was made for, and the clips of the ids that were not held
        let field = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Field { key, .. } if key == "nmkin_2"), "the field's sounds");
        let Done::Field { story, read, .. } = field else { unreachable!("waited for it") };
        assert_eq!(story, [1, 15, 0]);
        let read = read.expect("a plan");
        assert_eq!((read.plan.key.as_str(), read.plan.story.as_slice()), ("nmkin_2", &[1u8, 15, 0][..]));
        assert_eq!(read.clips.clips.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 3], "id 2 was held");
        let nowhere = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Field { key, .. } if key == "nowhere"), "the failing field");
        assert!(matches!(nowhere, Done::Field { read: Err(why), .. } if why == "no such field"));
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
            voice: no_voice(),
            battle_ambience: None,
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
            sounds: Arc::new(|_| SoundsRead::default()),
            field: Arc::new(|_, _, _| Err("no field".to_string())),
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
    fn a_waiting_background_decode_is_promoted_by_an_urgent_request_and_never_queued_twice() {
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let gates = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let (log, gate) = (Arc::clone(&started), Arc::clone(&gates));
        let tasks = Tasks {
            voice: no_voice(),
            battle_ambience: None,
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
            sounds: Arc::new(|_| SoundsRead::default()),
            field: Arc::new(|_, _, _| Err("no field".to_string())),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        for n in 0..THREADS {
            worker.decode(&format!("block{n}"), true);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while started.lock().expect("log").len() < THREADS {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        // `first` is asked for in advance, then `wanted` too; `wanted` is then wanted now, twice over.
        worker.decode("first", false);
        worker.decode("wanted", false);
        worker.decode("wanted", true);
        worker.decode("wanted", true);
        worker.decode("first", false);
        // A decode a thread is running right now is not queued again either.
        worker.decode("block0", true);
        gates[0].store(1, SeqCst);
        wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "first"), "the other job");
        let order = started.lock().expect("log").clone();
        let at = |n: &str| order.iter().position(|x| x == n).expect("it ran");
        assert!(at("wanted") < at("first"), "the promoted job ran first: {order:?}");
        assert_eq!(order.iter().filter(|n| *n == "wanted").count(), 1, "decoded once: {order:?}");
        assert_eq!(order.iter().filter(|n| *n == "first").count(), 1, "decoded once: {order:?}");
        assert_eq!(order.iter().filter(|n| *n == "block0").count(), 1, "a running decode is not started again: {order:?}");
        gates[1].store(1, SeqCst);
    }

    #[test]
    fn the_sounds_of_a_battle_that_starts_are_read_before_music_that_was_only_asked_for_in_advance() {
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let gate = Arc::new(AtomicUsize::new(0));
        let (log, hold) = (Arc::clone(&started), Arc::clone(&gate));
        let sounds_log = Arc::clone(&started);
        let tasks = Tasks {
            voice: no_voice(),
            battle_ambience: None,
            analyze: Arc::new(|_| AreaMusic::default()),
            decode: Arc::new(move |name| {
                log.lock().expect("log").push(name.to_string());
                if name.starts_with("block") {
                    while hold.load(SeqCst) == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                Ok(tracks(10))
            }),
            cues: Arc::new(|| Err("none".into())),
            sounds: Arc::new(move |ids| {
                sounds_log.lock().expect("log").push(format!("sounds {ids:?}"));
                SoundsRead::default()
            }),
            field: Arc::new(|_, _, _| Err("no field".to_string())),
        };
        let mut worker = ThreadWorker::start(tasks);
        let mut backlog = Vec::new();
        for n in 0..THREADS {
            worker.decode(&format!("block{n}"), true);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while started.lock().expect("log").len() < THREADS {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        // Every thread is stuck; the music asked for in advance is queued first, the sounds after it.
        worker.decode("later", false);
        worker.load_sounds(vec![18, 27]);
        gate.store(1, SeqCst);
        wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "later"), "the music asked for in advance");
        let order = started.lock().expect("log").clone();
        let at = |n: &str| order.iter().position(|x| x == n).expect("it ran");
        assert!(at("sounds [18, 27]") < at("later"), "{order:?}");
    }

    #[test]
    fn a_panicking_task_is_an_error_for_that_job_and_the_threads_carry_on() {
        let tasks = Tasks {
            voice: no_voice(),
            battle_ambience: None,
            analyze: Arc::new(|_| panic!("a broken script")),
            decode: Arc::new(|name| if name == "boom" { panic!("a broken decoder") } else { Ok(tracks(5)) }),
            cues: Arc::new(|| panic!("a broken sound file")),
            sounds: Arc::new(|_| panic!("a broken sound file")),
            field: Arc::new(|_, _, _| panic!("a broken field")),
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
        worker.load_sounds(vec![7, 8]);
        let sounds = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Sounds { .. }), "the battle sounds");
        assert!(matches!(sounds, Done::Sounds { read, .. } if read.clips.is_empty() && read.missing.iter().map(|(id, _)| *id).eq([7, 8])));
        worker.scan_field("anywhere", Vec::new(), Vec::new());
        let field = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Field { .. }), "the field's sounds");
        assert!(matches!(field, Done::Field { read: Err(_), .. }));
        worker.decode("fine", true);
        let fine = wait_for(&mut worker, &mut backlog, |d| matches!(d, Done::Track { name, .. } if name == "fine"), "a decode after the panics");
        assert!(matches!(fine, Done::Track { result: Ok(_), .. }));
    }

    #[test]
    fn dropping_the_worker_stops_its_threads() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        let tasks = Tasks {
            voice: no_voice(),
            battle_ambience: None,
            analyze: Arc::new(move |_| {
                counter.fetch_add(1, SeqCst);
                AreaMusic::default()
            }),
            decode: Arc::new(|_| Ok(tracks(1))),
            cues: Arc::new(|| Err("none".into())),
            sounds: Arc::new(|_| SoundsRead::default()),
            field: Arc::new(|_, _, _| Err("no field".to_string())),
        };
        let worker = ThreadWorker::start(tasks);
        drop(worker);
        // A job queued after the drop (through a stale handle) cannot exist; what matters is that the
        // drop returned at once and the pool did not run anything meanwhile.
        assert_eq!(ran.load(SeqCst), 0);
    }
}
