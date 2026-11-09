//! Areas and travel: what the world keeps per area, and how a player gets from one to the next.
//!
//! # Areas
//!
//! The world hosts the area the server was started with (the *entry* area: every player joins
//! there and it is never unloaded) and, when it was given a source (`World::set_area_source`),
//! every other area of the same adapter. Those are loaded the first time a player is sent there and
//! unloaded [`AREA_UNLOAD_GRACE`] after the last player is gone from them (`World::manage_areas`,
//! called on every tick). An area holds what is specific to it: the decoded runtime, the movement
//! rules derived from its speed, its spawn points, exits, NPCs and random-encounter table. Players,
//! parties and battles belong to the world, not to an area: a player stands in exactly one area
//! (or none, in transit) and every broadcast about an area reaches only the players in it.
//!
//! # Exits and travel
//!
//! An area's exits come from `AreaRuntime::exits()`. A move the server applies that crosses an
//! exit line (`Exit::crossing`: strictly from one side of the line to the other or onto it) starts
//! a transfer:
//!
//! 1. The target is loaded if it is not (`ensure_area`). If that fails (the adapter lists it but
//!    cannot load it, or too many areas are loaded) the exit is closed for the life of the area and
//!    the move goes through as an ordinary one: the player walks over the line and nothing happens.
//! 2. The arrival is settled on the target's walkable surface (`AreaRuntime::place_arrival`, which
//!    knows the layer), so the position the client is told is one the target accepts.
//! 3. The player leaves its area (`PlayerLeft` to the others there) and is in transit: it is in no
//!    area, nobody sees it, its moves are ignored (not answered) and it can start no battle. It is
//!    offered the target with `AreaChange`.
//! 4. The client answers `JoinArea` (checked: area, fingerprint and edition, like at the first
//!    join), and the player enters at the arrival with a full movement budget: `AreaSnapshot`
//!    (entities and NPCs) and `InstancePlacement` to it, `PlayerJoined` to the others of the area.
//!    Or it answers `CannotJoin`: it is sent back where it stood before the move, with another
//!    `AreaChange` for the area it came from (a second refusal ends the connection). Or it does not
//!    answer within [`TRAVEL_TIMEOUT`] and is disconnected.
//!
//! The area a player left stays loaded until the transfer is over (it is where a refused player
//! goes back to), and so does the target (a transit counts as being there when areas are unloaded).
//!
//! # Instances
//!
//! An area is loaded once, but its players are split into INSTANCES by story phase and party (see
//! the `instances` module): a player's `AreaSnapshot`, `PlayerJoined`/`PlayerLeft` and `EntityUpdate`s
//! concern its own instance only, the NPCs it is shown are the cast of its instance's phase
//! ([`AreaState::cast`]), and a transfer moves the player out of one instance and into one of the
//! target area: which one is decided by the player's phase there (or its party leader's), after the
//! `JoinArea` answer, exactly like the first join. The area stays loaded while anybody stands in any
//! instance of it.
//!
//! # Exits a script decides
//!
//! An exit of kind `ExitKind::Script` is a way out that the area's scripts decide, with conditions on the story (FF7:
//! a `MAPJUMP` that a line's trigger script runs when the player walks onto the line). Several of them may share a
//! line (the elevator of the first reactor has four, picked by the story): when a move crosses it, the server asks the
//! area which of the exits it crosses is open (`AreaRuntime::exit_state`) for the **story of the account that governs
//! the crossing player's instance**: its own progression solo, the party leader's for a member of a party (the phase
//! owner of its placement, the one whose phase chose the instance), and takes the first exit in the order
//! the area lists them (gateways first, then the scripts' in script order) that is open. A line whose exits are all
//! closed is walked over like any line. An exit whose conditions the area cannot judge (a variable the story does not
//! carry, a key the player would have to press) stays closed, and the server says so ONCE PER AREA
//! (`exit closed: its conditions cannot be judged ...`), with what the first such exit reads.
//!
//! **A member follows its leader's way out.** The story that judges a member is its leader's, and it moves while the
//! members are still on their way (the leader's finale moves FF7's story past the point where the lift of the first
//! reactor leads back into it). So when a party's leader leaves an area through an exit its story decided, the world
//! remembers which one, and a member of that party that later crosses the same line in the same area takes THAT exit,
//! once, whatever the story now says (the `follow` module; logged as `player follows its leader's way out` when the
//! story would have sent it elsewhere).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, AreaRuntime, Exit, ExitState, GameAdapter, Link, NpcSpawn, StoryPoint};
use fmv_core::{AnimState, AreaId, EntityId, Fingerprint, PhaseKey, Pos, ProvenanceSummary};
use fmv_protocol::{NpcSnapshot, RejectCode, ServerMsg, MAX_NPCS_PER_AREA, NPC_NAME_CHARS, NPC_REF_CHARS};

use super::battle::{AreaEncounters, BattleWorld};
use super::{is_full, normalize_facing, MoveBudget, MoveRules, World};
use crate::ServerError;

/// How long an area that no player is in (or heading for) stays loaded.
pub const AREA_UNLOAD_GRACE: Duration = Duration::from_secs(60);
/// How long a player may take to answer an `AreaChange` before it is disconnected.
pub const TRAVEL_TIMEOUT: Duration = Duration::from_secs(30);
/// Most areas loaded at once (the entry area included). Every player that walks through an exit
/// can load one, so this bounds what a crowd can make the server hold.
const MAX_LOADED_AREAS: usize = 128;
/// Most exits kept per area (FF7 has twelve gateway slots; the adapter adds at most 96 exits its scripts decide).
const MAX_EXITS_PER_AREA: usize = 128;
/// Most story points kept per area (a real field has at most a few).
const MAX_STORY_POINTS_PER_AREA: usize = 64;
/// Most links (ladders, jumps) kept per area (a real field has at most a handful).
const MAX_LINKS_PER_AREA: usize = 64;
/// Most of the exits and points an adapter left off that the log line about them lists (it counts them all).
const MAX_WITHHELD_LISTED: usize = 8;

/// What an area is offered as: the fields of `AreaManifest` / `AreaChange` that do not depend on
/// who is asking.
#[derive(Debug, Clone)]
pub(super) struct AreaOffer {
    display_name: String,
    fingerprint: Fingerprint,
    /// Path-free (`EditionProvenance::summary`).
    provenance: ProvenanceSummary,
    fixture: bool,
}

/// Everything the world keeps for one loaded area.
pub(super) struct AreaState {
    runtime: Box<dyn AreaRuntime>,
    id: AreaId,
    offer: AreaOffer,
    spawn_points: Vec<Pos>,
    rules: MoveRules,
    next_spawn: usize,
    exits: Vec<Exit>,
    /// Targets whose load failed: their exits are not watched any more.
    closed: BTreeSet<AreaKey>,
    /// An exit whose conditions the area cannot judge has been reported (once per area, see the module docs).
    unjudged_exit_noted: bool,
    /// The NPCs shown when no phase says otherwise (counted in the "area loaded" log line); what an
    /// instance shows is [`AreaState::cast`].
    npcs: Vec<NpcSnapshot>,
    /// The places where this area's story can advance a player, decoded once at load.
    story_points: Vec<StoryPoint>,
    /// The lines that put a player somewhere else in the area (ladders, jumps), decoded once at load.
    links: Vec<Link>,
    /// The area's random encounters, when battles are on and it has a table.
    encounters: Option<AreaEncounters>,
    /// Since when no player is in or heading for the area (`None` while there is one).
    empty_since: Option<Instant>,
}

