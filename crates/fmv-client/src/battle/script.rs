//! The headless bot's script, `--battle-script`: commands it gives, in order, one per full ATB gauge, before
//! it falls back to its normal rule ([`BattleClient::bot_command`]: heal when hurt, give a full limit, attack).
//!
//! Why it exists: the normal rule only ever attacks (it heals below 30% HP, which the first enemies never
//! bring about, and a limit gauge fills by about 12 of 255 in a battle), so Magic, Item, Defend, Limit and
//! Escape were verified in-process only. A script lets a smoke run fire each of them over real processes
//! (`scripts/battle-smoke.sh --commands`).
//!
//! # The list
//!
//! `attack`, `defend`, `escape`, `limit`, `magic:<spell id>` and `item:<item id>`, separated by commas (the
//! game's own ids: Cure is spell 0, Potion is item 0, Phoenix Down is item 7). The targets are chosen like
//! this: an offensive spell, item or limit, and `attack`, go to the first living enemy; a healing spell, item
//! or limit goes to the party member with the least HP left (by fraction; the bot's own character on a tie,
//! then the lowest actor id); a reviving spell or item (Life, Phoenix Down) goes to the first fallen member,
//! and is skipped when nobody has fallen.
//!
//! **Whole sides** (protocol v8): `magic:<id>:all` and `item:<id>:all` aim the spell or the item at the whole
//! side instead (every enemy for an offensive one, the whole party for a healing one: `magic:0:all` is Cure on
//! the party, `magic:27:all` Fire on every enemy), which costs the caster one use of its All materia for a spell
//! of the Cure and Fire kind (and is skipped like any other refused entry when it has none left, or when the
//! spell can only hit one target). What can only hit a whole side needs no `:all`: `item:6` is Megalixir on the
//! party, `magic:52` Ultima on every enemy, and `limit` is Healing Wind on the party for Aerith.
//!
//! # How an entry is used
//!
//! * One entry per full gauge, in order, before the bot's normal rule takes over for good. The script goes on
//!   across battles: an entry that was not reached when a battle ended is the next one of the next battle.
//! * An entry the engine refuses for our character at this moment (not enough MP, a spell it does not know, an
//!   item it cannot use or none of it left, a limit gauge that is not full, a battle that cannot be escaped,
//!   nobody to aim at) is **logged, skipped for good and counted**, never retried: the next entry is tried at
//!   once, in the same turn, so that a refused entry costs no turn. This is how "one per full gauge" reads: a
//!   gauge is spent by a command that is sent, not by one that was skipped.
//! * An entry is used up when the server APPLIES it (it echoes the command in its event) or refuses it (a
//!   `BattleRejected`), not when it is sent. A command that got no answer for [`super::PENDING_TIMEOUT_S`] is
//!   sent again, and it is the SAME entry that is sent, so a slow server cannot make the script lose one: if
//!   the first copy is applied after all, the entry is used up and the duplicate that the server refuses
//!   afterwards skips nothing.
//! * Whatever the script has not used when the run ends is reported as `unused` in the closing
//!   `headless: battle script:` line, next to how many were applied, skipped, refused by the server and lost.

use super::{BattleClient, BattleCmd};
use fmv_ff7_battle::rules::{Aim, BattleState, CommandError, Combatant, Target};
use fmv_protocol::battle::Command;
use fmv_protocol::ActorId;
use std::fmt;
use std::str::FromStr;

/// One entry of a script (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptStep {
    /// The plain attack, on the first living enemy.
    Attack,
    Defend,
    Escape,
    /// The character's limit break (its gauge must be full).
    Limit,
    /// A spell by the game's own id, on one target (a spell that only hits a whole side hits it).
    Magic(u16),
    /// A spell on the whole side (`magic:<id>:all`).
    MagicAll(u16),
    /// An item by the game's own id (the account must carry one), on one target (an item that only hits a whole
    /// side hits it).
    Item(u16),
    /// An item on the whole side (`item:<id>:all`).
    ItemAll(u16),
}

/// What a script is made of, for messages.
pub const SYNTAX: &str = "attack, defend, escape, limit, magic:<spell id>[:all] or item:<item id>[:all]";

