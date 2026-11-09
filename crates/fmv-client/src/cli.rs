//! Command line interface, plus the small pure helpers behind it (tint parsing, name-hash tint,
//! server address normalisation, duration validation).

use crate::battle::ScriptStep;
use crate::ff7::{Ff7Labels, Ff7Source};
use clap::{Parser, ValueEnum};
use fmv_core::AvatarAppearance;
use fmv_library::LibraryError;
use fmv_protocol::DEFAULT_DEV_PORT;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Process exit codes (documented in `--help`).
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Runtime failure: cannot connect, handshake failed, server rejected us, connection lost,
    /// window or GPU start-up failed, or there is no usable account (nothing saved for the server
    /// and no `--name` to register one, several saved and none chosen, an unusable token file).
    pub const FAILURE: u8 = 1;
    // Exit code 2 is reserved by clap for command-line usage errors.
    /// Local content is missing or does not match the server's area (CannotJoin was sent), or the
    /// requested game's content cannot be opened locally (no library, no usable set, no archive).
    pub const CONTENT: u8 = 3;
}

const AFTER_HELP: &str = "\
EXIT CODES:
  0  success
  1  runtime failure (connect/handshake/server reject/connection lost/window or GPU start-up, or no
     usable account: nothing saved for the server and no --name to register one)
  2  command-line usage error
  3  local content missing or not matching the server's area, or the game's content cannot be opened

ACCOUNTS:
  The server knows you by an account. The first run with --name registers one: the server issues a
  token ONCE, and this client saves it at once, before anything else can fail, to
  <state dir>/accounts/<server>.token (mode 0600). Later runs log in with the saved token and need
  no --name (a window opens the character menu to choose, see below). The state dir is
  --state-dir, else $FMV_STATE_DIR/client, else ~/.local/share/ff-multiverse/client. A second
  account on the same server gets its own file (<server>.<name>.token); when several are saved,
  pass --name to choose one.
  --register creates a NEW account with --name and replaces the token saved for that name: use it
  when the server's accounts were reset (\"invalid or unknown token\"). The token is a password: it
  is never printed, except in the error shown when it could not be saved (so it is not lost).
  After registering, the server decides your name, tint and look from your stored profile.

CONTENT:
  --game fixture (default) uses the synthetic FIXTURE area, NOT FF7.
  --game ff7 reads YOUR OWN FF7 data, read-only, and never receives game files from the server:
  it compares the area's fingerprint with its own copy before joining. Default source: the library
  fmv-import wrote under $FMV_STATE_DIR (else the user data directory); --ff7-library names another
  state directory, --ff7-set picks a set when it holds several, --ff7-flevel reads one flevel.lgp
  directly. Passing any --ff7-* source flag selects --game ff7. The fingerprint binds edition,
  region and revision: the server and every client must hold the same copy.