/// Keep only printable characters, at most `max` of them (the text is the server's own data, but it
/// goes into frames the client shows, so it is bounded and clean anyway).
fn clip(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// What the server shows of the NPCs an area places: only the ones the data places for certain (a
/// story-dependent NPC may not be there at all), at most [`MAX_NPCS_PER_AREA`], bounded and clean.
fn npc_snapshots(spawns: Vec<NpcSpawn>) -> Vec<NpcSnapshot> {
    spawns
        .into_iter()
        .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
        .take(MAX_NPCS_PER_AREA)
        .enumerate()
        .map(|(i, n)| NpcSnapshot {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            name: clip(&n.entity_name, NPC_NAME_CHARS),
            model: clip(&n.appearance.model, NPC_REF_CHARS),
            idle_animation: n.appearance.idle_animation.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
            pos: n.pos,
            facing: normalize_facing(n.facing),
        })
        .collect()
}

impl AreaState {
    /// Take over a freshly loaded `runtime`. Refuses an area that cannot be hosted: no usable
    /// spawn point, an unusable speed, or provenance of another game than its id.
    pub(super) fn new(runtime: Box<dyn AreaRuntime>, battle: Option<&BattleWorld>) -> Result<Self, ServerError> {
        // `AreaInfo::provenance` carries local install paths: only its summary is kept.
        let info = runtime.info().clone();
        let id = info.id.clone();
        let label = crate::label_area(&info)?;
        let spawn_points: Vec<Pos> = runtime.spawn_points().into_iter().filter(|p| p.is_finite()).collect();
        if spawn_points.is_empty() {
            return Err(ServerError::BadArea(id, "the area has no usable (finite) spawn points".into()));
        }
        let max_speed = runtime.max_speed();
        if !(max_speed.is_finite() && max_speed > 0.0) {
            return Err(ServerError::BadArea(id, format!("max_speed must be finite and positive (got {max_speed})")));
        }
        let exits: Vec<Exit> = runtime
            .exits()
            .into_iter()
            .filter(|e| {
                e.line.0.is_finite() && e.line.1.is_finite() && e.arrival.is_finite() && !e.target.is_empty() && e.target != id.key
            })
            .take(MAX_EXITS_PER_AREA)
            .collect();
        let npcs = npc_snapshots(runtime.npcs());
        let story_points: Vec<StoryPoint> = runtime.story_points().into_iter().take(MAX_STORY_POINTS_PER_AREA).collect();
        let links: Vec<Link> = runtime
            .links()
            .into_iter()
            .filter(|l| l.line.0.is_finite() && l.line.1.is_finite() && l.to.is_finite())
            .take(MAX_LINKS_PER_AREA)
            .collect();
        let encounters = battle.and_then(|b| b.area_encounters(runtime.as_ref()));
        Ok(Self {
            offer: AreaOffer {
                display_name: label.display_name,
                fingerprint: info.fingerprint,
                provenance: info.provenance.summary(),
                fixture: label.fixture,
            },
            id,
            runtime,
            spawn_points,
            rules: MoveRules::new(max_speed),
            next_spawn: 0,
            exits,
            closed: BTreeSet::new(),
            unjudged_exit_noted: false,
            npcs,
            story_points,
            links,
            encounters,
            empty_since: None,
        })
    }

    pub(super) fn key(&self) -> &AreaKey {
        &self.id.key
    }

    /// The one log line about the exits, story points and links the adapter left off this area (`AreaRuntime::story_withheld`),
    /// said when the area is loaded: how many, which (the first few) and why. `None` when nothing was left off.
    pub(super) fn withheld_line(&self) -> Option<String> {
        let withheld = self.runtime.story_withheld()?;
        let listed = |items: &[String]| {
            let mut shown: Vec<String> = items.iter().take(MAX_WITHHELD_LISTED).map(|item| clip(item, 100)).collect();
            if items.len() > MAX_WITHHELD_LISTED {
                shown.push(format!("+{} more", items.len() - MAX_WITHHELD_LISTED));
            }
            shown.join("; ")
        };
        Some(format!(
            "story left off (said once for this area) area={} why={} script_exits={} story_points={} exits=[{}] points=[{}] links={} link_list=[{}]",
            self.id,
            clip(&withheld.reason, 60),
            withheld.exits.len(),
            withheld.points.len(),
            listed(&withheld.exits),
            listed(&withheld.points),
            withheld.links.len(),
            listed(&withheld.links)
        ))
    }

    pub(super) fn id(&self) -> &AreaId {
        &self.id
    }

    pub(super) fn runtime(&self) -> &dyn AreaRuntime {
        self.runtime.as_ref()
    }

    pub(super) fn rules(&self) -> &MoveRules {
        &self.rules
    }

    /// The NPCs of the instance of this area that runs `phase`: what the area says its cast is in that
    /// phase (`AreaRuntime::npcs_in_phase`, the same NPCs in every phase unless the adapter says
    /// otherwise), cleaned and bounded like every NPC list.
    pub(super) fn cast(&self, phase: &PhaseKey) -> Vec<NpcSnapshot> {
        npc_snapshots(self.runtime.npcs_in_phase(phase))
    }

    /// The places where this area's story can advance a player.
    pub(super) fn story_points(&self) -> &[StoryPoint] {
        &self.story_points
    }

    /// The lines that put a player somewhere else in this area.
    pub(super) fn links(&self) -> &[Link] {
        &self.links
    }

    /// Where the non-player character `id` of this area's own list (`AreaSnapshot::npcs`) stands.
    pub(super) fn npc_position(&self, id: u32) -> Option<Pos> {
        self.npcs.iter().find(|n| n.id == id).map(|n| n.pos)
    }

    pub(super) fn encounters(&self) -> Option<&AreaEncounters> {
        self.encounters.as_ref()
    }

    /// Round-robin over the spawn points.
    pub(super) fn next_spawn_point(&mut self) -> Pos {
        let pos = self.spawn_points.get(self.next_spawn % self.spawn_points.len().max(1)).copied().unwrap_or_default();
        self.next_spawn = self.next_spawn.wrapping_add(1);
        pos
    }

    /// Recompute what depends on the battle settings (they can be configured after the area was
    /// loaded).
    pub(super) fn refresh_encounters(&mut self, battle: Option<&BattleWorld>) {
        self.encounters = battle.and_then(|b| b.area_encounters(self.runtime.as_ref()));
    }

    /// The exits a move `from -> to` crosses, the one it meets first first (ties keep the area's order: gateways
    /// before the exits scripts decide, those in script order). Whether any of them is open for the player is
    /// [`World::open_exit`].
    pub(super) fn crossed_exits(&self, from: Pos, to: Pos) -> Vec<&Exit> {
        let mut crossed: Vec<(f64, &Exit)> = self
            .exits
            .iter()
            .filter(|e| !self.closed.contains(&e.target))
            .filter_map(|e| e.crossing(from, to).map(|t| (t, e)))
            .collect();
        // a stable sort: equal crossings (several exits on one line) stay in the area's order
        crossed.sort_by(|a, b| a.0.total_cmp(&b.0));
        crossed.into_iter().map(|(_, e)| e).collect()
    }

    /// The `AreaChange` that offers this area and says where the player will stand.
    fn change_message(&self, arrival: Pos, arrival_facing: f32) -> ServerMsg {
        ServerMsg::AreaChange {
            area: self.id.clone(),
            display_name: self.offer.display_name.clone(),
            fingerprint: self.offer.fingerprint,
            provenance: self.offer.provenance.clone(),
            fixture: self.offer.fixture,
            arrival,
            arrival_facing,
        }
    }
}

/// A player between two areas (see the module docs).
#[derive(Debug, Clone)]
pub(super) struct Transit {
    /// The area the player has been offered and must answer for.
    pub to: AreaKey,
    pub arrival: Pos,
    pub arrival_facing: f32,
    /// The player is disconnected if it has not answered by then.
    pub deadline: Instant,
    /// Where the player stood before the move that crossed the exit, in the area it left (which
    /// is `Player::area`, as long as the transit lasts): a refused transfer puts it back there.
    pub from_pos: Pos,
    pub from_facing: f32,
    /// This is the way back: the player refused the first offer. A second refusal ends the
    /// connection.
    pub bounced: bool,
}

/// What became of a `JoinArea` or `CannotJoin` sent in the middle of the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TravelReply {
    /// Handled (the messages that follow were queued).
    Done,
    /// The answer is not acceptable: end the connection with this code and reason.
    Refused(RejectCode, String),
    /// The player is not in transit: the message is out of place.
    NotTravelling,
    /// The player is no longer in the world.
    Gone,
}

/// Cut untrusted text for an error message that goes back to the client (and into logs).
fn clip_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

impl World {
    /// Say where the areas other than the entry area come from. Without a source the world has one
    /// area and its exits are not followed.
    pub fn set_area_source(&mut self, source: Arc<dyn GameAdapter>) {
        self.source = Some(source);
    }

    /// How long a player may take to answer an `AreaChange` (default [`TRAVEL_TIMEOUT`]).
    pub fn set_travel_timeout(&mut self, timeout: Duration) {
        self.travel_timeout = timeout;
    }

    /// How long an empty area stays loaded (default [`AREA_UNLOAD_GRACE`]; only tests change it).
    #[cfg(test)]
    pub fn set_unload_grace(&mut self, grace: Duration) {
        self.unload_grace = grace;
    }

