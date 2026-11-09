//! The FFNx audio-engine layer config (`config.toml`), the small TOML subset the 7th Heaven sound mods ship.
//!
//! Echo-S and Cosmo Memory do not name their clips after the game's sound ids alone. Each audio layer folder
//! (`sfx`, `music/vgmstream`, `voice`, `ambient`) may hold a `config.toml` whose `[sections]` are the names the
//! game asks for and whose `sequential` / `shuffle` arrays say which clips play instead. Cosmo's `sfx` config has
//! 3 507 sections, every one a `sequential` list (`[6] sequential = [ 6303 ]`: when the game plays sound id 6, play
//! the file `6303.ogg`).
//!
//! Sources, FFNx pinned to commit `4fc392ba5e1bfdd53d39aa6542464379a446083a` (2026-11-08):
//!
//! * [`docs/mods/audio_engine.md`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/docs/mods/audio_engine.md)
//!   and [`misc/FFNx.SFX.toml`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/misc/FFNx.SFX.toml):
//!   the flags (`shuffle`, `sequential`, `loop`, `skip`), "IDs MUST be numbers ONLY", missing files fall back to the
//!   original game effect.
//! * [`src/audio.cpp`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/audio.cpp)
//!   `NxAudioEngine::playSFX` (lines 299 to 375): the section named by the layered name is looked up; a `shuffle`
//!   array picks one entry at random, a `sequential` array the next entry in turn (a per-name counter that starts
//!   again at the end), `skip` plays silence; the picked number names the file `sfx/<number>.<ext>`. Music
//!   (`music.cpp` side, line 679) and voice and ambient (lines 1144 and 1294) do the same with arrays of *strings*.
//!
//! The parser is lenient and bounded, and never panics: a line it does not understand is counted and skipped.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

/// The most text parsed (the largest config in use is 954 KB, the voice probabilities).
pub const MAX_CONFIG_BYTES: usize = 8 * 1024 * 1024;
/// The most sections kept.
pub const MAX_SECTIONS: usize = 100_000;
/// The most entries kept in one array.
pub const MAX_ARRAY: usize = 4096;
/// The most lines one array may span.
const MAX_ARRAY_LINES: usize = 256;

/// One element of a `sequential` or `shuffle` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An integer: the sound effect layer's clip number (`6303` is `6303.ogg`).
    Id(u32),
    /// A string: the music, voice and ambient layers' clip names.
    Name(String),
}

impl Entry {
    /// The file stem this entry names (`6303`, or the string itself).
    pub fn stem(&self) -> String {
        match self {
            Self::Id(n) => n.to_string(),
            Self::Name(s) => s.clone(),
        }
    }
}

/// How a section picks among its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One entry in turn, over and over (the per-name counter of FFNx).
    Sequential,
    /// One entry at random each time.
    Shuffle,
}

/// One `[section]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    pub sequential: Vec<Entry>,
    pub shuffle: Vec<Entry>,
    /// `skip = true`: the sound is silent (FFNx "fakes a match" so that nothing else plays).
    pub skip: bool,
    /// `loop = true/false`, when set.
    pub looped: Option<bool>,
    /// `volume = n` (percent in the voice layer), when set.
    pub volume: Option<f64>,
}

impl Section {
    /// The mode that applies: FFNx applies `shuffle` first and `sequential` after it, so `sequential` wins if both are
    /// given (the files in use never give both).
    pub fn mode(&self) -> Option<Mode> {
        if !self.sequential.is_empty() {
            Some(Mode::Sequential)
        } else if !self.shuffle.is_empty() {
            Some(Mode::Shuffle)
        } else {
            None
        }
    }

    /// The entries of the mode that applies.
    pub fn entries(&self) -> &[Entry] {
        match self.mode() {
            Some(Mode::Sequential) => &self.sequential,
            Some(Mode::Shuffle) => &self.shuffle,
            None => &[],
        }
    }
}

/// A parsed `config.toml`: sections by name (looked up ignoring case).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerConfig {
    sections: HashMap<String, Section>,
    /// Lines (or array items) that were not understood and skipped.
    pub skipped: usize,
}