HEADLESS OUTPUT:
  stdout carries exactly one line:
    SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n|-1> party=<n> battles=<n> victories=<n> area_changes=<n> level=<n> exp=<n> items=<n>
  <n> in name= is the display name percent-encoded: letters, digits and - . _ ~ are kept, every
  other byte becomes %XX (\"Red XIII\" prints as Red%20XIII), so the line always splits on spaces.
  party=<n> is the LARGEST party this client was in at any moment of the run (0: never in one).
  battles=<n> is how many shared battles this client entered, victories=<n> how many the server
  ended in victory. A bot whose replay of a battle ever disagreed with the server exits 1.
  area_changes=<n> is how many times the server moved this client on to another area. level=<n> and
  exp=<n> are the level and total experience of this player's character as the server last reported
  them (0 and 0 when it reports none: a server without battle data, such as the FIXTURE one, has no
  characters); every report is also logged on stderr as a `progress:` line. items=<n> is how many
  items this player's account carries as the server last reported it (the counts of all stacks
  added up, 0 when it reports none); every report is also logged on stderr as a
  `battle: inventory: stacks=<n> total=<n> items=<item>:<count>,...` line (item ids, never names).
  New keys are only ever added at the end of the line.
  A server that goes silent for 10 s is treated as gone (exit code 1, the summary is still printed).
  Party events and refusals are logged on stderr, one `party:` line each.
  --party-invite-first-peer invites the visible player with the lowest entity id (a refused invite
  is retried, at most 5 times, 1 s apart); --party-accept accepts every invite sent to this player.
  --battle-auto plays battles: whenever its gauge is full it casts its cheapest healing spell on
  itself if its HP is under 30% and it has the MP, otherwise gives its limit break if the limit
  gauge is full, and otherwise attacks the first living enemy; it never runs unless a script says
  so. With --debug-encounters it also asks the server for one battle (see BATTLES). Battle events
  and refusals are logged on stderr as `battle:` lines; when a battle ends the line says whether
  this machine replayed it from its own battle data and matched (`verified: ...`) or could not
  (`NOT verified: ...`), and the closing `headless: battles:` line counts the verified ones and
  the commands of this player the server applied, by kind:
  `(<n> attack, <n> magic, <n> item, <n> defend, <n> limit, <n> escape)`. A --battle-auto bot that
  is in a battle when --duration runs out plays it to the end (at most 12 s longer).
  --battle-script LIST (with --battle-auto) makes the bot give these commands first, in order, one
  per full ATB gauge, and only then play as above. Entries, separated by commas: attack, defend,
  escape, limit, magic:<spell id> and item:<item id> (the game's own ids: Cure is spell 0, Potion
  is item 0, Phoenix Down is item 7), for example --battle-script defend,item:0,magic:0,limit.
  An offensive spell, item or limit, and attack, aim at the first living enemy; a healing spell,
  item or limit at the party member with the least HP left (the bot itself on a tie); a reviving
  one at a fallen member. `magic:<id>:all` and `item:<id>:all` aim at the whole side instead (every
  enemy, or the whole party for a healing one: magic:0:all is Cure on the party), which costs the
  once-per-battle All materia use of a Cure or Fire kind of spell; what only ever hits a whole side
  (Megalixir, item:6; Ultima, magic:52; Aerith's Healing Wind, limit) needs no `:all`. An entry
  this character cannot give at that moment (not enough MP, none
  of the item left, the limit gauge is not full, a battle that cannot be escaped, nobody has
  fallen) is logged as a `battle: script: ... skipped: <why>` line, counted and skipped for good,
  and the next entry is tried at once. An entry is used up when the server applies (or refuses)
  it; an escape can fail, so list it more than once. The script goes on across battles, and the
  closing `headless: battle script: entries=<n> applied=<n> skipped=<n> refused=<n> lost=<n>
  unused=<n>` line says how it went.

BATTLES:
  A battle starts for a whole party (random encounters in a field that has a table, or on request
  when the server runs with --debug-encounters). You are frozen in the field until it ends; the
  client replays the battle from YOUR OWN scene.bin and kernel.bin (only ids and seeds are sent)
  and shouts `BATTLE DIVERGENCE` if its replay ever disagrees with the server. Without battle data
  in your library you can only wait the battle out.
  Your character is your own: the server keeps its level, experience, AP and gil per account (the
  character is the one your look names, Cloud unless you chose another), fights you as it at that
  level, pays it when a battle is won and tells you (and when you join) what it has now; the window
  shows it under the area name, for instance `Cloud Lv 7  EXP 626/949  Gil 20`.

CHARACTER MENU:
  A window started without --name opens on a character menu instead of logging in at once; with
  --name the menu is skipped and the run behaves as before, and --headless never shows it. The
  menu lists the characters saved for the server (name, colour, look). Up/Down choose, Enter
  plays, N creates a new one, Del forgets a saved login on THIS computer (it asks first; the
  character stays on the server, but you can only log in as it again if you still have its
  token), Esc quits. The create screen takes a name (up to 24 characters), a look (FF7: Cloud,
  Tifa, Barret or Aerith) and a tint (a palette plus red, green and blue sliders); Tab or Up/Down
  move between the fields and Enter registers. When the server refuses (name taken, registration
  closed, rate limited) the reason is shown on screen and you can try again.

CONTROLLER:
  A game controller works in the character menu and in the game (through the system's gamepad
  support, so Steam Input's virtual controller counts like any other). With none connected nothing
  changes; one can be plugged in or out at any time, and only the window with the focus is
  listened to. A controller makes the same key events as the keyboard: left stick or D-pad = the
  arrows (walk, move through the menus; a held direction repeats in menus and battles); A = Enter;
  B = Backspace (back a level in a battle), and Esc in the character menu (back, cancel; it never
  quits); X = P (invite the nearest player), and in the character menu a new character or, on the
  create form, Backspace; Y = Y (accept an invite), and in the character menu forget a saved login
  (it asks first); Select = L (leave the party); Start = Esc (in the field a second press quits; in
  a battle it only backs out of the menu; in the character menu it backs out or quits); right
  bumper (R1/RB) = B (ask for a debug battle); left trigger (L2/LT), held = Shift (walk instead of
  run).

FULL SCREEN AND THE ON-SCREEN KEYBOARD:
  --fullscreen opens the window as borderless full screen. Steam's Game Mode does it by itself: the
  window is full screen when GAMESCOPE_WAYLAND_DISPLAY is set, or XDG_CURRENT_DESKTOP lists
  gamescope, or SteamGamepadUI is 1. An ordinary desktop session stays windowed. Inside a
  gamescope session (the first two checks) a text field that gets the focus (the name of a new
  character) asks Steam to show its on-screen keyboard (steam://open/keyboard, handed over with
  xdg-open) and lets it go (steam://close/keyboard) when the field loses the focus; the keyboard
  types ordinary key events. On a desktop nothing is ever asked of Steam. The `device:` line on
  stderr at start-up says what was seen and what it led to.

WINDOW CONTROLS:
  WASD / arrow keys move, Esc twice quits (never during a battle: close the window to quit
  there). P invites the nearest other player to your party, Y accepts
  the newest pending invite, L leaves your party. The window shows player names over their
  markers, your party in the top right corner, the area name and the connection status; a FIXTURE
  area is labelled \"FIXTURE (synthetic, not FF7)\". The window title shows your party size and a
  pending invite; party members are drawn with a gold ring. B asks the server for a debug battle
  (refused unless it runs with --debug-encounters). A battle is drawn as a 3D scene with FF7-style
  windows: the enemy list, your party's HP, MP and ATB gauges, and on your turn a command menu
  (Attack, Magic, Item, Defend; the Magic list holds the spells your character knows and the Item
  list the items your account carries). In a battle the arrows (or WASD) move the menu and the target
  pointer, Space or Enter selects, Backspace or Esc goes back (Esc never quits in a battle).
  FMV_BATTLE_FLAT=1 draws the battle without the 3D scene.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BotKind {
    /// Walk a circle around the spawn point.
    Circle,
    /// Walk back and forth along a line through the spawn point.
    Line,
    /// Stand still.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GameChoice {
    /// Synthetic FIXTURE content. NOT FF7.
    Fixture,
    /// Synthetic FIXTURE rooms (`hall`, `yard`): several connected areas, to try walking between
    /// areas without owned data (needs --name or --headless; the character menu is not for it).
    /// NOT FF7.
    FixtureRooms,
    /// Real FF7 content from this machine's own copy (see --ff7-library / --ff7-flevel).
    Ff7,
}

#[derive(Debug, Parser)]
#[command(
    name = "fmv-client",
    version,
    about = "FF Multiverse client: connect to a server, join its area and walk around with other players",
    after_help = AFTER_HELP
)]
pub struct Args {
    /// Server address as host:port (the port defaults to the development port when omitted).
    #[arg(long, value_name = "HOST:PORT", default_value = DEFAULT_SERVER, value_parser = normalize_server)]
    pub server: String,

    /// Display name shown to other players (at most 24 characters). Needed to register an
    /// account from the command line; without it a window opens the character menu (choose or
    /// create a character) and --headless uses the account saved for the server.
    #[arg(long, value_name = "NAME", value_parser = parse_name)]
    pub name: Option<String>,

    /// Create a NEW account with --name, replacing the token saved for that name (use it when the
    /// server's accounts were reset and the saved token is no longer known).
    #[arg(long, requires = "name")]
    pub register: bool,

    /// Where this client keeps its state, the saved account tokens (in `accounts/`). Default:
    /// $FMV_STATE_DIR/client, else the user data directory's ff-multiverse/client.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Player tint as rrggbb hex (default: derived from the name). Only used when registering:
    /// after that the server takes it from the stored profile.
    #[arg(long, value_name = "RRGGBB", value_parser = parse_tint)]
    pub tint: Option<[u8; 3]>,

    /// Open the window as borderless full screen (no title bar, no frame). Steam's Game Mode does
    /// this by itself, see FULL SCREEN AND THE ON-SCREEN KEYBOARD below; this asks for it anywhere.
    /// A headless run has no window.
    #[arg(long, conflicts_with = "headless")]
    pub fullscreen: bool,

    /// Run without a window as a scripted bot and print one SMOKE summary line.
    #[arg(long)]
    pub headless: bool,

    /// Bot movement pattern (headless only).
    #[arg(long, value_enum, default_value_t = BotKind::Circle, requires = "headless")]
    pub bot: BotKind,

    /// How long the bot stays in the area, in seconds (headless only).
    #[arg(long, value_name = "SECS", default_value = "10", value_parser = parse_duration_secs, requires = "headless")]
    pub duration: Duration,

    /// Invite the visible player with the lowest entity id to a party, as soon as there is one
    /// (headless only). A refused invite is retried, at most 5 times, 1 s apart.
    #[arg(long, requires = "headless")]
    pub party_invite_first_peer: bool,

    /// Accept every party invite sent to this player (headless only).
    #[arg(long, requires = "headless")]
    pub party_accept: bool,

    /// Which game's content to use.
    #[arg(long, value_enum, default_value_t = GameChoice::Fixture)]
    pub game: GameChoice,

    /// Owned FF7 library: the state directory fmv-import wrote (default: $FMV_STATE_DIR, else the
    /// user data directory). Selects `--game ff7`.
    #[arg(long, value_name = "STATE_DIR", conflicts_with = "ff7_flevel")]
    pub ff7_library: Option<PathBuf>,

    /// Library set to read (default: the only FF7 PC set). Selects `--game ff7`.
    #[arg(long, value_name = "SET_ID", conflicts_with = "ff7_flevel")]
    pub ff7_set: Option<String>,

    /// Read this flevel.lgp directly (read-only) instead of the library. Selects `--game ff7`.
    #[arg(long, value_name = "PATH")]
    pub ff7_flevel: Option<PathBuf>,

    /// Edition label for --ff7-flevel (default `unknown`). It is part of the area fingerprint:
    /// give the label of the server's library set (`fmv-import list` shows it).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    pub ff7_edition: Option<String>,

    /// Region label for --ff7-flevel (default `unknown`; part of the area fingerprint).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    pub ff7_region: Option<String>,

    /// Language label for --ff7-flevel (default `unknown`).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    pub ff7_language: Option<String>,

    /// Revision label for --ff7-flevel (default `unknown`; part of the area fingerprint).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    pub ff7_revision: Option<String>,

    // ---- battles (protocol v3): one contiguous block ----
    /// Play battles as a bot: whenever this player's ATB gauge is full, cast its cheapest healing
    /// spell on itself if its HP is under 30% and it has the MP, else give its limit break if the
    /// limit gauge is full, else attack the first living enemy; it never runs unless --battle-script
    /// says so (headless only). Battles start from random encounters, or with --debug-encounters.
    #[arg(long, requires = "headless")]
    pub battle_auto: bool,

    /// With --battle-auto: the commands to give first, in order, one per full ATB gauge, before
    /// playing as usual. Entries, separated by commas: attack, defend, escape, limit, magic:<spell
    /// id>, item:<item id> (Cure is spell 0, Potion is item 0), for example
    /// `defend,item:0,magic:0,limit`. An entry the character cannot give at that moment (no MP, none
    /// of the item, the limit gauge not full, a battle that cannot be escaped) is logged, skipped and
    /// counted; an escape can fail, so list it more than once.
    #[arg(long, value_name = "STEP[,STEP...]", value_delimiter = ',', value_parser = parse_script_step, requires = "battle_auto")]
    pub battle_script: Vec<ScriptStep>,

    /// With --battle-auto: also ask the server for a debug encounter, once (the server must run
    /// with --debug-encounters; a refusal is logged and not retried).
    #[arg(long, requires = "battle_auto")]
    pub debug_encounters: bool,

    /// With --debug-encounters: wait until this player's party has at least N members, and only
    /// then (as its leader) ask, so one request starts one battle for the whole party.
    #[arg(long, value_name = "N", default_value_t = 1, requires = "debug_encounters")]
    pub battle_wait_party: usize,

    // ---- area travel: one contiguous block ----
    /// Walk through the exits to these areas, in order, as a bot (headless only): for each area
    /// walk to the exit of the current area that leads there, cross it and wait for the server to
    /// move this player, then go on to the next one. Several areas are separated by commas
    /// (`--travel md1_1,md1stin`: there and back). A bot with --party-invite-first-peer or
    /// --party-accept holds still until its party has two members (at most 12 s); in every area
    /// but the last it also waits to see another player (at most 8 s) and a moment longer, so they
    /// see it too, before it moves on. Bots without --travel never cross an exit by accident.
    #[arg(long, value_name = "AREA[,AREA...]", value_delimiter = ',', value_parser = parse_area_key, requires = "headless")]
    pub travel: Vec<String>,

    // ---- story (phasing): one contiguous block ----
    /// Advance this player's story as a bot (headless only): walk to the story point of the area it
    /// stands in that would advance THIS account's story (the same points and rule the server uses; in
    /// FF7's first field `md1stin` it is walkmesh triangle 12), stay there until the server has moved the
    /// story, then stand still. The server decides: the bot only goes where the data says the story
    /// happens. With --party-invite-first-peer it first waits (at most 15 s) until its invite is out.
    /// Never crosses an exit. Run the server with --no-random-encounters (or give the bot --battle-auto):
    /// a bot caught in a battle without it stands frozen until the run ends.
    #[arg(long, requires = "headless")]
    pub story_advance: bool,

    /// With --party-accept: do not accept an invite until a player this bot had seen is no longer in
    /// view (that other player left its instance: advanced its story, or went away), instead of at once.
    /// This is how a smoke test joins a party across a phase split.
    #[arg(long, requires = "party_accept")]
    pub party_accept_after_split: bool,
}

/// Default server for `--server`. Kept in sync with `DEFAULT_DEV_PORT` by a unit test.
const DEFAULT_SERVER: &str = "127.0.0.1:47770";

impl Args {
    /// The tint to send in `Hello`: the explicit `--tint`, or one derived from `name`. (Only a
    /// registration uses it; the server ignores it when logging in with a token.)
    pub fn effective_tint(&self, name: &str) -> [u8; 3] {
        self.tint.unwrap_or_else(|| tint_from_name(name))
    }

    /// This client's state directory (see [`client_state_dir`]).
    pub fn client_state_dir(&self) -> Result<PathBuf, String> {
        client_state_dir(self.state_dir.as_deref())
    }

    /// Does this run open on the character menu? A window without `--name` does: the player
    /// chooses or creates a character there. `--name` (log in as, or register, that name) keeps
    /// the command-line behaviour, and `--headless` never shows a menu.
    pub fn wants_menu(&self) -> bool {
        !self.headless && self.name.is_none()
    }

    /// The game whose content to use. Any `--ff7-*` source flag asks for FF7, even next to
    /// `--game fixture`: synthetic content is never used when real content was asked for.
    pub fn effective_game(&self) -> GameChoice {
        let wants_ff7 = self.ff7_library.is_some() || self.ff7_set.is_some() || self.ff7_flevel.is_some();
        if wants_ff7 {
            GameChoice::Ff7
        } else {
            self.game
        }
    }

    /// Where this machine's FF7 field archive comes from (only meaningful for FF7).
    pub fn ff7_source(&self) -> Ff7Source {
        match &self.ff7_flevel {
            Some(path) => {
                let unknown = Ff7Labels::default();
                let label = |given: &Option<String>, default: String| given.clone().unwrap_or(default);
                Ff7Source::Flevel {
                    path: path.clone(),
                    labels: Ff7Labels {
                        edition: label(&self.ff7_edition, unknown.edition),
                        region: label(&self.ff7_region, unknown.region),
                        language: label(&self.ff7_language, unknown.language),
                        revision: label(&self.ff7_revision, unknown.revision),
                    },
                }
            }
            None => Ff7Source::Library { state_dir: self.ff7_library.clone(), set: self.ff7_set.clone() },
        }
    }
}

/// The client's state directory: `--state-dir` if given, else `<state dir>/client` where the state
/// dir is what `fmv_library::resolve_state_dir` finds (`$FMV_STATE_DIR`, else
/// `~/.local/share/ff-multiverse`).
pub fn client_state_dir(explicit: Option<&Path>) -> Result<PathBuf, String> {
    client_state_dir_from(explicit, || fmv_library::resolve_state_dir(None))
}

/// [`client_state_dir`] with the library's default state directory injected, so the precedence
/// rules can be tested without touching the environment.
pub fn client_state_dir_from(
    explicit: Option<&Path>,
    default_state_dir: impl FnOnce() -> Result<PathBuf, LibraryError>,
) -> Result<PathBuf, String> {
    if let Some(dir) = explicit {
        if dir.as_os_str().is_empty() {
            return Err("--state-dir needs a non-empty directory".to_string());
        }
        return Ok(dir.to_path_buf());
    }
    default_state_dir()
        .map(|dir| dir.join("client"))
        .map_err(|e| format!("cannot find the state directory ({e}); pass --state-dir or set $FMV_STATE_DIR"))
}

/// Parse `rrggbb` (an optional leading `#` is accepted).
pub fn parse_tint(s: &str) -> Result<[u8; 3], String> {
    let t = s.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("`{s}` is not a tint: expected 6 hex digits like ff8800"));
    }
    // All six bytes are ASCII hex digits, so slicing on byte offsets is on char boundaries.
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| e.to_string());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// Deterministic, random-looking, bright tint derived from a name hash (FNV-1a -> hue).
pub fn tint_from_name(name: &str) -> [u8; 3] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Mix the high bits down so short, similar names still spread over the hue wheel.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    let hue = (h % 360) as f32;
    hsv_to_rgb(hue, 0.65, 0.95)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h6 = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to_u8(r), to_u8(g), to_u8(b)]
}