    /// The keys of the loaded areas, in order.
    #[cfg(test)]
    pub fn loaded_areas(&self) -> Vec<AreaKey> {
        self.areas.keys().cloned().collect()
    }

    /// The area `entity` stands in, and whether it is in transit (in which case it is the area it
    /// left).
    #[cfg(test)]
    pub fn area_of(&self, entity: EntityId) -> Option<(AreaKey, bool)> {
        self.players.get(&entity).map(|p| (p.area.clone(), p.transit.is_some()))
    }

    /// The area `entity` is being sent to, if it is in transit.
    #[cfg(test)]
    pub fn transit_target(&self, entity: EntityId) -> Option<AreaKey> {
        self.players.get(&entity).and_then(|p| p.transit.as_ref().map(|t| t.to.clone()))
    }

    /// `entity` has just come to stand in its area (it joined, or finished a transfer): place it in
    /// the instance its phase (or its party leader's) selects, send it the `AreaSnapshot` (the entities
    /// of that instance, itself included, and the cast of its phase) and, directly after, its
    /// `InstancePlacement`; tell the others of the instance it is there (`World::settle`). The placement
    /// is always sent, even when it equals the last one told (a player sent back to the area it came
    /// from), so that every entry looks the same to a client.
    pub(super) fn announce_entry(&mut self, entity: EntityId, failed: &mut Vec<(EntityId, bool)>) {
        // Placed here, wherever it came from: every link within reach of the player starts disarmed.
        self.place_links(entity);
        // A leader that is back in an area is with its party again: the way it left by is over (`follow` module).
        self.forget_ways_into(entity);
        self.settle(entity, true, failed);
        // What the area's own scripts do by themselves from the first frame on moves the story of whoever stands
        // there (the points that fire on entry; a no-op in an area without any).
        self.enter_story(entity);
    }

    /// The first of `crossed` (the exits a move of `entity` crosses, in the order they are met) that is open for
    /// the story that governs it, if any (module docs, "Exits a script decides"). Gateways are always open; a script
    /// exit is asked of the area with the progression of the account that governs the player's instance. An exit
    /// the area cannot judge stays closed and is reported once per area.
    ///
    /// A member of a party that crosses the line its leader left the area by takes the exit the leader took, once,
    /// whatever the story now says (module docs, "A member follows its leader's way out"; the `follow` module).
    pub(super) fn open_exit(&mut self, entity: EntityId, crossed: Vec<Exit>) -> Option<Exit> {
        if crossed.is_empty() {
            return None;
        }
        let by_story = self.open_exit_by_story(entity, &crossed);
        let area_key = self.players.get(&entity)?.area.clone();
        let Some(way) = self.followed_exit(entity, &area_key, &crossed) else { return by_story };
        if by_story.as_ref() != Some(&way) {
            self.logs.info(format!(
                "player follows its leader's way out (the story it is judged on now would send it elsewhere) entity={} area={area_key} follows={} story_says={}",
                entity.0,
                clip_text(&way.target),
                by_story.as_ref().map_or_else(|| "nowhere".to_string(), |e| clip_text(&e.target))
            ));
        }
        Some(way)
    }

    /// The first of `crossed` that is open for the story that governs `entity` (see [`World::open_exit`]).
    fn open_exit_by_story(&mut self, entity: EntityId, crossed: &[Exit]) -> Option<Exit> {
        let progress = self.instance_progression(entity);
        let area_key = self.players.get(&entity)?.area.clone();
        let mut unjudged: Option<(String, String)> = None;
        let mut open = None;
        {
            let area = self.areas.get(&area_key)?;
            for exit in crossed {
                match area.runtime.exit_state(exit, &progress) {
                    ExitState::Open => {
                        open = Some(exit.clone());
                        break;
                    }
                    ExitState::Closed => {}
                    ExitState::Unknown(why) => {
                        unjudged.get_or_insert((exit.target.clone(), why));
                    }
                }
            }
        }
        if let Some((target, why)) = unjudged {
            if let Some(area) = self.areas.get_mut(&area_key).filter(|a| !a.unjudged_exit_noted) {
                area.unjudged_exit_noted = true;
                self.logs.info(format!(
                    "exit closed: its conditions cannot be judged (said once for this area) area={} to={} reads={}",
                    area.id,
                    clip_text(&target),
                    clip_text(&why)
                ));
            }
        }
        open
    }

    /// Make sure area `key` is loaded, loading it from the area source if it is not. Returns the key
    /// the area is stored under (the adapter's own spelling).
    fn ensure_area(&mut self, key: &str) -> Result<AreaKey, String> {
        if self.areas.contains_key(key) {
            return Ok(key.to_string());
        }
        let Some(source) = self.source.clone() else {
            return Err("this world has no area source".to_string());
        };
        if self.areas.len() >= MAX_LOADED_AREAS {
            return Err(format!("{MAX_LOADED_AREAS} areas are loaded already"));
        }
        let runtime = source.load_area(key).map_err(|e| format!("cannot load area {key:?}: {e}"))?;
        let game = runtime.info().id.game;
        if game != self.home_game() {
            return Err(format!("area {key:?} belongs to {game:?}, not to the game this server hosts"));
        }
        let state = AreaState::new(runtime, self.battle.as_ref()).map_err(|e| e.to_string())?;
        let stored = state.key().clone();
        if self.areas.contains_key(&stored) {
            // The adapter spelled the key differently from the one asked for.
            return Ok(stored);
        }
        // The `key=value` layout is read by `scripts/travel-smoke.sh` ("area loaded area=..."): keep it
        // (new keys go at the end).
        self.logs.info(format!(
            "area loaded area={} name={} exits={} npcs={} story_points={}",
            state.id(),
            state.offer.display_name,
            state.exits.len(),
            state.npcs.len(),
            state.story_points.len()
        ));
        for point in &state.story_points {
            self.logs.info(format!("story point area={} point={} {}", state.id(), point.id, clip(&point.label, 160)));
        }
        // What the adapter has not switched on for this area (FF7: a field nobody has reviewed), once per area.
        if let Some(line) = state.withheld_line() {
            self.logs.info(line);
        }
        self.areas.insert(stored.clone(), state);
        Ok(stored)
    }

    /// The game of the entry area (every other area must be of the same one).
    fn home_game(&self) -> fmv_core::GameId {
        self.areas.get(&self.home).map_or(fmv_core::GameId::Fixture, |a| a.id.game)
    }

    /// `entity`'s move `from -> ...` crossed `exit`: start the transfer (module docs, "Exits and
    /// travel"). `facing` is the heading of the move, which the player keeps unless the exit says
    /// otherwise. Returns `false`, with the exit closed, if the target is unusable; the caller then
    /// treats the move as an ordinary one.
    pub(super) fn start_travel(&mut self, entity: EntityId, exit: &Exit, from: Pos, facing: f32, now: Instant) -> bool {
        let Some(old_key) = self.players.get(&entity).map(|p| p.area.clone()) else { return false };
        let target_key = match self.ensure_area(&exit.target) {
            Ok(key) => key,
            Err(why) => {
                self.logs.warn(format!(
                    "an exit leads nowhere; it is closed for as long as the area stays loaded area={old_key} target={} why={why}",
                    clip_text(&exit.target)
                ));
                if let Some(area) = self.areas.get_mut(&old_key) {
                    area.closed.insert(exit.target.clone());
                }
                return false;
            }
        };
        let Some(target) = self.areas.get(&target_key) else { return false };
        let arrival = target.runtime.place_arrival(exit.arrival, exit.arrival_hint);
        if !arrival.is_finite() {
            self.logs.warn(format!("an exit's arrival cannot be placed; it is closed area={old_key} target={target_key}"));
            if let Some(area) = self.areas.get_mut(&old_key) {
                area.closed.insert(exit.target.clone());
            }
            return false;
        }
        let arrival_facing = normalize_facing(exit.arrival_facing.filter(|f| f.is_finite()).unwrap_or(facing));
        let offer = Arc::new(target.change_message(arrival, arrival_facing));

        let Some(p) = self.players.get_mut(&entity) else { return false };
        let from_facing = p.snapshot.facing;
        p.transit = Some(Transit {
            to: target_key.clone(),
            arrival,
            arrival_facing,
            deadline: now + self.travel_timeout,
            from_pos: from,
            from_facing,
            bounced: false,
        });
        p.dirty = false;
        let account = p.account;
        self.logs.info(format!("player leaves through an exit entity={} from={old_key} to={target_key}", entity.0));
        // The members of its party that come this way later go where it went (`follow` module).
        self.note_way_out(entity, &old_key, exit);

        // Everyone left behind in its instance hears it (the player is in no instance while in
        // transit, so it is not one of them), then the player hears where it goes.
        let mut failed = Vec::new();
        if let Some(instance) = self.leave_instance(account) {
            self.tell_instance(instance, Arc::new(ServerMsg::PlayerLeft { entity }), &mut failed);
        }
        if let Some(p) = self.players.get(&entity) {
            if let Err(e) = p.tx.try_send(offer) {
                failed.push((entity, is_full(&e)));
            }
        }
        self.drop_failed(failed);
        true
    }

