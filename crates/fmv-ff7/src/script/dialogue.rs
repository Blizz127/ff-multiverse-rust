//! Talking to a field character: run its talk script far enough to collect the message windows it
//! opens, with the field's own dialog text, exactly as written.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08; other editions are unverified.**
//!
//! # Which script is "talk"
//!
//! Slot 1 of an entity ([`TALK_SCRIPT`], Makou Reactor's `S1 - Talk`) runs when the player presses
//! the action button next to it. In the owned data 1 040 of the 2 412 character entities (a `CHAR`
//! that is no `PC`) open a message directly in slot 1 and 42 more only request other scripts; the
//! rest have a bare `RET` (a door, a bystander, a character who only speaks in a cutscene: nothing
//! in `md1_1` has talk text at the start of the game, its AVALANCHE members speak in scripts 3 and
//! 4 that the field's director starts). [`Walk::script`] runs any slot, so those cutscene lines are
//! reachable too.
//!
//! # What is interpreted
//!
//! The walker is a small interpreter of the opcodes that decide *which messages show, in what order*
//! and ignores the rest (movement, animation, sound, the camera, palettes, menus and money never
//! change what is said):
//!
//! * `MESSAGE` and `ASK` (windows [`Message`], the dialog text from the field's own table). `ASK`
//!   waits for an answer and stores it in its variable.
//! * Control flow: `JMPF`/`JMPFL`/`JMPB`/`JMPBL`, the conditionals `IFUB`/`IFSW`/`IFUW` (and their
//!   long forms) with all eleven operators, `IFPRTYQ`/`IFMEMBQ`, `IFKEY*` (no key is ever held),
//!   `RET`, `RETTO`, and the requests of other entities' scripts. The engine runs scripts side by
//!   side; this walker runs one at a time, so `REQEW` (request and wait for the end) runs the
//!   requested script **to its end** before the requester goes on, and `REQ` and `REQSW` (which
//!   wait for nothing, or only for the start) run it **as far as its first loop**: many scripts
//!   so requested idle in a loop for ever, and a request into one must not hang the walk (16 of
//!   the 2 412 characters of the owned data ended in the step budget when they were run to
//!   their end). A window such a script opens comes before the requester's next one. `PREQ`,
//!   `PRQSW` and `PRQEW` (a party member's script) are ignored.
//! * Variables: `SETBYTE`/`SETWORD`, `BITON`/`BITOFF`/`BITXOR`, `PLUS`/`MINUS`/`MUL`/`DIV`/`MOD`,
//!   `AND`/`OR`/`XOR` and their word forms, `INC`/`DEC` and their word forms, the saturating `!`
//!   forms, `RANDOM`, `LBYTE`/`HBYTE`/`2BYTE` and `GETPC`. Reads come from the writes the walk made
//!   itself, else from the [`StoryVars`] hook.
//! * `TLKON` and `VISI` only in the one-pass prelude of [`availability`].
//! * The script **ends** at `RET` (the last frame), `MAPJUMP`, `BATTLE`, `MINIGAME` and `GAMEOVER`
//!   ([`End`]): the walker never changes the world, so a conversation that leads somewhere just stops.
//!
//! **Bounded:** at most [`Limits::steps`] instructions, [`Limits::beats`] windows and
//! [`Limits::depth`] nested requests per walk, so a loop (`JMPB 0` waiting for an event that never
//! comes) ends in [`End::Budget`] rather than hanging, and a damaged script ends in
//! [`End::Broken`]. Nothing here panics on any bytes (the unit tests feed it mutated and random
//! sections).
//!
//! # Answers to `ASK`
//!
//! `ASK window dialog first last var`: the question is the text of the *last page* of the dialog
//! (a page break restarts the line count), the choices are its lines `first..=last` (counted from 0)
//! and the answer stored in `var` is the **absolute line number** of the choice, not an index from
//! `first` (measured on the owned data, over the 908 `ASK`s of the distinct scripts: 465 test their
//! answer against line numbers only, 5 against indexes from `first` only, 20 both and 418 in no way
//! that tells; 387 of them `SETBYTE` the variable to the line they want the cursor on just before,
//! so the cursor starts on the line the variable holds when that is one of the choices and on
//! `first` otherwise). The line count is *not* the `{CHOICE}` markers: 251 of the 908 have none in
//! front of some choice line (they indent with tabs, or are a debug menu), and 20 name a last line
//! the text does not have (clamped here). `tests/dialogue_real.rs` repeats the measurements.
//!
//! # The story hook
//!
//! Branches that read story variables read them through [`StoryVars`]. [`StartOfGame`] answers for a
//! new game (every variable 0, Cloud and Barret in the party); a server-fed implementation
//! that mirrors a real player's progress replaces it without touching the walker.
//!
//! # Origins
//!
//! Every [`Message`] says where it came from ([`MessageOrigin`]): the field's own script
//! ([`MessageOrigin::Canon`]) or a line this project adds on top ([`MessageOrigin::Added`]), which
//! never passes for canon. [`MessageSource`] is what a window pulls its messages from; the walker
//! is one source ([`TalkSession`]) and an added content layer can be another.
//!
//! # Gaps
//!
//! * Only the talked-to entity's own scripts (and what they `REQ`) run; the other entities' init
//!   scripts do not, so a variable they would set before the player arrives stays at its hook value.
//! * Writes a conversation makes are not kept for the next one.
//! * `WINDOW`/`WSIZW` (position and size), `WSPCL` (transparent windows), `MPARA`/`WNUMB` (numbers in
//!   a window) and the timing codes of the text are ignored; `{VAR}`-type codes show `0`.
//! * The names of the nine playable characters are constants ([`CHARACTER_NAMES`]); the game keeps
//!   its defaults in the kernel data, which is not read here.
//! * `IFKEY`-type waits, `PRQEW` and other party-member requests and the opcodes the data never
//!   uses are not interpreted.