/// Validate and clean the display name (same rules the server applies).
pub fn parse_name(s: &str) -> Result<String, String> {
    AvatarAppearance::sanitize_name(s)
        .ok_or_else(|| "the display name is empty after removing control characters".to_string())
}

/// An area key for `--travel`: what adapters use as keys (ASCII letters, digits, `_` and `-`, at
/// most 32 characters), so it can be compared and printed as it is.
pub fn parse_area_key(s: &str) -> Result<String, String> {
    let key = s.trim();
    if key.is_empty() || key.len() > 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err(format!("`{}` is not an area key (ASCII letters, digits, `_` and `-`, at most 32 characters)", clean_for_error(s)));
    }
    Ok(key.to_string())
}

/// One entry of `--battle-script` (`attack`, `defend`, `escape`, `limit`, `magic:<id>[:all]`, `item:<id>[:all]`).
pub fn parse_script_step(s: &str) -> Result<ScriptStep, String> {
    s.parse()
}

/// Text from the command line made safe to put into an error message.
fn clean_for_error(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(40).collect()
}

/// Seconds, finite and within a day. Fractions are allowed (`0.5`).
pub fn parse_duration_secs(s: &str) -> Result<Duration, String> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a number of seconds"))?;
    if !v.is_finite() || !(0.0..=86_400.0).contains(&v) {
        return Err(format!("`{s}` is out of range: expected 0..=86400 seconds"));
    }
    Ok(Duration::from_secs_f64(v))
}