impl LayerConfig {
    /// Parse `text`. Bounded and total: anything that is not understood is skipped and counted.
    pub fn parse(text: &str) -> Self {
        let text = text.get(..floor_boundary(text, MAX_CONFIG_BYTES)).unwrap_or("");
        let mut config = Self::default();
        let mut current: Option<String> = None;
        let mut lines = text.lines();
        while let Some(raw) = lines.next() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = inner.trim().trim_matches('"').trim_matches('\'').to_ascii_lowercase();
                if config.sections.len() >= MAX_SECTIONS && !config.sections.contains_key(&name) {
                    current = None;
                    config.skipped += 1;
                } else {
                    config.sections.entry(name.clone()).or_default();
                    current = Some(name);
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                config.skipped += 1;
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let mut value = value.trim().to_string();
            // An array that spans lines: gather up to its closing bracket.
            if value.starts_with('[') && !balanced(&value) {
                for _ in 0..MAX_ARRAY_LINES {
                    let Some(more) = lines.next() else { break };
                    value.push(' ');
                    value.push_str(strip_comment(more).trim());
                    if balanced(&value) {
                        break;
                    }
                }
            }
            let Some(name) = current.as_ref() else {
                config.skipped += 1;
                continue;
            };
            let Some(section) = config.sections.get_mut(name) else { continue };
            match key.as_str() {
                "sequential" | "shuffle" => {
                    let (entries, bad) = parse_array(&value);
                    config.skipped += bad;
                    if key == "sequential" {
                        section.sequential = entries;
                    } else {
                        section.shuffle = entries;
                    }
                }
                "skip" => section.skip = value == "true",
                "loop" => section.looped = Some(value == "true"),
                "volume" => section.volume = value.parse::<f64>().ok().filter(|v| v.is_finite()),
                _ => {}
            }
        }
        config
    }

    /// The section called `name` (any case).
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.get(&name.to_ascii_lowercase())
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

/// The largest index at or below `at` that is a character boundary of `text`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// `line` without a trailing `# comment` (a `#` inside quotes is text).
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') => return line.get(..i).unwrap_or(line),
            _ => {}
        }
    }
    line
}

/// Whether the brackets of `value` (outside quotes) close.
fn balanced(value: &str) -> bool {
    let (mut depth, mut quote) = (0i32, None::<char>);
    for c in value.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// The entries of `[ a, b, "c" ]` and how many items were not understood.
fn parse_array(value: &str) -> (Vec<Entry>, usize) {
    let inner = value.trim().strip_prefix('[').map(|v| v.trim_end().strip_suffix(']').unwrap_or(v)).unwrap_or(value);
    let mut entries = Vec::new();
    let mut bad = 0usize;
    let (mut item, mut quote) = (String::new(), None::<char>);
    let flush = |item: &mut String, entries: &mut Vec<Entry>, bad: &mut usize| {
        let t = item.trim();
        if !t.is_empty() {
            if entries.len() >= MAX_ARRAY {
                *bad += 1;
            } else if let Some(s) = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')).or_else(|| t.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))) {
                entries.push(Entry::Name(s.to_string()));
            } else if t.bytes().all(|b| b.is_ascii_digit()) {
                match t.parse::<u32>() {
                    Ok(n) => entries.push(Entry::Id(n)),
                    Err(_) => *bad += 1,
                }
            } else {
                *bad += 1;
            }
        }
        item.clear();
    };
    for c in inner.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                item.push(c);
            }
            (Some(q), c) if c == q => {
                quote = None;
                item.push(c);
            }
            (None, ',') => flush(&mut item, &mut entries, &mut bad),
            _ => item.push(c),
        }
    }
    flush(&mut item, &mut entries, &mut bad);
    (entries, bad)
}

/// The pick FFNx makes from a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Entry(Entry),
    /// `skip = true`: nothing plays.
    Silent,
}

/// FFNx's per-name state: the `sequential` counters and the `shuffle` random numbers. Shared between threads.
#[derive(Debug)]
pub struct Picker {
    state: Mutex<PickerState>,
}

#[derive(Debug)]
struct PickerState {
    next: HashMap<String, usize>,
    rng: u64,
}

impl Picker {
    /// A picker whose `shuffle` choices follow `seed`.
    pub fn new(seed: u64) -> Self {
        Self { state: Mutex::new(PickerState { next: HashMap::new(), rng: seed | 1 }) }
    }