mod pieces;
mod walk;

#[cfg(test)]
mod tests;

pub use pieces::{parse_message, Piece, TextColour, CHARACTER_NAMES};
pub use walk::{availability, Availability, Limits, TalkError, TalkSession, Walk, EFFECT_NOTE_LAYER};

use super::FieldScript;

/// What the story says about the walker's world: the one place a talk script's branches learn how
/// far the player is. The default [`StartOfGame`] stands for a new game.
pub trait StoryVars {
    /// The savemap variable `address` of bank `bank` (1..=15) read as one byte. A word is the
    /// byte at `address` and the one after it (little endian), so only bytes are asked for.
    fn byte(&self, bank: u8, address: u8) -> u8;

    /// The playable character (0 = Cloud, 1 = Barret, ... 8 = Cid) standing in party slot `slot`
    /// (0..=2), if any.
    fn party_member(&self, slot: u8) -> Option<u8>;

    /// Whether character `id` is in the party right now.
    fn in_party(&self, id: u8) -> bool {
        (0..3).any(|slot| self.party_member(slot) == Some(id))
    }

    /// Whether character `id` can be called a member of the team at all (`IFMEMBQ`).
    fn is_member(&self, id: u8) -> bool {
        self.in_party(id)
    }
}

/// A new game: every variable is 0 and the party is Cloud and Barret (the Bombing Mission).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartOfGame;

impl StoryVars for StartOfGame {
    fn byte(&self, _bank: u8, _address: u8) -> u8 {
        0
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        match slot {
            0 => Some(0),
            1 => Some(1),
            _ => None,
        }
    }
}

/// Where a message comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageOrigin {
    /// A line of the field's own script and dialog table, shown exactly as written. `entity` is
    /// the entity whose script opened the window and `dialog` the index in the field's table.
    Canon { entity: usize, dialog: usize },
    /// A line this project adds on top of the game: never canon, and a window may show it as such.
    /// `layer` names the content layer.
    Added { layer: String },
}

impl MessageOrigin {
    /// Whether the line is the game's own.
    pub fn is_canon(&self) -> bool {
        matches!(self, Self::Canon { .. })
    }
}

/// The choices of an `ASK`: lines `first..=last` of the message's last page (counted from 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub first: usize,
    pub last: usize,
    /// The line the cursor starts on (one of `first..=last`).
    pub cursor: usize,
}

/// One message window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub origin: MessageOrigin,
    /// The window number the script asked for (a script may keep several open; a client that
    /// shows one at a time can ignore it).
    pub window: u8,
    pub pieces: Vec<Piece>,
    /// Set for an `ASK`: the player picks a line and the answer goes back to the source.
    pub choice: Option<Choice>,
}

impl Message {
    /// A message of the given origin.
    pub fn new(origin: MessageOrigin, pieces: Vec<Piece>) -> Self {
        Self { origin, window: 0, pieces, choice: None }
    }