/// Text from the command line made safe to put into a message.
fn shown(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(40).collect()
}

impl FromStr for ScriptStep {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let word = s.trim().to_ascii_lowercase();
        let (name, rest) = match word.split_once(':') {
            Some((name, rest)) => (name.trim(), Some(rest.trim())),
            None => (word.as_str(), None),
        };
        // `<id>` or `<id>:all` (only a spell and an item have the second form).
        let (id, all) = match rest.and_then(|rest| rest.split_once(':')) {
            Some((id, flag)) if matches!(name, "magic" | "item") => (Some(id.trim()), Some(flag.trim())),
            _ => (rest, None),
        };
        if let Some(flag) = all {
            if flag != "all" {
                return Err(format!("`{}`: after the id only `:all` can follow, not `{}`", shown(s), shown(flag)));
            }
        }
        let id_of = || -> Result<u16, String> {
            match id {
                None | Some("") => Err(format!("`{}` needs an id: {name}:<id>", shown(s))),
                Some(digits) if digits.bytes().all(|b| b.is_ascii_digit()) => {
                    digits.parse().map_err(|_| format!("`{}`: the id is too big (at most 65535)", shown(s)))
                }
                Some(other) => Err(format!("`{}`: `{}` is not an id (a whole number)", shown(s), shown(other))),
            }
        };
        match name {
            "magic" if all.is_some() => id_of().map(ScriptStep::MagicAll),
            "item" if all.is_some() => id_of().map(ScriptStep::ItemAll),
            "magic" => id_of().map(ScriptStep::Magic),
            "item" => id_of().map(ScriptStep::Item),
            "attack" | "defend" | "escape" | "limit" if id.is_some() => Err(format!("`{}`: {name} takes no id", shown(s))),
            "attack" => Ok(ScriptStep::Attack),
            "defend" => Ok(ScriptStep::Defend),
            "escape" => Ok(ScriptStep::Escape),
            "limit" => Ok(ScriptStep::Limit),
            _ => Err(format!("`{}` is not a battle command: expected {SYNTAX}", shown(s))),
        }
    }
}

impl fmt::Display for ScriptStep {
    /// The syntax of `--battle-script` (what [`ScriptStep::from_str`] reads).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptStep::Attack => f.write_str("attack"),
            ScriptStep::Defend => f.write_str("defend"),
            ScriptStep::Escape => f.write_str("escape"),
            ScriptStep::Limit => f.write_str("limit"),
            ScriptStep::Magic(id) => write!(f, "magic:{id}"),
            ScriptStep::MagicAll(id) => write!(f, "magic:{id}:all"),
            ScriptStep::Item(id) => write!(f, "item:{id}"),
            ScriptStep::ItemAll(id) => write!(f, "item:{id}:all"),
        }
    }
}

/// What one script entry comes to for our character at this moment (see [`BattleClient::scripted_command`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scripted {
    /// It is not our turn to command (no battle or no replay, our gauge is not full, a command of ours is in
    /// flight): wait.
    NotNow,
    /// Send this.
    Send(BattleCmd),
    /// The entry cannot be given here and now, and why.
    Skip(String),
}

/// The first living enemy.
fn first_living_enemy(state: &BattleState) -> Option<ActorId> {
    state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(Combatant::is_alive))
}

/// The living party member with the least HP left by fraction; on a tie `me`, then the lowest actor id.
fn most_hurt_ally(state: &BattleState, me: ActorId) -> Option<ActorId> {
    state
        .party_ids()
        .into_iter()
        .filter_map(|id| state.actor(id).filter(|c| c.is_alive()).map(|c| (id, c)))
        .min_by_key(|(id, c)| (u64::from(c.hp) * 1000 / u64::from(c.max_hp.max(1)), *id != me, id.0))
        .map(|(id, _)| id)
}

/// The first fallen party member.
fn first_fallen_ally(state: &BattleState) -> Option<ActorId> {
    state.party_ids().into_iter().find(|id| state.actor(*id).is_some_and(|c| !c.is_alive()))
}

/// The whole side an action aims at: the party for a restoring one, every enemy for a damaging one.
fn side_of(allies: bool) -> Target {
    if allies {
        Target::AllAllies
    } else {
        Target::AllEnemies
    }
}