    /// What plays for the section called `name`: `None` when it names nothing to pick (no array, no `skip`).
    pub fn pick(&self, name: &str, section: &Section) -> Option<Pick> {
        if section.skip {
            return Some(Pick::Silent);
        }
        let entries = section.entries();
        if entries.is_empty() {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let index = match section.mode() {
            Some(Mode::Sequential) => {
                let counter = state.next.entry(name.to_ascii_lowercase()).or_insert(0);
                // The counter starts again once it has run past the end (the array may be shorter than before).
                let at = if *counter >= entries.len() { 0 } else { *counter };
                *counter = at + 1;
                at
            }
            _ => {
                let mut x = state.rng;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                state.rng = x;
                (x % entries.len() as u64) as usize
            }
        };
        entries.get(index).cloned().map(Pick::Entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a synthetic config in the shape of the real ones
[6]
sequential = [ 6303 ]

[7]
sequential = [ 10, 11 ,12, 13 ]   # four of them

[Field_1_2_159]
sequential = [
    21,
    22, # the second
    23,
]

[music_a]
shuffle = [ \"alpha\", 'beta', \"gam#ma\" ]

[quiet]
skip = true

[looper]
loop = true
volume = 50
";

    #[test]
    fn parses_ids_names_flags_and_multi_line_arrays() {
        let c = LayerConfig::parse(SAMPLE);
        assert_eq!(c.len(), 6);
        assert_eq!(c.section("6").unwrap().sequential, vec![Entry::Id(6303)]);
        assert_eq!(c.section("7").unwrap().sequential, vec![Entry::Id(10), Entry::Id(11), Entry::Id(12), Entry::Id(13)]);
        assert_eq!(c.section("field_1_2_159").unwrap().sequential, vec![Entry::Id(21), Entry::Id(22), Entry::Id(23)]);
        assert_eq!(
            c.section("MUSIC_A").unwrap().shuffle,
            vec![Entry::Name("alpha".into()), Entry::Name("beta".into()), Entry::Name("gam#ma".into())]
        );
        assert!(c.section("quiet").unwrap().skip);
        let l = c.section("looper").unwrap();
        assert_eq!((l.looped, l.volume), (Some(true), Some(50.0)));
        assert_eq!(l.mode(), None);
        assert!(c.section("nothing").is_none());
        assert_eq!(c.skipped, 0);
    }

    #[test]
    fn unknown_lines_are_counted_not_fatal() {
        let c = LayerConfig::parse("orphan = 1\n[a]\nsequential = [ 1, x, \"s\", 99999999999 ]\nnonsense line\n");
        assert_eq!(c.section("a").unwrap().sequential, vec![Entry::Id(1), Entry::Name("s".into())]);
        assert_eq!(c.skipped, 4);
    }

    #[test]
    fn sequential_wins_when_both_are_given_and_entries_follow_the_mode() {
        let c = LayerConfig::parse("[a]\nshuffle = [1, 2]\nsequential = [3]\n");
        let s = c.section("a").unwrap();
        assert_eq!(s.mode(), Some(Mode::Sequential));
        assert_eq!(s.entries(), &[Entry::Id(3)]);
    }

    #[test]
    fn the_sequential_counter_runs_through_the_array_and_starts_again() {
        let c = LayerConfig::parse(SAMPLE);
        let p = Picker::new(1);
        let s = c.section("7").unwrap();
        let picks: Vec<_> = (0..6).map(|_| p.pick("7", s).unwrap()).collect();
        let ids: Vec<u32> = picks.iter().map(|p| if let Pick::Entry(Entry::Id(n)) = p { *n } else { 0 }).collect();
        assert_eq!(ids, vec![10, 11, 12, 13, 10, 11]);
        // Another name has its own counter.
        assert_eq!(p.pick("6", c.section("6").unwrap()), Some(Pick::Entry(Entry::Id(6303))));
    }

    #[test]
    fn shuffle_stays_inside_the_array_and_follows_its_seed() {
        let c = LayerConfig::parse(SAMPLE);
        let s = c.section("music_a").unwrap();
        let run = |seed| {
            let p = Picker::new(seed);
            (0..40).map(|_| p.pick("music_a", s).unwrap()).collect::<Vec<_>>()
        };
        let a = run(7);
        assert_eq!(a, run(7));
        assert!(a.iter().all(|p| matches!(p, Pick::Entry(Entry::Name(n)) if ["alpha", "beta", "gam#ma"].contains(&n.as_str()))));
        let distinct: std::collections::BTreeSet<_> = a.iter().map(|p| format!("{p:?}")).collect();
        assert!(distinct.len() >= 2, "a shuffle that never varies");
    }

    #[test]
    fn skip_is_silence_and_a_section_without_entries_picks_nothing() {
        let c = LayerConfig::parse(SAMPLE);
        let p = Picker::new(1);
        assert_eq!(p.pick("quiet", c.section("quiet").unwrap()), Some(Pick::Silent));
        assert_eq!(p.pick("looper", c.section("looper").unwrap()), None);
    }

    #[test]
    fn a_comment_marker_inside_quotes_is_text_and_unbalanced_arrays_end_at_the_file() {
        let c = LayerConfig::parse("[a]\nshuffle = [ \"x#y\" ] # note\n[b]\nsequential = [ 1,\n 2,\n");
        assert_eq!(c.section("a").unwrap().shuffle, vec![Entry::Name("x#y".into())]);
        assert_eq!(c.section("b").unwrap().sequential, vec![Entry::Id(1), Entry::Id(2)]);
    }

    #[test]
    fn bounds_hold_for_huge_and_hostile_input() {
        // An array longer than the cap keeps the cap and counts the rest.
        let big = format!("[a]\nsequential = [{}]\n", (0..MAX_ARRAY + 10).map(|n| n.to_string()).collect::<Vec<_>>().join(","));
        let c = LayerConfig::parse(&big);
        assert_eq!(c.section("a").unwrap().sequential.len(), MAX_ARRAY);
        assert_eq!(c.skipped, 10);
        // Text over the cap is cut at a character boundary, never inside one.
        let wide = "é".repeat(MAX_CONFIG_BYTES);
        let _ = LayerConfig::parse(&wide);
        // Random bytes (as lossy text), deterministic.
        let mut state = 0x1234_5678_9ABC_DEF1_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..300 {
            let n = (next() % 400) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| b"[]=,#\"'\n abc019\xC3\xA9"[(next() % 17) as usize]).collect();
            let _ = LayerConfig::parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