    /// The text of each page, line by line, without colours and choice markers (for logs and
    /// tests; a window works from [`Message::pieces`]).
    pub fn page_lines(&self) -> Vec<Vec<String>> {
        let mut pages: Vec<Vec<String>> = vec![vec![String::new()]];
        for piece in &self.pieces {
            match piece {
                Piece::Text(t) => {
                    if let Some(line) = pages.last_mut().and_then(|p| p.last_mut()) {
                        line.push_str(t);
                    }
                }
                Piece::NewLine => {
                    if let Some(page) = pages.last_mut() {
                        page.push(String::new());
                    }
                }
                Piece::NewPage => pages.push(vec![String::new()]),
                Piece::Choice | Piece::Colour(_) => {}
            }
        }
        pages
    }
}

/// How a conversation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The script returned.
    Finished,
    /// The script ran past its instruction budget (a wait that never ends).
    Budget,
    /// The script could not be decoded any further.
    Broken,
    /// The script moves the party to another field (not performed).
    MapJump { field: u16 },
    /// The script starts a battle (not performed).
    Battle,
    /// The script starts a minigame (not performed).
    MiniGame,
    /// The script is the game over.
    GameOver,
}

/// What a conversation gives next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Beat {
    /// Show this window and wait for the player.
    Message(Message),
    /// Nothing more to say.
    End(End),
}

/// Where a window gets its messages: the field's script ([`TalkSession`]), or an added layer.
pub trait MessageSource {
    /// The next beat. `answer` is the player's answer to the previous message when it asked: the
    /// absolute line number of the choice, as a script stores it (anything else is clamped into
    /// the choices; `None` takes the cursor's starting line).
    fn next(&mut self, answer: Option<usize>) -> Beat;
}

/// Talk radius of a character whose script 0 never says (`TALKR`): the most usual of the values
/// the owned data sets (70 and 80 in 87 of the 228 scripts that set one), in field units at scale
/// 512. The engine's own default was not found in any source this module could reach.
pub const DEFAULT_TALK_RADIUS: u8 = 80;

/// One character entity of a field, as its script 0 declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct Character {
    pub entity: usize,
    /// The entity's name in the script (a label, not an identifier: 54 of 1 134 are empty).
    pub name: String,
    /// Every literal `(x, y)` its script 0 places it at, in any part and on any branch.
    pub places: Vec<(i16, i16)>,
    /// How close the player has to be to talk, in walkmesh units: `TALKR` (else
    /// [`DEFAULT_TALK_RADIUS`]) times the field scale over 512.
    pub talk_radius: f32,
}

/// The characters of a field that are not players, in entity order.
pub fn characters(script: &FieldScript<'_>) -> Vec<Character> {
    let scale = f32::from(script.header().scale.max(1)) / 512.0;
    script
        .entity_inits(None)
        .into_iter()
        .filter(|init| init.is_character() && !init.is_player())
        .map(|init| {
            let radius = init.talk_radius.and_then(|t| t.value.literal()).unwrap_or(DEFAULT_TALK_RADIUS);
            Character {
                entity: init.entity,
                name: init.name.clone(),
                places: init.placements.iter().filter_map(|p| p.xy()).collect(),
                talk_radius: f32::from(radius) * scale,
            }
        })
        .collect()
}

/// How near (in walkmesh units) a place has to be to count as where a character stands.
const SAME_PLACE: f32 = 1.5;

/// The character that stands at `(x, y)` under `name`: the one with that name and a literal
/// placement within a unit and a half, the nearest if several (the first in entity order among
/// equally near ones: the owned data has eight announced NPCs that are identical characters stacked
/// on one spot with the same empty name, and which of them is talked to does not matter); failing
/// that the only character of that name (a character that has been moved). Names are labels (empty
/// ones and shared ones exist), so the position decides whenever it can. Every one of the 1 134 NPCs
/// the owned data announces is found.
pub fn find_character<'a>(chars: &'a [Character], name: &str, x: f32, y: f32) -> Option<&'a Character> {
    let near = |c: &Character| {
        c.places
            .iter()
            .map(|&(px, py)| (f32::from(px) - x).hypot(f32::from(py) - y))
            .filter(|d| d.is_finite())
            .fold(f32::INFINITY, f32::min)
    };
    let by_place = chars
        .iter()
        .filter(|c| c.name == name)
        .map(|c| (near(c), c))
        .filter(|(d, _)| *d <= SAME_PLACE)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c);
    by_place.or_else(|| {
        let mut same_name = chars.iter().filter(|c| c.name == name && !name.is_empty());
        match (same_name.next(), same_name.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    })
}