/// What the engine's refusal says, in this script's words.
fn refusal_text(error: CommandError) -> String {
    match error {
        CommandError::NotEnoughMp(_) => "not enough MP".to_string(),
        CommandError::UnknownSpell(id) => format!("this character does not know spell {id}"),
        CommandError::UnknownItem(id) => format!("item {id} cannot be used in battle"),
        CommandError::NoLimit(_) => "this character has no limit break".to_string(),
        CommandError::LimitNotReady(_) => "the limit gauge is not full".to_string(),
        CommandError::CannotEscape => "this battle cannot be escaped".to_string(),
        CommandError::InvalidReach(_) => "it cannot be aimed that way (one target, or a whole side)".to_string(),
        CommandError::NoAllLeft(_) => "no All materia use is left this battle".to_string(),
        CommandError::NoTargets(_) => "there is nobody to aim at".to_string(),
        CommandError::InvalidTarget(_) | CommandError::UnknownTarget(_) | CommandError::TargetDead(_) | CommandError::TargetNotDead(_) => {
            "there is no valid target".to_string()
        }
        other => other.to_string(),
    }
}

impl BattleClient {
    /// What script entry `step` comes to for our own character right now: [`Scripted::NotNow`] when it is not
    /// our turn (as [`BattleClient::bot_command`] judges it), the request to send when the engine accepts it
    /// (the engine's own check, [`BattleState::check_command`], with the target chosen as the module docs say
    /// and the account's items counted from the inventory the server last told), or why it cannot be given.
    pub fn scripted_command(&self, now: f64, step: ScriptStep) -> Scripted {
        let Some((active, state, mine)) = self.my_turn(now) else { return Scripted::NotNow };
        let command = match self.command_for_step(state, mine, step) {
            Ok(command) => command,
            Err(why) => return Scripted::Skip(why),
        };
        match state.check_command(mine, &command) {
            Ok(()) => Scripted::Send(BattleCmd::Command { battle_id: active.id, actor: mine, command }),
            Err(e) => Scripted::Skip(refusal_text(e)),
        }
    }

    /// The command `step` means for `mine` in `state`, or why there is none (no target, no such spell, ...).
    /// What the engine itself would refuse (MP, the limit gauge, escaping) is left to `check_command`.
    fn command_for_step(&self, state: &BattleState, mine: ActorId, step: ScriptStep) -> Result<Command, String> {
        let enemy = || first_living_enemy(state).ok_or_else(|| "there is no living enemy to aim at".to_string());
        let hurt = || most_hurt_ally(state, mine).ok_or_else(|| "there is nobody to aim at".to_string());
        let fallen = || first_fallen_ally(state).ok_or_else(|| "nobody has fallen".to_string());
        match step {
            ScriptStep::Attack => Ok(Command::Attack { target: enemy()? }),
            ScriptStep::Defend => Ok(Command::Defend),
            ScriptStep::Escape => Ok(Command::Escape),
            ScriptStep::Limit => {
                let info = state.limit_for(mine).ok_or_else(|| "this character has no limit break".to_string())?;
                let target = match (info.aim, info.targets_allies) {
                    // Healing Wind: the whole party, and nothing smaller.
                    (Aim::Side, allies) => side_of(allies),
                    (_, true) => Target::One(hurt()?),
                    (_, false) => Target::One(enemy()?),
                };
                Ok(Command::Limit { target })
            }
            ScriptStep::Magic(spell) | ScriptStep::MagicAll(spell) => {
                let known = state
                    .actor(mine)
                    .and_then(|me| me.spells.iter().find(|s| s.index == spell))
                    .ok_or_else(|| format!("this character does not know spell {spell}"))?;
                // What can only hit a whole side hits it; the rest does only when `:all` says so.
                let whole = matches!(step, ScriptStep::MagicAll(_)) || known.spec.aim == Aim::Side;
                let target = match (known.spec.heals(), known.spec.hits_dead) {
                    _ if whole => side_of(known.spec.heals()),
                    (true, true) => Target::One(fallen()?),
                    (true, false) => Target::One(hurt()?),
                    (false, _) => Target::One(enemy()?),
                };
                Ok(Command::Magic { spell, target })
            }
            ScriptStep::Item(item) | ScriptStep::ItemAll(item) => {
                let info = state.item_info(item).ok_or_else(|| format!("item {item} cannot be used in battle"))?;
                if !self.inventory.iter().any(|stack| stack.item == item && stack.count > 0) {
                    return Err(format!("none of item {item} is left"));
                }
                let whole = matches!(step, ScriptStep::ItemAll(_)) || info.aim == Aim::Side;
                let target = match (info.revives, info.targets_allies) {
                    _ if whole => side_of(info.targets_allies),
                    (true, _) => Target::One(fallen()?),
                    (false, true) => Target::One(hurt()?),
                    (false, false) => Target::One(enemy()?),
                };
                Ok(Command::Item { item, target })
            }
        }
    }
}