/// Accept `host:port`, `ip:port`, `[v6]:port`, a bare ip or a bare host (default dev port).
pub fn normalize_server(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("the server address is empty".into());
    }
    if s.parse::<SocketAddr>().is_ok() {
        return Ok(s.to_string());
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_DEV_PORT).to_string());
    }
    let bad_host = |h: &str| h.is_empty() || h.chars().any(|c| c.is_whitespace() || c == '/' || c == '@');
    match s.rsplit_once(':') {
        Some((host, port)) => {
            if bad_host(host) || host.contains(':') {
                return Err(format!("`{s}` is not a valid host:port"));
            }
            match port.parse::<u16>() {
                Ok(p) if p != 0 => Ok(s.to_string()),
                _ => Err(format!("`{s}` has an invalid port `{port}`")),
            }
        }
        None if bad_host(s) => Err(format!("`{s}` is not a valid host")),
        None => Ok(format!("{s}:{DEFAULT_DEV_PORT}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn tint_parses_plain_and_hash_prefixed_hex() {
        assert_eq!(parse_tint("ff8800"), Ok([0xff, 0x88, 0x00]));
        assert_eq!(parse_tint("#00AaFf"), Ok([0x00, 0xaa, 0xff]));
        assert_eq!(parse_tint("  123456 "), Ok([0x12, 0x34, 0x56]));
    }

    #[test]
    fn tint_rejects_bad_input_without_panicking() {
        for bad in [
            "",
            "fff",
            "ff88000",
            "gg0000",
            "ff 800",
            "#12345",
            "##ff880",
            "ééé",
            "ff88é0",
            "\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}",
            "+f8800",
            "-f8800",
        ] {
            assert!(parse_tint(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn name_hash_tint_is_deterministic_bright_and_varied() {
        assert_eq!(tint_from_name("cloud"), tint_from_name("cloud"));
        let names = [
            "a", "b", "c", "bot1", "bot2", "cloud", "tifa", "barret", "aerith", "red xiii",
        ];
        let tints: std::collections::HashSet<[u8; 3]> = names.iter().map(|n| tint_from_name(n)).collect();
        assert!(
            tints.len() >= 8,
            "names should spread over many tints, got {}",
            tints.len()
        );
        for t in tints {
            assert!(t.iter().copied().max().unwrap() >= 200, "tint {t:?} should be bright");
        }
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv_to_rgb(359.9, 0.0, 0.5), [128, 128, 128]);
    }

    #[test]
    fn duration_validation() {
        assert_eq!(parse_duration_secs("10"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration_secs("0.5"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration_secs("0"), Ok(Duration::ZERO));
        for bad in ["-1", "nan", "inf", "1e300", "86401", "abc", ""] {
            assert!(parse_duration_secs(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn server_address_normalisation() {
        assert_eq!(normalize_server("127.0.0.1:47770").unwrap(), "127.0.0.1:47770");
        assert_eq!(normalize_server("example.test:9000").unwrap(), "example.test:9000");
        assert_eq!(
            normalize_server("example.test").unwrap(),
            format!("example.test:{DEFAULT_DEV_PORT}")
        );
        assert_eq!(
            normalize_server("10.0.0.5").unwrap(),
            format!("10.0.0.5:{DEFAULT_DEV_PORT}")
        );
        assert_eq!(normalize_server("[::1]:1234").unwrap(), "[::1]:1234");
        assert_eq!(normalize_server("::1").unwrap(), format!("[::1]:{DEFAULT_DEV_PORT}"));
        for bad in [
            "",
            "  ",
            "host:",
            "host:0",
            "host:99999",
            ":80",
            "a b:80",
            "http://x:80",
            "a:b:80",
        ] {
            assert!(normalize_server(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn default_server_matches_protocol_dev_port() {
        assert_eq!(DEFAULT_SERVER, format!("127.0.0.1:{DEFAULT_DEV_PORT}"));
    }

    #[test]
    fn cli_defaults_and_requirements() {
        let a = Args::try_parse_from(["fmv-client", "--name", "Cloud"]).unwrap();
        assert_eq!(a.server, DEFAULT_SERVER);
        assert_eq!(a.name.as_deref(), Some("Cloud"));
        assert!(!a.headless && !a.register);
        assert_eq!((a.party_invite_first_peer, a.party_accept), (false, false));
        assert_eq!(a.state_dir, None);
        assert_eq!(a.bot, BotKind::Circle);
        assert_eq!(a.duration, Duration::from_secs(10));
        assert_eq!(a.game, GameChoice::Fixture);
        assert_eq!(a.effective_tint("Cloud"), tint_from_name("Cloud"));

        let a = Args::try_parse_from([
            "fmv-client",
            "--name",
            "x",
            "--tint",
            "#102030",
            "--headless",
            "--bot",
            "line",
            "--duration",
            "2.5",
        ])
        .unwrap();
        assert_eq!(a.effective_tint("x"), [0x10, 0x20, 0x30]);
        assert_eq!(a.bot, BotKind::Line);
        assert_eq!(a.duration, Duration::from_millis(2500));

        // --bot/--duration need --headless; bad values are usage errors.
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--bot", "idle"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--duration", "3"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--tint", "nothex"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "\u{7}\u{7}"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--game", "ff7"]).is_ok());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--game", "ff8"]).is_err());
    }

    /// `--name` is only needed to register: a saved account logs in without it, so clap no longer
    /// insists on it (the login plan reports a missing name when one is needed).
    #[test]
    fn name_is_optional_but_register_needs_it() {
        let a = Args::try_parse_from(["fmv-client"]).unwrap();
        assert_eq!(a.name, None);
        assert!(Args::try_parse_from(["fmv-client", "--headless"]).is_ok());
        assert!(Args::try_parse_from(["fmv-client", "--register"]).is_err(), "--register needs --name");
        let a = Args::try_parse_from(["fmv-client", "--register", "--name", "Cloud"]).unwrap();
        assert!(a.register);
        // The name is cleaned like the server cleans it.
        let a = Args::try_parse_from(["fmv-client", "--name", "  Cl\u{200b}oud\n "]).unwrap();
        assert_eq!(a.name.as_deref(), Some("Cloud"));
        // Without a name the tint (only used for a registration) is derived from what is given.
        let a = Args::try_parse_from(["fmv-client", "--tint", "010203"]).unwrap();
        assert_eq!(a.effective_tint("anything"), [1, 2, 3]);
    }

    #[test]
    fn party_bot_flags_are_headless_only() {
        for flag in ["--party-invite-first-peer", "--party-accept"] {
            assert!(Args::try_parse_from(["fmv-client", "--name", "x", flag]).is_err(), "{flag} needs --headless");
            let a = Args::try_parse_from(["fmv-client", "--name", "x", "--headless", flag]).unwrap();
            assert!(a.party_invite_first_peer || a.party_accept);
        }
        let a = Args::try_parse_from(["fmv-client", "--headless", "--party-invite-first-peer", "--party-accept"]).unwrap();
        assert!(a.party_invite_first_peer && a.party_accept);
    }

    #[test]
    fn travel_takes_a_comma_separated_route_of_area_keys_and_is_headless_only() {
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--travel", "md1_1"]).is_err(), "--travel needs --headless");
        let a = Args::try_parse_from(["fmv-client", "--name", "x", "--headless", "--travel", "md1_1,md1stin"]).unwrap();
        assert_eq!(a.travel, ["md1_1", "md1stin"]);
        let a = Args::try_parse_from(["fmv-client", "--name", "x", "--headless", "--travel", "md1_1"]).unwrap();
        assert_eq!(a.travel, ["md1_1"]);
        let a = Args::try_parse_from(["fmv-client", "--name", "x", "--headless"]).unwrap();
        assert!(a.travel.is_empty(), "no route unless asked for");
        // Keys are what adapters use: a bad one is refused before anything runs.
        for bad in ["", "md1 1", "../x", "a,,b", "x".repeat(33).as_str(), "caf\u{e9}", "line\nbreak"] {
            assert!(
                Args::try_parse_from(["fmv-client", "--name", "x", "--headless", "--travel", bad]).is_err(),
                "{bad:?} is not an area key"
            );
        }
        // The synthetic rooms (several areas, no owned data) are a game of their own.
        let a = Args::try_parse_from(["fmv-client", "--name", "x", "--game", "fixture-rooms"]).unwrap();
        assert_eq!(a.effective_game(), GameChoice::FixtureRooms);
        let a = Args::try_parse_from(["fmv-client", "--name", "x", "--game", "fixture-rooms", "--ff7-library", "/x"]).unwrap();
        assert_eq!(a.effective_game(), GameChoice::Ff7, "real content asked for wins, as for the plain fixture");
        assert_eq!(parse_area_key(" md1_1 "), Ok("md1_1".to_string()), "surrounding blanks are not part of the key");
        let err = parse_area_key("bad\u{1b}[2Jkey").unwrap_err();
        assert!(!err.chars().any(char::is_control), "the message is safe to print: {err:?}");
    }

    #[test]
    fn the_client_state_dir_is_the_flag_else_the_library_default_plus_client() {
        use fmv_library::resolve_state_dir_from;
        let env = |v: &str| Some(std::ffi::OsString::from(v));
        let explicit = PathBuf::from("/tmp/my-state");
        // The flag wins, and is used as is.
        let default = || resolve_state_dir_from(None, env("/ignored"), None);
        assert_eq!(client_state_dir_from(Some(&explicit), default), Ok(explicit.clone()));
        // Otherwise: whatever the library resolves ($FMV_STATE_DIR, else the data dir) + `client`.
        let from_env = || resolve_state_dir_from(None, env("/var/fmv"), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(client_state_dir_from(None, from_env), Ok(PathBuf::from("/var/fmv/client")));
        let from_data = || resolve_state_dir_from(None, None, Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(client_state_dir_from(None, from_data), Ok(PathBuf::from("/home/u/.local/share/ff-multiverse/client")));
        // Nothing to go on: an error that says what to do, not a panic.
        let nothing = || resolve_state_dir_from(None, None, None);
        let err = client_state_dir_from(None, nothing).unwrap_err();
        assert!(err.contains("--state-dir") && err.contains("FMV_STATE_DIR"), "{err}");
        // An empty --state-dir is refused rather than meaning "the current directory".
        assert!(client_state_dir_from(Some(Path::new("")), || resolve_state_dir_from(None, env("/x"), None)).is_err());
        // Through the parser.
        let a = Args::try_parse_from(["fmv-client", "--state-dir", "/tmp/s", "--name", "x"]).unwrap();
        assert_eq!(a.client_state_dir(), Ok(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn the_help_documents_accounts_party_flags_and_the_extended_smoke_line() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        for needle in [
            "--state-dir",
            "--register",
            "--party-invite-first-peer",
            "--party-accept",
            "ACCOUNTS:",
            "rtt_ms=<n|-1> party=<n>",
            "level=<n> exp=<n> items=<n>",
            "New keys are only ever added at the end of the line.",
            "LARGEST party",
            "P invites the nearest other player",
            "Y accepts",
            "L leaves",
            "invalid or unknown token",
        ] {
            assert!(help.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
    }

    #[test]
    fn battle_script_takes_comma_separated_commands_and_needs_battle_auto() {
        use crate::battle::ScriptStep::{Attack, Defend, Escape, Item, Limit, Magic};
        let parse = |extra: &[&str]| Args::try_parse_from(["fmv-client", "--headless", "--battle-auto"].into_iter().chain(extra.iter().copied()));
        let a = parse(&["--battle-script", "defend,item:0,magic:0,limit,escape,attack"]).unwrap();
        assert_eq!(a.battle_script, [Defend, Item(0), Magic(0), Limit, Escape, Attack]);
        assert_eq!(parse(&["--battle-script", "Magic:27"]).unwrap().battle_script, [Magic(27)], "case does not matter");
        // Whole sides (protocol v8): `:all` after the id of a spell or an item.
        let a = parse(&["--battle-script", "magic:0:all,item:6:all,magic:27"]).unwrap();
        assert_eq!(a.battle_script, [crate::battle::ScriptStep::MagicAll(0), crate::battle::ScriptStep::ItemAll(6), Magic(27)]);
        assert!(parse(&["--battle-script", "magic:0:everyone"]).is_err() && parse(&["--battle-script", "limit:all"]).is_err());
        assert!(parse(&[]).unwrap().battle_script.is_empty(), "no script unless asked for");
        // Given twice, the lists add up.
        let a = parse(&["--battle-script", "defend", "--battle-script", "escape,escape"]).unwrap();
        assert_eq!(a.battle_script, [Defend, Escape, Escape]);
        // It is a bot's flag: it needs --battle-auto, and so --headless.
        assert!(Args::try_parse_from(["fmv-client", "--headless", "--battle-script", "attack"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--battle-auto", "--battle-script", "attack"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--battle-script", "attack"]).is_err());
        // A bad entry is a usage error before anything runs, and the message says what is expected.
        for bad in ["run", "defend,", ",defend", "magic", "magic:", "item:x", "item:70000", "limit:3", "", "defend,fire"] {
            assert!(parse(&["--battle-script", bad]).is_err(), "{bad:?} is not a script");
        }
        let err = parse(&["--battle-script", "defend,run"]).unwrap_err().to_string();
        assert!(err.contains("magic:<spell id>") && err.contains("run"), "{err}");
    }

    #[test]
    fn the_help_documents_the_battle_script_and_the_breakdown_of_the_applied_commands() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--battle-script",
            "attack, defend, escape, limit, magic:<spell id> and item:<item id>",
            "defend,item:0,magic:0,limit",
            "Cure is spell 0, Potion is item 0",
            "one per full ATB gauge",
            "skipped for good",
            "(<n> attack, <n> magic, <n> item, <n> defend, <n> limit, <n> escape)",
            "headless: battle script: entries=<n> applied=<n> skipped=<n> refused=<n> lost=<n> unused=<n>",
            "it never runs unless a script says so",
        ] {
            assert!(flat.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
    }

    #[test]
    fn a_window_without_a_name_opens_the_character_menu_and_nothing_else_does() {
        let wants = |args: &[&str]| {
            Args::try_parse_from(std::iter::once("fmv-client").chain(args.iter().copied())).unwrap().wants_menu()
        };
        assert!(wants(&[]));
        assert!(wants(&["--server", "10.0.0.1:1"]));
        assert!(wants(&["--game", "ff7"]));
        assert!(wants(&["--tint", "ff8800"]), "a tint alone does not name a character");
        // --name keeps the command-line behaviour (log in as, or register, that name).
        assert!(!wants(&["--name", "Cloud"]));
        assert!(!wants(&["--name", "Cloud", "--register"]));
        // A headless run never shows a menu.
        assert!(!wants(&["--headless"]));
        assert!(!wants(&["--headless", "--name", "x"]));
    }

    #[test]
    fn the_help_describes_the_character_menu_and_the_hud() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        for needle in [
            "CHARACTER MENU:",
            "N creates a new one",
            "Del forgets a saved login on THIS computer",
            "the menu is skipped",
            "--headless never shows it",
            "name taken, registration closed, rate limited",
            "player names over their",
            "FIXTURE (synthetic, not FF7)",
        ] {
            let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(flat.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
    }

    #[test]
    fn fullscreen_is_off_by_default_and_has_no_meaning_for_a_headless_run() {
        assert!(!Args::try_parse_from(["fmv-client"]).unwrap().fullscreen);
        assert!(!Args::try_parse_from(["fmv-client", "--name", "x"]).unwrap().fullscreen);
        assert!(Args::try_parse_from(["fmv-client", "--fullscreen"]).unwrap().fullscreen);
        assert!(Args::try_parse_from(["fmv-client", "--fullscreen", "--name", "x", "--game", "ff7"]).unwrap().fullscreen);
        assert!(Args::try_parse_from(["fmv-client", "--fullscreen", "--headless"]).is_err(), "a headless run has no window");
        // It does not change whether a window opens on the character menu.
        assert!(Args::try_parse_from(["fmv-client", "--fullscreen"]).unwrap().wants_menu());
    }

    #[test]
    fn the_help_documents_the_controller_full_screen_and_the_keyboard() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--fullscreen",
            "CONTROLLER:",
            "left stick or D-pad = the arrows",
            "A = Enter",
            "B = Backspace",
            "X = P (invite",
            "Y = Y (accept",
            "Select = L (leave the party)",
            "Start = Esc",
            "right bumper (R1/RB) = B",
            "left trigger (L2/LT), held = Shift",
            "FULL SCREEN AND THE ON-SCREEN KEYBOARD:",
            "GAMESCOPE_WAYLAND_DISPLAY",
            "XDG_CURRENT_DESKTOP lists gamescope",
            "SteamGamepadUI is 1",
            "steam://open/keyboard",
            "steam://close/keyboard",
            "On a desktop nothing is ever asked of Steam",
        ] {
            assert!(flat.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
    }

    fn parse(extra: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(["fmv-client", "--name", "x"].into_iter().chain(extra.iter().copied()))
    }

    #[test]
    fn ff7_source_flags_select_the_game_and_the_source() {
        assert_eq!(parse(&[]).unwrap().effective_game(), GameChoice::Fixture);
        assert_eq!(parse(&["--game", "ff7"]).unwrap().effective_game(), GameChoice::Ff7);
        for flags in [vec!["--ff7-library", "/x"], vec!["--ff7-set", "s"], vec!["--ff7-flevel", "/y/flevel.lgp"]] {
            assert_eq!(parse(&flags).unwrap().effective_game(), GameChoice::Ff7, "{flags:?}");
        }
        // An FF7 source flag wins over an explicit --game fixture.
        assert_eq!(parse(&["--game", "fixture", "--ff7-library", "/x"]).unwrap().effective_game(), GameChoice::Ff7);

        assert_eq!(parse(&["--game", "ff7"]).unwrap().ff7_source(), Ff7Source::Library { state_dir: None, set: None });
        assert_eq!(
            parse(&["--ff7-library", "/state", "--ff7-set", "ff7-x"]).unwrap().ff7_source(),
            Ff7Source::Library { state_dir: Some(PathBuf::from("/state")), set: Some("ff7-x".into()) }
        );
        match parse(&["--ff7-flevel", "/y/flevel.lgp", "--ff7-edition", "Steam", "--ff7-revision", "r1"]).unwrap().ff7_source() {
            Ff7Source::Flevel { path, labels } => {
                assert_eq!(path, PathBuf::from("/y/flevel.lgp"));
                assert_eq!(
                    labels,
                    Ff7Labels { edition: "Steam".into(), region: "unknown".into(), language: "unknown".into(), revision: "r1".into() }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn contradictory_ff7_flags_are_usage_errors() {
        assert!(parse(&["--ff7-library", "/x", "--ff7-flevel", "/y"]).is_err());
        assert!(parse(&["--ff7-set", "s", "--ff7-flevel", "/y"]).is_err());
        // Labels only make sense for a direct archive, and clap would otherwise ignore them next
        // to a library flag (a required argument that conflicts with a present one is waived).
        for label in ["--ff7-edition", "--ff7-region", "--ff7-language", "--ff7-revision"] {
            assert!(parse(&[label, "x"]).is_err(), "{label} needs --ff7-flevel");
            assert!(parse(&["--ff7-library", "/x", label, "x"]).is_err(), "{label} needs --ff7-flevel");
            assert!(parse(&["--ff7-set", "s", label, "x"]).is_err(), "{label} needs --ff7-flevel");
            assert!(parse(&["--ff7-flevel", "/y", label, "x"]).is_ok(), "{label}");
        }
    }
}