    /// `JoinArea` from `entity`'s session in the middle of the game: the answer to an `AreaChange`.
    /// The area, fingerprint and edition are checked like at the first join; then the player enters
    /// the area at the arrival it was told (see the module docs).
    pub fn join_area(
        &mut self,
        entity: EntityId,
        area: &AreaId,
        fingerprint: &Fingerprint,
        provenance: &ProvenanceSummary,
        now: Instant,
    ) -> TravelReply {
        let Some(p) = self.players.get(&entity) else { return TravelReply::Gone };
        let Some(transit) = p.transit.clone() else { return TravelReply::NotTravelling };
        let Some(target) = self.areas.get(&transit.to) else {
            return TravelReply::Refused(RejectCode::Internal, "the area you were sent to is gone".to_string());
        };
        // Never echo client-supplied strings back unbounded.
        if *area != target.id {
            return TravelReply::Refused(RejectCode::ContentMismatch, format!("area mismatch: the server sent you to {}", target.id));
        }
        if *fingerprint != target.offer.fingerprint {
            return TravelReply::Refused(
                RejectCode::ContentMismatch,
                format!(
                    "fingerprint mismatch for area {}: your owned copy differs from the server's (different edition, region or revision?); client sent {:?}, server has {:?}",
                    target.id, fingerprint, target.offer.fingerprint
                ),
            );
        }
        let server = &target.offer.provenance;
        if !server.compatible_with(provenance) {
            return TravelReply::Refused(
                RejectCode::ContentMismatch,
                format!(
                    "edition mismatch for area {}: the server's copy is {:?}/{:?}/{:?}, yours is {:?}/{:?}/{:?}",
                    target.id,
                    server.edition,
                    server.region,
                    server.revision,
                    clip_text(&provenance.edition),
                    clip_text(&provenance.region),
                    clip_text(&provenance.revision),
                ),
            );
        }
        let budget = MoveBudget::full(now, target.rules());
        let Some(p) = self.players.get_mut(&entity) else { return TravelReply::Gone };
        p.area = transit.to.clone();
        p.transit = None;
        p.snapshot.pos = transit.arrival;
        p.snapshot.facing = transit.arrival_facing;
        p.snapshot.anim = AnimState::Idle;
        p.budget = budget;
        p.walked = 0.0;
        p.dirty = false;
        self.logs.info(format!("player entered the area entity={} to={}", entity.0, transit.to));
        let mut failed = Vec::new();
        self.announce_entry(entity, &mut failed);
        self.drop_failed(failed);
        TravelReply::Done
    }

    /// `CannotJoin` from `entity`'s session in the middle of the game: it cannot hold the area it
    /// was sent to. It goes back where it stood (with an `AreaChange` for the area it came from); a
    /// player that refuses the way back as well is told to leave.
    pub fn cannot_join(&mut self, entity: EntityId, area: &AreaId, now: Instant) -> TravelReply {
        let Some(p) = self.players.get(&entity) else { return TravelReply::Gone };
        let Some(transit) = p.transit.clone() else { return TravelReply::NotTravelling };
        self.logs.info(format!("client cannot hold the area it was sent to entity={} area={}", entity.0, clip_text(&area.to_string())));
        if transit.bounced {
            return TravelReply::Refused(RejectCode::ContentMismatch, "you cannot hold the area you came from either".to_string());
        }
        let origin = p.area.clone();
        let Some(origin_area) = self.areas.get(&origin) else {
            return TravelReply::Refused(RejectCode::Internal, "the area you came from is gone".to_string());
        };
        let offer = Arc::new(origin_area.change_message(transit.from_pos, transit.from_facing));
        let Some(p) = self.players.get_mut(&entity) else { return TravelReply::Gone };
        p.transit = Some(Transit {
            to: origin,
            arrival: transit.from_pos,
            arrival_facing: transit.from_facing,
            deadline: now + self.travel_timeout,
            from_pos: transit.from_pos,
            from_facing: transit.from_facing,
            bounced: true,
        });
        if let Err(e) = p.tx.try_send(offer) {
            let failed = vec![(entity, is_full(&e))];
            self.drop_failed(failed);
        }
        TravelReply::Done
    }

    /// Housekeeping, once per tick: disconnect players that did not answer an `AreaChange` in
    /// time, and unload the areas that have been empty for [`AREA_UNLOAD_GRACE`] (never the entry
    /// area). A player in transit counts as being in the area it left and in the one it was sent to.
    pub fn manage_areas(&mut self, now: Instant) {
        let late: Vec<EntityId> = self
            .players
            .iter()
            .filter(|(_, p)| p.transit.as_ref().is_some_and(|t| t.deadline <= now))
            .map(|(id, _)| *id)
            .collect();
        if !late.is_empty() {
            for id in &late {
                self.logs.warn(format!("no answer to AreaChange in time; disconnecting entity={}", id.0));
            }
            self.drop_failed(late.into_iter().map(|id| (id, false)).collect());
        }

        let occupied: BTreeSet<AreaKey> = self
            .players
            .values()
            .flat_map(|p| std::iter::once(p.area.clone()).chain(p.transit.as_ref().map(|t| t.to.clone())))
            .collect();
        let mut unload = Vec::new();
        for (key, area) in &mut self.areas {
            if *key == self.home || occupied.contains(key) {
                area.empty_since = None;
                continue;
            }
            let since = *area.empty_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= self.unload_grace {
                unload.push(key.clone());
            }
        }
        for key in unload {
            self.areas.remove(&key);
            self.logs.info(format!("area unloaded (empty) area={key}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MoveOutcome, NewPlayer, Outbound, PartyOutcome};
    use super::*;
    use fmv_core::{AccountId, AvatarAppearance};
    use fmv_fixture::rooms::{
        RoomsAdapter, EXIT_X, GATE_ADVANCED_ARRIVAL, GATE_LINE_Y, GATE_NEW_ARRIVAL, GATE_WALL_X, GHOST, HALL, HALL_ARRIVAL, HALL_MAX_SPEED,
        STORY_ADVANCED, YARD, YARD_ARRIVAL, YARD_MAX_SPEED,
    };

    /// Near and far side of the hall's exit line (x = `EXIT_X`).
    const NEAR: f32 = EXIT_X - 50.0;
    const FAR: f32 = EXIT_X + 50.0;
    use fmv_party::PartyConfig;
    use tokio::sync::{mpsc, watch};

    struct Seat {
        entity: EntityId,
        account: AccountId,
        rx: mpsc::Receiver<Outbound>,
        close: watch::Receiver<bool>,
    }

    impl Seat {
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            out
        }
    }

    fn rooms_world(adapter: RoomsAdapter) -> World {
        let adapter: Arc<dyn GameAdapter> = Arc::new(adapter);
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        w
    }

    fn seat(w: &mut World, name: &str, now: Instant) -> Seat {
        seat_with(w, name, &[], now)
    }

    /// A player whose account has the story progression `progression` (empty: a new account).
    fn seat_with(w: &mut World, name: &str, progression: &[u8], now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(256);
        let close = Arc::new(watch::channel(false).0);
        let close_rx = close.subscribe();
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), tx, close }, now)
            .expect("joins");
        Seat { entity, account, rx, close: close_rx }
    }

    /// Walk to (x, y) in small hops (the speed budget refills in real... server time we pass).
    fn walk_to(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
        *now += Duration::from_millis(700);
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
    }

    /// The exit's answer: `JoinArea` for whatever the last `AreaChange` offered.
    fn answer_join(w: &mut World, s: &Seat, offer: &ServerMsg, now: Instant) -> TravelReply {
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { panic!("not an AreaChange: {offer:?}") };
        w.join_area(s.entity, area, fingerprint, provenance, now)
    }