/// A command that is on its way and has not been answered: which entry it is, and the counters the answer
/// will move.
#[derive(Debug, Clone, Copy)]
struct Inflight {
    index: usize,
    applied_before: u64,
    rejections_before: u64,
}

/// What the script says to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Send this.
    Send(BattleCmd),
    /// Nothing yet: it is not our turn, or the last command has not been answered.
    Wait,
    /// Every entry is used up: the bot's normal rule takes over.
    Done,
}

/// How a script went, for the closing line (every entry is exactly one of the six).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptStats {
    pub entries: usize,
    /// The server applied it.
    pub applied: u64,
    /// The engine refused it for our character (checked before sending): logged and skipped.
    pub skipped: u64,
    /// The server refused it (`BattleRejected`).
    pub refused: u64,
    /// The battle ended before the server answered.
    pub lost: u64,
    /// Not reached when the run ended.
    pub unused: usize,
}

/// The cursor of a script: which entry comes next and what became of the ones before.
#[derive(Debug, Clone)]
pub struct BattleScript {
    steps: Vec<ScriptStep>,
    next: usize,
    inflight: Option<Inflight>,
    applied: u64,
    skipped: u64,
    refused: u64,
    lost: u64,
    notes: Vec<String>,
}

impl BattleScript {
    pub fn new(steps: Vec<ScriptStep>) -> Self {
        Self { steps, next: 0, inflight: None, applied: 0, skipped: 0, refused: 0, lost: 0, notes: Vec::new() }
    }

    /// The entries, in order.
    pub fn steps(&self) -> &[ScriptStep] {
        &self.steps
    }

    /// How the script went so far.
    pub fn stats(&self) -> ScriptStats {
        ScriptStats {
            entries: self.steps.len(),
            applied: self.applied,
            skipped: self.skipped,
            refused: self.refused,
            lost: self.lost,
            unused: self.steps.len() - self.next,
        }
    }

    /// `entries=N applied=N skipped=N refused=N lost=N unused=N` (the closing `headless: battle script:` line
    /// starts with it, and the smoke script reads it by key).
    pub fn summary(&self) -> String {
        let s = self.stats();
        format!("entries={} applied={} skipped={} refused={} lost={} unused={}", s.entries, s.applied, s.skipped, s.refused, s.lost, s.unused)
    }