    fn area_change(msgs: &[ServerMsg]) -> ServerMsg {
        msgs.iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).cloned().expect("an AreaChange")
    }

    #[test]
    fn crossing_the_exit_line_sends_the_player_away_and_the_answer_brings_it_in() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        a.drain();
        b.drain();
        assert_eq!(w.loaded_areas(), [HALL.to_string()]);

        // A walks from its spawn (-100, 0) to x = 250, then across the line at x = 300.
        let mut now = t0;
        assert_eq!(walk_to(&mut w, &a, NEAR, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        assert_eq!(walk_to(&mut w, &a, FAR, 0.0, &mut now), MoveOutcome::Applied, "the crossing move is accepted");

        // In transit: the yard was loaded, A is in no area, B (left behind) heard it leave.
        assert_eq!(w.loaded_areas(), [HALL.to_string(), YARD.to_string()]);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), true)));
        assert_eq!(w.transit_target(a.entity), Some(YARD.to_string()));
        let b_msgs = b.drain();
        assert!(matches!(&b_msgs[..], [ServerMsg::PlayerLeft { entity }] if *entity == a.entity), "{b_msgs:?}");
        let a_msgs = a.drain();
        let offer = area_change(&a_msgs);
        match &offer {
            ServerMsg::AreaChange { area, display_name, fixture, arrival, arrival_facing, .. } => {
                assert_eq!(area.key, YARD);
                assert!(display_name.contains("FIXTURE") && *fixture);
                assert_eq!((arrival.x, arrival.y), YARD_ARRIVAL, "the arrival is the area data's, settled on its floor");
                assert_eq!(*arrival_facing, 0.0, "the heading of the crossing move is kept");
            }
            other => panic!("{other:?}"),
        }
        // While in transit its moves are ignored (neither applied nor answered) and nobody hears of it.
        assert_eq!(walk_to(&mut w, &a, 360.0, 0.0, &mut now), MoveOutcome::Frozen);
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());

        // The client holds the yard: A enters it. Snapshot, then placement, for A alone.
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        assert_eq!(w.area_of(a.entity), Some((YARD.to_string(), false)));
        let a_msgs = a.drain();
        match &a_msgs[..] {
            [ServerMsg::AreaSnapshot { area, entities, npcs, .. }, ServerMsg::InstancePlacement { placement }] => {
                assert_eq!(area.key, YARD);
                assert_eq!(entities.len(), 1, "only A is in the yard");
                assert_eq!(entities[0].id, a.entity);
                assert_eq!((entities[0].pos.x, entities[0].pos.y), YARD_ARRIVAL);
                assert_eq!(npcs.len(), 1, "the yard's guard is part of the snapshot");
                assert_eq!((npcs[0].name.as_str(), npcs[0].model.as_str()), ("guard", "guardm"));
                assert_eq!(placement.key.area.key, YARD, "placement is per area");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(b.drain().is_empty(), "B is in the hall and hears nothing of the yard");
        // The hall is still B's, and A can walk again in the yard.
        assert_eq!(walk_to(&mut w, &a, -150.0, 50.0, &mut now), MoveOutcome::Applied);
        w.advance_tick();
        let a_msgs = a.drain();
        assert!(matches!(&a_msgs[..], [ServerMsg::EntityUpdate { entities, .. }] if entities[0].pos.x == -150.0), "{a_msgs:?}");
        assert!(b.drain().is_empty(), "updates are scoped to the area");
    }

    #[test]
    fn a_player_arriving_sees_those_already_there_and_they_see_it() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        let mut now = t0;
        // A goes to the yard first.
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let offer = area_change(&a.drain());
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        a.drain();
        b.drain();
        // B follows: it walks from (100, 0) over the line.
        walk_to(&mut w, &b, FAR, 0.0, &mut now);
        let offer = area_change(&b.drain());
        assert_eq!(answer_join(&mut w, &b, &offer, now), TravelReply::Done);
        match &b.drain()[..] {
            [ServerMsg::AreaSnapshot { entities, .. }, ServerMsg::InstancePlacement { .. }] => {
                let ids: Vec<EntityId> = entities.iter().map(|e| e.id).collect();
                assert_eq!(ids, [a.entity, b.entity], "B sees A, and itself");
            }
            other => panic!("{other:?}"),
        }
        match &a.drain()[..] {
            [ServerMsg::PlayerJoined { snapshot }] => assert_eq!(snapshot.id, b.entity),
            other => panic!("A is told B arrived: {other:?}"),
        }
    }

    // ---- exits a script decides ----

    /// Where the `AreaChange` in `msgs` puts the player, in the target's coordinates.
    fn offered_arrival(msgs: &[ServerMsg]) -> (f32, f32) {
        match area_change(msgs) {
            ServerMsg::AreaChange { arrival, .. } => (arrival.x, arrival.y),
            other => panic!("{other:?}"),
        }
    }

    /// Walk from the spawn to the gated line of the hall (y = `GATE_LINE_Y`) and across it.
    fn cross_the_gated_line(w: &mut World, s: &Seat, now: &mut Instant) {
        assert_eq!(walk_to(w, s, 0.0, GATE_LINE_Y + 50.0, now), MoveOutcome::Applied);
        assert_eq!(walk_to(w, s, 0.0, GATE_LINE_Y - 30.0, now), MoveOutcome::Applied, "the crossing move is accepted");
    }

    fn logged(w: &mut World, needle: &str) -> usize {
        w.take_logs().iter().filter(|l| l.text().contains(needle)).count()
    }

    #[test]
    fn of_several_exits_on_one_line_the_first_that_is_open_for_the_story_is_taken_and_a_condition_it_cannot_judge_is_said_once() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut new_account = seat(&mut w, "New", t0);
        let mut advanced = seat_with(&mut w, "Advanced", STORY_ADVANCED, t0);
        new_account.drain();
        advanced.drain();
        w.take_logs();
        let mut now = t0;

        // The three exits of the line (hall y = -250) all lead to the yard but put the player in different places: the
        // first cannot be judged, the second is open for the advanced story, the third for a new account.
        cross_the_gated_line(&mut w, &new_account, &mut now);
        assert_eq!(w.transit_target(new_account.entity), Some(YARD.to_string()));
        assert_eq!(offered_arrival(&new_account.drain()), GATE_NEW_ARRIVAL, "a new account takes the exit that is open for it");
        let said = w.take_logs();
        let lines: Vec<&str> = said.iter().map(|l| l.text()).filter(|t| t.contains("exit closed")).collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("area=Fixture/hall") && lines[0].contains("to=yard") && lines[0].contains("reads=a key the fixture's hall wants pressed"), "{}", lines[0]);

        cross_the_gated_line(&mut w, &advanced, &mut now);
        assert_eq!(offered_arrival(&advanced.drain()), GATE_ADVANCED_ARRIVAL, "the advanced story takes the other one");
        assert_eq!(logged(&mut w, "exit closed"), 0, "once per area: the hall has said it");
    }

    #[test]
    fn a_member_of_a_party_crosses_with_the_story_of_its_leader_and_a_solo_player_with_its_own() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat_with(&mut w, "Leader", STORY_ADVANCED, t0);
        let mut member = seat(&mut w, "Member", t0);
        let mut solo = seat(&mut w, "Solo", t0);
        assert_eq!(w.party_invite(leader.entity, member.entity, t0), PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(member.entity, invite, t0), PartyOutcome::Done);
        for s in [&mut leader, &mut member, &mut solo] {
            s.drain();
        }
        let mut now = t0;
        // the member's own story is a new account's, but its instance is the leader's: the leader's exit is open
        cross_the_gated_line(&mut w, &member, &mut now);
        assert_eq!(offered_arrival(&member.drain()), GATE_ADVANCED_ARRIVAL, "the party's story, not the member's own");
        // a solo player with the same progression as the member takes the exit open for its own story
        cross_the_gated_line(&mut w, &solo, &mut now);
        assert_eq!(offered_arrival(&solo.drain()), GATE_NEW_ARRIVAL);
        // the leader is judged by itself, of course
        cross_the_gated_line(&mut w, &leader, &mut now);
        assert_eq!(offered_arrival(&leader.drain()), GATE_ADVANCED_ARRIVAL);
    }

    // ---- a member follows its leader's way out ----

    /// `leader` invites `member` (both in the world) and `member` accepts.
    fn form_party(w: &mut World, leader: &Seat, member: &Seat, now: Instant) {
        assert_eq!(w.party_invite(leader.entity, member.entity, now), PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(member.entity, invite, now), PartyOutcome::Done);
    }

    /// `s` crosses the hall's gated line and its client answers: it stands in the yard (where it is the entry point
    /// that moves a new account's story on). Returns where the line put it.
    fn through_the_gated_line_into_the_yard(w: &mut World, s: &mut Seat, now: &mut Instant) -> (f32, f32) {
        cross_the_gated_line(w, s, now);
        let msgs = s.drain();
        let arrival = offered_arrival(&msgs);
        assert_eq!(answer_join(w, s, &area_change(&msgs), *now), TravelReply::Done);
        s.drain();
        arrival
    }

    /// `s` (in the yard, where the gated line put it) walks out through the yard's gateway into the hall.
    fn back_into_the_hall(w: &mut World, s: &mut Seat, now: &mut Instant) {
        assert_eq!(walk_to(w, s, -350.0, -100.0, now), MoveOutcome::Applied);
        let msgs = s.drain();
        assert_eq!(answer_join(w, s, &area_change(&msgs), *now), TravelReply::Done);
        assert_eq!(w.area_of(s.entity), Some((HALL.to_string(), false)));
        s.drain();
    }

    /// THE RACE (found on the first reactor's lift, `scripts/travel-smoke.sh --escape` with a member some seconds behind):
    /// the leader takes the exit that is open for its story and the area it comes to moves that story on (the finale), and
    /// the member, still in the old area, comes to the same line afterwards: judged on the story the party has NOW it
    /// would be sent through another exit of the line (the later story's, to a field nobody has reviewed). It must go
    /// where the leader went.
    #[test]
    fn a_member_that_comes_to_the_line_after_its_leader_has_gone_through_takes_the_leaders_exit_though_the_story_moved_on() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat(&mut w, "Leader", t0);
        let mut member = seat(&mut w, "Member", t0);
        form_party(&mut w, &leader, &member, t0);
        leader.drain();
        member.drain();
        w.take_logs();
        let mut now = t0;

        // the leader takes the exit that is open for a new account's story; the yard's entry moves its story on
        assert_eq!(through_the_gated_line_into_the_yard(&mut w, &mut leader, &mut now), GATE_NEW_ARRIVAL);
        assert_eq!(w.players.get(&leader.entity).map(|p| p.progression.clone()), Some(STORY_ADVANCED.to_vec()), "the story moved on");
        assert_eq!(w.ways_of(leader.entity), 1, "the leader remembers the exit it took");
        assert_eq!(w.area_of(member.entity), Some((HALL.to_string(), false)), "the member is still in the hall");
        assert_eq!(logged(&mut w, "player follows"), 0);
        member.drain();

        // the member comes to the line only now: the story of its party says the other exit is the open one...
        cross_the_gated_line(&mut w, &member, &mut now);
        // ...and it goes where the leader went
        assert_eq!(offered_arrival(&member.drain()), GATE_NEW_ARRIVAL, "the leader's way out, not the one the story now opens");
        assert_eq!(w.transit_target(member.entity), Some(YARD.to_string()));
        let said = w.take_logs();
        let lines: Vec<&str> = said.iter().map(|l| l.text()).filter(|t| t.contains("player follows its leader's way out")).collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("area=hall") && lines[0].contains("follows=yard") && lines[0].contains("story_says=yard"), "{}", lines[0]);
        assert!(!lines[0].contains("from=") && !lines[0].contains(" to="), "not a move: a smoke that counts the server's moves must not count it: {}", lines[0]);
        // following is no story step: nobody's story moved, nothing was saved for the member, its own progression is its own
        assert_eq!(w.players.get(&member.entity).map(|p| p.progression.clone()), Some(Vec::new()));
        assert!(!said.iter().any(|l| l.text().contains("story advanced")), "following moves no story");
    }

    #[test]
    fn every_member_follows_once_and_the_next_time_it_comes_by_it_is_judged_on_the_story_as_always() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat(&mut w, "Leader", t0);
        let mut first = seat(&mut w, "First", t0);
        let mut second = seat(&mut w, "Second", t0);
        form_party(&mut w, &leader, &first, t0);
        form_party(&mut w, &leader, &second, t0);
        let mut now = t0;
        assert_eq!(through_the_gated_line_into_the_yard(&mut w, &mut leader, &mut now), GATE_NEW_ARRIVAL);
        first.drain();
        second.drain();

        // both come to the line afterwards and both go where the leader went: the way is not used up by the first
        for s in [&mut first, &mut second] {
            assert_eq!(through_the_gated_line_into_the_yard(&mut w, s, &mut now), GATE_NEW_ARRIVAL);
        }
        // `first` walks back into the hall and to the line again, the leader being in the yard still: that is no
        // way left by the leader any more, the story of the party decides (it is the advanced one now)
        back_into_the_hall(&mut w, &mut first, &mut now);
        cross_the_gated_line(&mut w, &first, &mut now);
        assert_eq!(offered_arrival(&first.drain()), GATE_ADVANCED_ARRIVAL, "judged on the story, not on a way it followed already");
    }

    #[test]
    fn a_leader_that_comes_back_to_the_area_is_with_its_party_again_and_the_story_decides() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat(&mut w, "Leader", t0);
        let mut member = seat(&mut w, "Member", t0);
        form_party(&mut w, &leader, &member, t0);
        let mut now = t0;
        assert_eq!(through_the_gated_line_into_the_yard(&mut w, &mut leader, &mut now), GATE_NEW_ARRIVAL);
        assert_eq!(w.ways_of(leader.entity), 1);
        member.drain();
        // the leader is back in the hall: the way it left by is forgotten...
        back_into_the_hall(&mut w, &mut leader, &mut now);
        assert_eq!(w.ways_of(leader.entity), 0);
        // ...so the member is judged on the story of its party (the advanced one) as before this existed
        cross_the_gated_line(&mut w, &member, &mut now);
        assert_eq!(offered_arrival(&member.drain()), GATE_ADVANCED_ARRIVAL);
    }

    #[test]
    fn a_way_out_is_for_the_party_it_was_made_for_and_ends_with_its_leadership() {
        let t0 = Instant::now();
        // a member that leaves the party before it comes to the line is nobody's follower: its own story judges it
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat(&mut w, "Leader", t0);
        let mut ahead = seat_with(&mut w, "Ahead", STORY_ADVANCED, t0);
        form_party(&mut w, &leader, &ahead, t0);
        let mut now = t0;
        assert_eq!(through_the_gated_line_into_the_yard(&mut w, &mut leader, &mut now), GATE_NEW_ARRIVAL);
        ahead.drain();
        assert_eq!(w.party_leave(ahead.entity), PartyOutcome::Done);
        ahead.drain();
        cross_the_gated_line(&mut w, &ahead, &mut now);
        assert_eq!(offered_arrival(&ahead.drain()), GATE_ADVANCED_ARRIVAL, "its own (advanced) story, no way to follow");

        // a leader that hands its party over remembers nothing of what it was
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut leader = seat(&mut w, "Leader", t0);
        let member = seat(&mut w, "Member", t0);
        form_party(&mut w, &leader, &member, t0);
        let mut now = t0;
        assert_eq!(through_the_gated_line_into_the_yard(&mut w, &mut leader, &mut now), GATE_NEW_ARRIVAL);
        assert_eq!(w.ways_of(leader.entity), 1);
        assert_eq!(w.party_promote(leader.entity, member.entity), PartyOutcome::Done);
        assert_eq!(w.ways_of(leader.entity), 0, "a new leader: the old one's ways are over");
    }

    #[test]
    fn only_the_exits_a_story_decides_are_remembered_a_way_replaces_the_one_of_its_area_and_a_leader_keeps_a_few() {
        use fmv_adapter_api::ExitKind;
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let leader = seat(&mut w, "Leader", t0);
        let member = seat(&mut w, "Member", t0);
        let solo = seat(&mut w, "Solo", t0);
        form_party(&mut w, &leader, &member, t0);
        let mut now = t0;

        // a gateway is data: open for every story, nothing to follow
        walk_to(&mut w, &leader, NEAR, 0.0, &mut now);
        walk_to(&mut w, &leader, FAR, 0.0, &mut now);
        assert_eq!(w.transit_target(leader.entity), Some(YARD.to_string()));
        assert_eq!(w.ways_of(leader.entity), 0, "a gateway is not a way a story decided");

        // the exits a script decides, as the world is told of them
        let exit = |id: u32, target: &str| Exit {
            line: (Pos::new(0.0, 0.0, 0.0), Pos::new(1.0, 0.0, 0.0)),
            target: target.to_string(),
            arrival: Pos::new(0.0, 0.0, 0.0),
            arrival_facing: None,
            arrival_hint: None,
            kind: ExitKind::Script { id },
        };
        w.note_way_out(leader.entity, &"a".to_string(), &exit(0, "x"));
        w.note_way_out(leader.entity, &"a".to_string(), &exit(1, "y"));
        assert_eq!(w.ways_of(leader.entity), 1, "the second way out of an area replaces the first");
        for i in 0..20 {
            w.note_way_out(leader.entity, &format!("area{i}"), &exit(i, "x"));
        }
        assert_eq!(w.ways_of(leader.entity), super::super::follow::MAX_WAYS_PER_LEADER, "a leader keeps the last few");
        // a member and a solo player remember nothing (only a party's leader is followed)
        w.note_way_out(member.entity, &"a".to_string(), &exit(0, "x"));
        w.note_way_out(solo.entity, &"a".to_string(), &exit(0, "x"));
        assert_eq!((w.ways_of(member.entity), w.ways_of(solo.entity)), (0, 0));
    }

    #[test]
    fn a_line_whose_exits_are_all_closed_is_walked_over_like_any_line() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_gates());
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        assert_eq!(walk_to(&mut w, &a, GATE_WALL_X + 50.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(walk_to(&mut w, &a, GATE_WALL_X - 50.0, 0.0, &mut now), MoveOutcome::Applied, "walked over, not refused");
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)), "nothing happened: the exit is closed");
        assert!(w.transit_target(a.entity).is_none());
        assert_eq!(w.players.get(&a.entity).map(|p| p.snapshot.pos.x), Some(GATE_WALL_X - 50.0));
        assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
        assert_eq!(logged(&mut w, "exit closed"), 0, "a closed exit is not an unjudged one");
        // the gateway to the yard is still a gateway: open whatever the story (B, from its own spawn)
        let mut b = seat(&mut w, "B", t0);
        b.drain();
        walk_to(&mut w, &b, NEAR, 0.0, &mut now);
        walk_to(&mut w, &b, FAR, 0.0, &mut now);
        assert_eq!(offered_arrival(&b.drain()), YARD_ARRIVAL);
    }

    #[test]
    fn the_way_back_works_and_arrivals_never_bounce() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let offer = area_change(&a.drain());
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        a.drain();
        // Standing at the arrival (x = -200) is not on the yard's line (x = -300): no bounce, and
        // walking about near it does nothing. Then across the line leads to the hall.
        assert_eq!(walk_to(&mut w, &a, -250.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((YARD.to_string(), false)));
        assert_eq!(walk_to(&mut w, &a, -350.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((YARD.to_string(), true)));
        let offer = area_change(&a.drain());
        match &offer {
            ServerMsg::AreaChange { area, arrival, .. } => {
                assert_eq!(area.key, HALL);
                assert_eq!((arrival.x, arrival.y), HALL_ARRIVAL);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        // Walking onwards in the hall behaves like in any area.
        assert_eq!(walk_to(&mut w, &a, 250.0, 100.0, &mut now), MoveOutcome::Applied);
    }

    #[test]
    fn the_new_area_has_its_own_rules_and_the_player_arrives_with_a_full_budget() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let a = seat(&mut w, "A", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let mut a = a;
        let offer = area_change(&a.drain());
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        // The very instant it arrives (no time has passed) it can already move: 0.5 s of travel at
        // 1.5 x the yard's speed is 900 units, more than the hall's 450 would allow. (500 units
        // across the yard, from (-200, 0) to (200, 300).)
        let out = w.apply_move(a.entity, Pos::new(200.0, 300.0, 0.0), 0.0, AnimState::Walk, now);
        assert_eq!(out, MoveOutcome::Applied, "yard speed {YARD_MAX_SPEED}, hall speed {HALL_MAX_SPEED}");
        // 400 units of the budget are left: the way back (500 units) is too much, the first half
        // of it is not.
        let out = w.apply_move(a.entity, Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0), 0.0, AnimState::Walk, now);
        assert!(matches!(out, MoveOutcome::Rejected { reason: super::super::Rejection::TooFast, .. }), "{out:?}");
        let out = w.apply_move(a.entity, Pos::new(0.0, 150.0, 0.0), 0.0, AnimState::Walk, now);
        assert_eq!(out, MoveOutcome::Applied);
    }

    #[test]
    fn a_wrong_answer_is_refused_and_a_stray_one_is_out_of_place() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut now = t0;
        // Not in transit: JoinArea / CannotJoin are out of place.
        let hall = w.areas.get(HALL).unwrap().id().clone();
        let fp = w.areas.get(HALL).unwrap().offer.fingerprint;
        let prov = w.areas.get(HALL).unwrap().offer.provenance.clone();
        assert_eq!(w.join_area(a.entity, &hall, &fp, &prov, now), TravelReply::NotTravelling);
        assert_eq!(w.cannot_join(a.entity, &hall, now), TravelReply::NotTravelling);
        assert_eq!(w.join_area(EntityId(99), &hall, &fp, &prov, now), TravelReply::Gone);

        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let offer = area_change(&a.drain());
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = &offer else { panic!() };
        // The wrong area, the wrong fingerprint and an incompatible edition are all refused...
        assert!(matches!(w.join_area(a.entity, &hall, fingerprint, provenance, now), TravelReply::Refused(RejectCode::ContentMismatch, m) if m.contains("area mismatch")));
        assert!(matches!(w.join_area(a.entity, area, &Fingerprint([9; 32]), provenance, now), TravelReply::Refused(RejectCode::ContentMismatch, m) if m.contains("fingerprint mismatch")));
        let mut other_edition = provenance.clone();
        other_edition.edition = "something\nelse".into();
        let refused = w.join_area(a.entity, area, fingerprint, &other_edition, now);
        assert!(matches!(&refused, TravelReply::Refused(RejectCode::ContentMismatch, m) if m.contains("edition mismatch") && !m.contains('\n')), "{refused:?}");
        // ...without changing anything: the player is still in transit and can still answer right.
        assert_eq!(w.transit_target(a.entity), Some(YARD.to_string()));
        assert_eq!(w.join_area(a.entity, area, fingerprint, provenance, now), TravelReply::Done);
    }

    #[test]
    fn a_client_that_cannot_hold_the_area_is_sent_back_where_it_stood() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        a.drain();
        b.drain();
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let offer = area_change(&a.drain());
        b.drain();
        let ServerMsg::AreaChange { area, .. } = &offer else { panic!() };
        // CannotJoin: an AreaChange for the hall comes back, with the position before the move.
        assert_eq!(w.cannot_join(a.entity, area, now), TravelReply::Done);
        let back = area_change(&a.drain());
        match &back {
            ServerMsg::AreaChange { area, arrival, .. } => {
                assert_eq!(area.key, HALL);
                assert_eq!((arrival.x, arrival.y), (NEAR, 0.0), "where it stood before the crossing move");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.transit_target(a.entity), Some(HALL.to_string()));
        assert_eq!(answer_join(&mut w, &a, &back, now), TravelReply::Done);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        // B sees it come back (and heard it leave before).
        assert!(matches!(&b.drain()[..], [ServerMsg::PlayerJoined { snapshot }] if snapshot.id == a.entity && snapshot.pos.x == NEAR));
        // Walking across the line again is a new crossing (it is on the near side).
        a.drain();
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        assert_eq!(w.transit_target(a.entity), Some(YARD.to_string()));
        // Refusing the way back as well ends the connection.
        a.drain();
        assert_eq!(w.cannot_join(a.entity, area, now), TravelReply::Done);
        let ServerMsg::AreaChange { area: hall, .. } = area_change(&a.drain()) else { panic!() };
        assert_eq!(hall.key, HALL);
        assert!(matches!(w.cannot_join(a.entity, &hall, now), TravelReply::Refused(RejectCode::ContentMismatch, _)));
    }

    #[test]
    fn a_player_that_never_answers_is_disconnected_and_the_world_forgets_the_transfer() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        w.set_travel_timeout(Duration::from_secs(5));
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        a.drain();
        b.drain();
        let started = now;
        w.manage_areas(started + Duration::from_secs(4));
        assert_eq!(w.player_count(), 2, "still within the time to answer");
        w.manage_areas(started + Duration::from_secs(6));
        assert_eq!(w.player_count(), 1);
        assert!(*a.close.borrow(), "the slow connection is told to close");
        assert!(!w.is_online(a.account));
        assert!(b.drain().is_empty(), "B had heard A leave the hall already, and hears nothing more");
    }

    #[test]
    fn an_empty_area_is_unloaded_after_its_grace_and_the_entry_area_never_is() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        w.set_unload_grace(Duration::from_secs(10));
        // This test jumps the clock by minutes: the time to answer must not run out in between.
        w.set_travel_timeout(Duration::from_secs(1_000_000));
        let mut a = seat(&mut w, "A", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        let offer = area_change(&a.drain());
        // In transit, the yard (the target) and the hall (the way back) are both occupied.
        w.manage_areas(now + Duration::from_secs(100));
        assert_eq!(w.loaded_areas().len(), 2);
        let t = now + Duration::from_secs(100);
        assert_eq!(answer_join(&mut w, &a, &offer, t), TravelReply::Done);
        // The hall is the entry area: it stays. The yard is occupied.
        w.manage_areas(t + Duration::from_secs(1000));
        assert_eq!(w.loaded_areas().len(), 2);
        // A walks back to the hall; the yard empties, and goes after its grace (not before).
        let mut now = t;
        walk_to(&mut w, &a, -250.0, 0.0, &mut now);
        walk_to(&mut w, &a, -350.0, 0.0, &mut now);
        let offer = area_change(&a.drain());
        assert_eq!(answer_join(&mut w, &a, &offer, now), TravelReply::Done);
        w.manage_areas(now); // starts the clock
        w.manage_areas(now + Duration::from_secs(9));
        assert_eq!(w.loaded_areas().len(), 2, "within the grace");
        w.manage_areas(now + Duration::from_secs(11));
        assert_eq!(w.loaded_areas(), [HALL.to_string()], "the yard is gone");
        // Someone going there again loads it again.
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        assert_eq!(w.loaded_areas().len(), 2);
        assert_eq!(w.transit_target(a.entity), Some(YARD.to_string()));
    }

    #[test]
    fn an_exit_to_an_area_that_cannot_load_is_closed_and_the_move_stands() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::with_unloadable_area());
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        let mut now = t0;
        // Walk up to the hall's top exit (y = 250) and across it.
        assert_eq!(walk_to(&mut w, &a, -100.0, 200.0, &mut now), MoveOutcome::Applied);
        assert_eq!(walk_to(&mut w, &a, -50.0, 280.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)), "the crossing did not take the player anywhere");
        assert_eq!(w.loaded_areas(), [HALL.to_string()], "{GHOST} was never loaded");
        w.advance_tick();
        let msgs = a.drain();
        assert!(matches!(&msgs[..], [ServerMsg::EntityUpdate { entities, .. }] if entities[0].pos.y == 280.0), "the move took effect: {msgs:?}");
        // The exit is closed: walking back and forth over the line stays uneventful (no log spam,
        // no second load attempt).
        assert_eq!(walk_to(&mut w, &a, -50.0, 200.0, &mut now), MoveOutcome::Applied);
        assert_eq!(walk_to(&mut w, &a, -50.0, 290.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        // The ordinary exit still works.
        assert_eq!(walk_to(&mut w, &a, NEAR, 0.0, &mut now), MoveOutcome::Applied);
        walk_to(&mut w, &a, FAR, 0.0, &mut now);
        assert_eq!(w.transit_target(a.entity), Some(YARD.to_string()));
    }

    #[test]
    fn a_single_area_world_has_no_exits_to_follow() {
        let t0 = Instant::now();
        let adapter = RoomsAdapter::new();
        // No `set_area_source`: the hall's exit to the yard is not followed.
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        let a = seat(&mut w, "A", t0);
        let mut now = t0;
        assert_eq!(walk_to(&mut w, &a, NEAR, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(walk_to(&mut w, &a, FAR, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        assert_eq!(w.loaded_areas(), [HALL.to_string()]);
    }

    #[test]
    fn a_player_in_a_battle_cannot_travel() {
        use crate::config::BattleSettings;
        use fmv_protocol::SyntheticRules;
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        w.configure_battle(Some(BattleSettings::new(Arc::new(SyntheticRules::default()))), true);
        let mut a = seat(&mut w, "A", t0);
        let mut now = t0;
        walk_to(&mut w, &a, NEAR, 0.0, &mut now);
        assert_eq!(w.request_encounter(a.entity, Some(0), now), super::super::BattleReply::Done);
        a.drain();
        // Frozen in the battle: its attempt to walk over the line is ignored, nothing happens.
        assert_eq!(walk_to(&mut w, &a, FAR, 0.0, &mut now), MoveOutcome::Frozen);
        assert_eq!(w.area_of(a.entity), Some((HALL.to_string(), false)));
        assert_eq!(w.loaded_areas(), [HALL.to_string()]);
    }

    #[test]
    fn parties_span_areas_and_a_battle_takes_only_the_members_in_the_requesters_area() {
        use crate::config::BattleSettings;
        use fmv_protocol::SyntheticRules;
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        w.configure_battle(Some(BattleSettings::new(Arc::new(SyntheticRules::default()))), true);
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        assert_eq!(w.party_invite(a.entity, b.entity, t0), PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, t0), PartyOutcome::Done);
        a.drain();
        b.drain();
        // B goes to the yard; the party stays whole.
        let mut now = t0;
        walk_to(&mut w, &b, FAR, 0.0, &mut now);
        let offer = area_change(&b.drain());
        assert_eq!(answer_join(&mut w, &b, &offer, now), TravelReply::Done);
        assert_eq!(w.party_size_of(a.account), 2, "a party spans areas");
        assert_eq!(w.party_size_of(b.account), 2);
        let b_msgs = b.drain();
        // B's placement now names the yard, with the leader's phase.
        let placements: Vec<_> = b_msgs.iter().filter_map(|m| match m { ServerMsg::InstancePlacement { placement } => Some(placement.clone()), _ => None }).collect();
        assert_eq!(placements.len(), 1);
        assert_eq!((placements[0].key.area.key.as_str(), placements[0].phase_owner, placements[0].party.is_some()), (YARD, a.account, true));
        // A asks for a battle: B is in another area, so it fights alone and B is not frozen.
        assert_eq!(w.request_encounter(a.entity, Some(0), now), super::super::BattleReply::Done);
        assert!(w.in_battle(a.entity) && !w.in_battle(b.entity));
        let started: Vec<ServerMsg> = a.drain().into_iter().filter(|m| matches!(m, ServerMsg::EncounterStart { .. })).collect();
        match &started[..] {
            [ServerMsg::EncounterStart { party, .. }] => assert_eq!(party.len(), 1),
            other => panic!("{other:?}"),
        }
        assert!(!b.drain().iter().any(|m| matches!(m, ServerMsg::EncounterStart { .. })), "B was not pulled into A's battle");
        // Disconnecting while the other is elsewhere still cleans up the party.
        assert!(w.leave(a.entity));
        assert_eq!(w.party_size_of(b.account), 1);
    }

    #[test]
    fn two_players_in_different_areas_do_not_see_each_others_updates() {
        let t0 = Instant::now();
        let mut w = rooms_world(RoomsAdapter::new());
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        let mut now = t0;
        walk_to(&mut w, &b, FAR, 0.0, &mut now);
        let offer = area_change(&b.drain());
        assert_eq!(answer_join(&mut w, &b, &offer, now), TravelReply::Done);
        a.drain();
        b.drain();
        // Both walk; each hears only its own area.
        walk_to(&mut w, &a, -50.0, 40.0, &mut now);
        walk_to(&mut w, &b, -150.0, 40.0, &mut now);
        w.advance_tick();
        let (a_msgs, b_msgs) = (a.drain(), b.drain());
        match (&a_msgs[..], &b_msgs[..]) {
            ([ServerMsg::EntityUpdate { entities: ea, .. }], [ServerMsg::EntityUpdate { entities: eb, .. }]) => {
                assert_eq!(ea.iter().map(|e| e.id).collect::<Vec<_>>(), [a.entity]);
                assert_eq!(eb.iter().map(|e| e.id).collect::<Vec<_>>(), [b.entity]);
            }
            other => panic!("{other:?}"),
        }
        // A disconnecting tells the hall (nobody), not the yard.
        assert!(w.leave(a.entity));
        assert!(b.drain().is_empty());
    }
}