    /// What the script noted since the last call (sends, skips, refusals), oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn describe(&self, index: usize) -> String {
        let step = self.steps.get(index).map_or_else(|| "?".to_string(), ToString::to_string);
        format!("entry {} of {} ({step})", index + 1, self.steps.len())
    }

    /// Settle the command that is in flight, if the server answered it: applied (the entry is used up),
    /// refused (used up, counted), or the battle ended without an answer (used up, counted as lost).
    pub fn settle(&mut self, battles: &BattleClient) {
        let Some(flight) = self.inflight else { return };
        if battles.my_commands_applied() > flight.applied_before {
            self.applied += 1;
        } else if battles.rejections() > flight.rejections_before {
            self.refused += 1;
            let why = battles.last_rejection().map_or_else(|| "?".to_string(), |r| r.to_string());
            self.notes.push(format!("{} was refused by the server: {why}", self.describe(flight.index)));
        } else if !battles.in_battle() {
            self.lost += 1;
            self.notes.push(format!("{} got no answer before the battle ended", self.describe(flight.index)));
        } else {
            return;
        }
        self.next = flight.index + 1;
        self.inflight = None;
    }

    /// What to do at this moment (see the module docs): settle what was answered, then take the entries in
    /// order, skipping the ones this character cannot give, until one can be sent, it is not our turn or the
    /// script is used up.
    pub fn next_command(&mut self, now: f64, battles: &BattleClient) -> Next {
        self.settle(battles);
        loop {
            let Some(&step) = self.steps.get(self.next) else {
                return if self.inflight.is_some() { Next::Wait } else { Next::Done };
            };
            match battles.scripted_command(now, step) {
                Scripted::NotNow => return Next::Wait,
                Scripted::Send(cmd) => {
                    let resent = self.inflight.is_some_and(|f| f.index == self.next);
                    if !resent {
                        self.inflight = Some(Inflight {
                            index: self.next,
                            applied_before: battles.my_commands_applied(),
                            rejections_before: battles.rejections(),
                        });
                    }
                    let aim = match cmd {
                        BattleCmd::Command { command: Command::Attack { target }, .. } => format!(", aimed at actor {}", target.0),
                        BattleCmd::Command { command: Command::Magic { target, .. } | Command::Item { target, .. } | Command::Limit { target }, .. } => match target {
                            Target::One(target) => format!(", aimed at actor {}", target.0),
                            Target::AllEnemies => ", aimed at every enemy".to_string(),
                            Target::AllAllies => ", aimed at the whole party".to_string(),
                        },
                        _ => String::new(),
                    };
                    self.notes.push(format!("{}: {}{aim}", self.describe(self.next), if resent { "sent again (no answer yet)" } else { "sent" }));
                    return Next::Send(cmd);
                }
                Scripted::Skip(why) => {
                    self.skipped += 1;
                    self.notes.push(format!("{} skipped: {why}", self.describe(self.next)));
                    self.next += 1;
                    self.inflight = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_of_a_script_parse_and_print_back_the_same() {
        for (text, step) in [
            ("attack", ScriptStep::Attack),
            ("defend", ScriptStep::Defend),
            ("escape", ScriptStep::Escape),
            ("limit", ScriptStep::Limit),
            ("magic:0", ScriptStep::Magic(0)),
            ("magic:27", ScriptStep::Magic(27)),
            ("magic:0:all", ScriptStep::MagicAll(0)),
            ("item:0", ScriptStep::Item(0)),
            ("item:65535", ScriptStep::Item(65535)),
            ("item:6:all", ScriptStep::ItemAll(6)),
        ] {
            assert_eq!(text.parse::<ScriptStep>(), Ok(step), "{text}");
            assert_eq!(step.to_string(), text);
        }
        // Case and blanks do not matter.
        assert_eq!("  Magic : 7 ".parse::<ScriptStep>(), Ok(ScriptStep::Magic(7)));
        assert_eq!("ESCAPE".parse::<ScriptStep>(), Ok(ScriptStep::Escape));
        assert_eq!(" Magic : 27 : ALL ".parse::<ScriptStep>(), Ok(ScriptStep::MagicAll(27)));
    }

    #[test]
    fn a_word_that_is_not_a_command_is_refused_with_what_was_expected_and_nothing_unprintable() {
        for bad in ["", "run", "fire", "magic", "magic:", "item:", "magic:x", "magic:-1", "magic:+1", "item:65536", "item:1.5", "attack:1", "limit:128", "magic:1:2", "magic:1:all:all", "magic:1:", "magic::all", "item:2:every", "attack:all", "limit:1:all", "defend:", "item: 3 4"] {
            let err = bad.parse::<ScriptStep>().expect_err(bad);
            assert!(!err.is_empty() && !err.contains('\n'), "{bad:?}: {err}");
        }
        assert!("run".parse::<ScriptStep>().unwrap_err().contains(SYNTAX));
        assert!("magic".parse::<ScriptStep>().unwrap_err().contains("magic:<id>"));
        let err = "bad\u{1b}[2Jword".parse::<ScriptStep>().unwrap_err();
        assert!(!err.chars().any(char::is_control), "the message is safe to print: {err:?}");
    }
}
