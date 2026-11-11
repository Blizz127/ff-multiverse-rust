//! MMO-style saving of where a player stands, and putting it back there at login.
//!
//! The owner's rule (2026-11-10): the game saves constantly and loads like an MMO: logging out keeps the place,
//! logging back in returns to the same spot; there are no save points, except ones that are there to heal.
//!
//! * **When a spot is saved** ([`World::location_arrived`], [`World::save_locations`], [`World::location_left`]):
//!   when the player comes to stand in an area (every travel's arrival), at most every [`SAVE_EVERY`] while it moves
//!   (the tick), and when it leaves the world (logging out, a dropped connection, and a server shutdown, which closes
//!   every connection before it stops). Never during a debug scene preview (its story is made up), and never between
//!   two areas: the spot saved before the exit stays (the area it left is the one it stood in).
//! * **What is saved** (`fmv_persist::SavedLocation`): the area, the position with its height (FF7 picks the floor of
//!   a spot where floors overlap by the height it is asked about), the heading, the story the player had, and whether
//!   a login may use the spot.
//! * **When a login uses it** ([`World::prepare_resume`] before `join`): the spot is resumable, it was saved with
//!   exactly the story the account has now, its area can be hosted and the spot is on that area's walkable surface.
//!   A spot in the entry area puts the player there instead of on a spawn point; a spot elsewhere sends it there with
//!   the ordinary area handshake (`start_travel`), as a story entry does. Otherwise the player joins as it always did:
//!   where everyone joins, then sent on to where its story stands (`enter_at_story`). A spot that cannot be used never
//!   closes an exit, never moves anybody else and is never deleted (the next save replaces it).
//! * **A spot is not resumable**
//!   - where the story stands somewhere else that the player has not stood in under this story. FF7: the reactor's
//!     finale moves the story to 33 in `nrthmk`, and only the story's entry (`md8_4`) leads on from there; no walk does.
//!     A player that reached the story's entry and walked on (from `md8_1` to `md8_2`) resumes where it walked to:
//!     the judgement is made when the spot is saved, and a resumed spot carries it into the next session.
//!   - when another account's story governed the player's place (a party member: alone after its next login, its own
//!     story need not open the ways out of where its leader took it).
//! * **Countdowns are not stored** (see `countdown`): a player that logs out while one runs comes back without it.
//! * **Save points save nothing** (saving is constant); where a game has them they may only heal.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, Exit, ExitKind};
use fmv_core::{AccountId, EntityId, GameId, Pos};
use fmv_persist::SavedLocation;

use super::{Player, World};
use crate::location_saver::LocationSaverHandle;

/// How often a moving player's spot is saved, at most: constantly, without a file write every tick.
pub(crate) const SAVE_EVERY: Duration = Duration::from_secs(5);
/// A move shorter than this (in the area's units) is no reason for a write.
const MOVED_AT_LEAST: f32 = 1.0;
/// A turn smaller than this (radians) is no reason for a write.
const TURNED_AT_LEAST: f32 = 0.05;

/// The area a story stands in, if it stands somewhere other than where everyone joins (the scene host's story
/// entry: `SceneHost::entry`). Given as a plain function so this module needs nothing else of the scenes.
pub(crate) type StoryEntryFn = Arc<dyn Fn(&[u8]) -> Option<AreaKey> + Send + Sync>;

/// The world's part of MMO-style saving.
#[derive(Default)]
pub(super) struct Resume {
    /// Where spots are queued (`None`: nothing is saved, as in most unit tests of the world).
    saver: Option<LocationSaverHandle>,
    game: Option<GameId>,
    entry_of: Option<StoryEntryFn>,
    /// The spot each joining account saved, read by its login just before `join`.
    pending: BTreeMap<AccountId, SavedLocation>,
    /// What was saved for each player in the world.
    tracked: BTreeMap<EntityId, Tracker>,
}

#[derive(Default)]
struct Tracker {
    /// What was saved last: area, position, heading, resumable, and the story it was saved with (a story step taken
    /// standing still must be saved too: a spot saved with an older story is stale to the next login).
    last: Option<(AreaKey, Pos, f32, bool, Vec<u8>)>,
    saved_at: Option<Instant>,
    /// The story entry field this player has stood in (FF7: `md8_1`): while its story still stands there, a spot it walks
    /// on to stays resumable, whatever flags the story sets on the way (they change the story, not where it stands).
    entry_reached: Option<AreaKey>,
}

/// Why a saved spot was not used (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unusable {
    NotResumable,
    StoryChanged,
    AreaUnavailable,
    OffTheSurface,
    TravelRefused,
}

impl Unusable {
    fn why(self) -> &'static str {
        match self {
            Unusable::NotResumable => "the story stood elsewhere, or a party leader's story governed the place",
            Unusable::StoryChanged => "the story changed after the spot was saved",
            Unusable::AreaUnavailable => "its area cannot be hosted",
            Unusable::OffTheSurface => "it is not on the area's walkable surface",
            Unusable::TravelRefused => "the area handshake could not start",
        }
    }
}

fn spot_pos(spot: &SavedLocation) -> Pos {
    Pos::new(spot.pos[0], spot.pos[1], spot.pos[2])
}

fn moved(a: Pos, b: Pos) -> bool {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    (dx * dx + dy * dy + dz * dz).sqrt() >= MOVED_AT_LEAST
}

/// Whether `p`'s spot may be resumed, and the remembered "the story's entry field was reached" for it.
fn judge(entry_of: Option<&StoryEntryFn>, p: &Player, tracker: &mut Tracker) -> bool {
    let governed = p.placement.as_ref().is_some_and(|placement| placement.phase_owner != p.account);
    if governed {
        return false;
    }
    let Some(entry) = entry_of.and_then(|entry| entry(&p.progression)) else {
        // A story that stands nowhere in particular: wherever it walked is where it is.
        return true;
    };
    if entry.eq_ignore_ascii_case(&p.area) {
        tracker.entry_reached = Some(entry);
        return true;
    }
    tracker.entry_reached.as_deref().is_some_and(|reached| reached.eq_ignore_ascii_case(&entry))
}

impl World {
    /// Save players' spots into `saver` from now on, for `game`; `entry_of` says where a story stands (see
    /// [`StoryEntryFn`]). Without this nothing is saved, and a saved spot is still used at login.
    pub(crate) fn configure_resume(&mut self, saver: LocationSaverHandle, game: GameId, entry_of: Option<StoryEntryFn>) {
        self.resume.saver = Some(saver);
        self.resume.game = Some(game);
        self.resume.entry_of = entry_of;
    }

    /// The spot `account` saved (`None`: none, or it could not be read), handed over by its login just before `join`,
    /// under the same world lock.
    pub(crate) fn prepare_resume(&mut self, account: AccountId, spot: Option<SavedLocation>) {
        match spot {
            Some(spot) => {
                self.resume.pending.insert(account, spot);
            }
            None => {
                self.resume.pending.remove(&account);
            }
        }
    }

    /// Whether the pending spot of `account` may be used by a player whose story is `progression`.
    fn judge_pending(&self, account: AccountId, progression: &[u8]) -> Option<Result<&SavedLocation, Unusable>> {
        let spot = self.resume.pending.get(&account)?;
        Some(if !spot.resumable {
            Err(Unusable::NotResumable)
        } else if spot.story != progression {
            Err(Unusable::StoryChanged)
        } else {
            Ok(spot)
        })
    }

    /// For `join`: where in the entry area the joining `account` stands, if it saved a usable spot there (instead of
    /// a spawn point), and its heading.
    pub(super) fn resume_home_spot(&self, account: AccountId, progression: &[u8]) -> Option<(Pos, f32)> {
        let spot = self.judge_pending(account, progression)?.ok()?;
        if !spot.area.eq_ignore_ascii_case(&self.home) {
            return None;
        }
        let placed = self.areas.get(&self.home)?.runtime().place_arrival(spot_pos(spot), None);
        placed.is_finite().then_some((placed, spot.facing))
    }

    /// The end of `join`: a usable spot elsewhere sends the player there; a usable spot in the entry area already put
    /// it there; otherwise it goes where its story stands, as it always did.
    pub(super) fn enter_at_saved_spot_or_story(&mut self, entity: EntityId, now: Instant) {
        let Some(player) = self.players.get(&entity) else { return };
        let (account, progression, from, facing) = (player.account, player.progression.clone(), player.snapshot.pos, player.snapshot.facing);
        self.resume.tracked.insert(entity, Tracker::default());
        let outcome = match self.judge_pending(account, &progression) {
            None => {
                self.enter_at_story(entity, now);
                return;
            }
            Some(Err(why)) => Err(why),
            Some(Ok(spot)) => Ok(spot.clone()),
        };
        self.resume.pending.remove(&account);
        let result = outcome.and_then(|spot| {
            if spot.area.eq_ignore_ascii_case(&self.home) {
                // `join` placed it (`resume_home_spot`) unless the spot was off the surface.
                let placed = self.areas.get(&self.home).map(|home| home.runtime().place_arrival(spot_pos(&spot), None));
                return if placed.is_some_and(|p| p.is_finite() && !moved(p, from)) { Ok(spot) } else { Err(Unusable::OffTheSurface) };
            }
            let key = self.ensure_area(&spot.area).map_err(|_| Unusable::AreaUnavailable)?;
            let placed = self.areas.get(&key).map(|area| area.runtime().place_arrival(spot_pos(&spot), None)).ok_or(Unusable::AreaUnavailable)?;
            if !placed.is_finite() {
                return Err(Unusable::OffTheSurface);
            }
            let exit = Exit {
                line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)),
                target: key,
                arrival: placed,
                arrival_facing: Some(spot.facing),
                arrival_hint: None,
                kind: ExitKind::Gateway,
            };
            if self.start_travel(entity, &exit, from, facing, now) { Ok(spot) } else { Err(Unusable::TravelRefused) }
        });
        match result {
            Ok(spot) => {
                // The spot was judged resumable under this very story when it was saved: carry that into this session.
                // The file already says where it stands, so a player that does not move rewrites nothing.
                if let Some(tracker) = self.resume.tracked.get_mut(&entity) {
                    tracker.last = Some((spot.area.clone(), spot_pos(&spot), spot.facing, true, progression.clone()));
                    tracker.entry_reached = self.resume.entry_of.as_ref().and_then(|entry| entry(&progression));
                }
                self.logs.info(format!(
                    "resumed account={} entity={} area={} at=({:.0},{:.0},{:.0}) (where it left)",
                    account.0, entity.0, spot.area, spot.pos[0], spot.pos[1], spot.pos[2]
                ));
            }
            Err(why) => {
                self.logs.info(format!("saved spot not used account={} entity={}: {}", account.0, entity.0, why.why()));
                self.enter_at_story(entity, now);
            }
        }
    }

    /// The end of a `join` that a recovering scene places (`continuation`): its spots are saved from now on, but its saved
    /// spot is not used.
    pub(super) fn resume_track(&mut self, entity: EntityId) {
        self.resume.tracked.entry(entity).or_default();
    }

    /// After a travel (`join_area`): the player stands somewhere new; save it now (unless it is the spot already saved,
    /// as at the end of a resume).
    pub(super) fn location_arrived(&mut self, entity: EntityId, now: Instant) {
        self.save_spot(entity, now, true);
    }

    /// The tick: save the spot of every player that moved or turned since its last one, at most every [`SAVE_EVERY`].
    pub fn save_locations(&mut self, now: Instant) {
        if self.resume.saver.is_none() {
            return;
        }
        let ids: Vec<EntityId> = self.players.keys().copied().collect();
        for entity in ids {
            self.save_spot(entity, now, false);
        }
    }

    /// `player` (already out of `players`) leaves the world: save where it stood. A player whose `join` never finished
    /// (it has no tracker: a scene recovery that failed) saves nothing: it stood on a spawn point, not on its spot.
    pub(super) fn location_left(&mut self, entity: EntityId, player: &Player) {
        self.resume.pending.remove(&player.account);
        let Some(mut tracker) = self.resume.tracked.remove(&entity) else { return };
        if let Some(spot) = self.spot_of(player, &mut tracker) {
            if let Some(saver) = &self.resume.saver {
                saver.save(spot);
            }
        }
    }

    fn save_spot(&mut self, entity: EntityId, now: Instant, arrived: bool) {
        if self.resume.saver.is_none() {
            return;
        }
        let Some(player) = self.players.get(&entity) else { return };
        let mut tracker = self.resume.tracked.remove(&entity).unwrap_or_default();
        let due = arrived || tracker.saved_at.is_none_or(|at| now.duration_since(at) >= SAVE_EVERY);
        let spot = if due { self.spot_of(player, &mut tracker) } else { None };
        if let Some(spot) = spot {
            let queued = self.resume.saver.as_ref().is_some_and(|saver| saver.save(spot.clone()));
            if queued {
                tracker.last = Some((spot.area.clone(), spot_pos(&spot), spot.facing, spot.resumable, spot.story.clone()));
                tracker.saved_at = Some(now);
            }
        }
        self.resume.tracked.insert(entity, tracker);
    }

    /// The spot to save for `p` now, if it should be saved: not while previewing a scene or between two areas, and only
    /// when it differs from the last one saved (its place, heading, resumability or story).
    fn spot_of(&self, p: &Player, tracker: &mut Tracker) -> Option<SavedLocation> {
        if p.preview.is_some() || p.transit.is_some() {
            return None;
        }
        let game = self.resume.game?;
        let resumable = judge(self.resume.entry_of.as_ref(), p, tracker);
        let (pos, facing) = (p.snapshot.pos, p.snapshot.facing);
        if !pos.is_finite() || !facing.is_finite() {
            return None;
        }
        let unchanged = tracker.last.as_ref().is_some_and(|(area, last_pos, last_facing, last_resumable, last_story)| {
            area == &p.area
                && !moved(*last_pos, pos)
                && (last_facing - facing).abs() < TURNED_AT_LEAST
                && *last_resumable == resumable
                && *last_story == p.progression
        });
        if unchanged {
            return None;
        }
        Some(SavedLocation::new(p.account, game, p.area.clone(), [pos.x, pos.y, pos.z], facing, p.progression.clone(), resumable, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NewPlayer, Outbound, PartyOutcome, TravelReply};
    use super::*;
    use crate::location_saver::Msg;
    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AnimState, AvatarAppearance};
    use fmv_fixture::rooms::{RoomsAdapter, EXIT_X, HALL, YARD};
    use fmv_party::PartyConfig;
    use fmv_protocol::ServerMsg;
    use tokio::sync::{mpsc, watch};

    struct Seat {
        entity: EntityId,
        rx: mpsc::Receiver<Outbound>,
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

    /// The fixture's two rooms (the hall is where everyone joins), saving spots into a queue the test reads, with a
    /// story entry: a story of `[33]` stands in the yard.
    fn world() -> (World, mpsc::UnboundedReceiver<Msg>) {
        let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::new());
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (saver, rx) = LocationSaverHandle::channel();
        let entry: StoryEntryFn = Arc::new(|story: &[u8]| (story == [33]).then(|| YARD.to_string()));
        w.configure_resume(saver, GameId::Ff7, Some(entry));
        (w, rx)
    }

    fn join(w: &mut World, account: u64, progression: &[u8], spot: Option<SavedLocation>, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(256);
        let close = Arc::new(watch::channel(false).0);
        let account = AccountId(account);
        let avatar = AvatarAppearance { display_name: format!("P{}", account.0), tint: [1, 2, 3], appearance_ref: None };
        w.prepare_resume(account, spot);
        w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now)
            .expect("joins");
        Seat { entity, rx }
    }

    fn spots(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<SavedLocation> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Msg::Spot(spot) = msg {
                out.push(spot);
            }
        }
        out
    }

    fn spot(account: u64, area: &str, at: (f32, f32), story: &[u8], resumable: bool) -> SavedLocation {
        SavedLocation::new(AccountId(account), GameId::Ff7, area, [at.0, at.1, 0.0], 0.75, story.to_vec(), resumable, 0)
    }

    /// Answer the `AreaChange` the player was sent, as its client does.
    fn arrive(w: &mut World, s: &mut Seat, now: Instant) -> ServerMsg {
        let offer = s.drain().into_iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).expect("an AreaChange");
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = &offer else { unreachable!() };
        assert_eq!(w.join_area(s.entity, area, fingerprint, provenance, now), TravelReply::Done);
        offer
    }

    fn walk(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) {
        *now += Duration::from_millis(700);
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now);
    }

    #[test]
    fn a_player_comes_back_to_the_spot_it_left_in_another_area() {
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let saved = spot(7, YARD, (40.0, 30.0), &[5], true);
        let mut s = join(&mut w, 7, &[5], Some(saved), t0);
        assert_eq!(w.transit_target(s.entity), Some(YARD.to_string()), "sent to the yard at once, as a story entry is");
        let offer = arrive(&mut w, &mut s, t0);
        let ServerMsg::AreaChange { arrival, arrival_facing, .. } = offer else { unreachable!() };
        assert_eq!((arrival.x, arrival.y), (40.0, 30.0), "on the spot it left");
        assert!((arrival_facing - 0.75).abs() < 1e-6, "facing the way it faced");
        assert_eq!(w.area_of(s.entity), Some((YARD.to_string(), false)));
        // The file already holds this spot: arriving on it, and standing there, writes nothing.
        w.save_locations(t0 + SAVE_EVERY);
        assert!(spots(&mut rx).is_empty());
        // A step away is saved, still resumable (the judgement came with the spot).
        let mut now = t0;
        walk(&mut w, &s, 60.0, 30.0, &mut now);
        w.save_locations(now + SAVE_EVERY);
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1, "{saved:?}");
        assert_eq!((saved[0].area.as_str(), saved[0].pos[0], saved[0].pos[1], saved[0].resumable), (YARD, 60.0, 30.0, true));
        assert!(w.take_logs().iter().any(|l| l.text().contains("resumed account=7")));
    }

    #[test]
    fn a_spot_in_the_entry_area_replaces_the_spawn_point_without_any_travel() {
        let (mut w, _rx) = world();
        let t0 = Instant::now();
        let s = join(&mut w, 8, &[], Some(spot(8, HALL, (-30.0, 25.0), &[], true)), t0);
        assert_eq!(w.transit_target(s.entity), None);
        assert_eq!(w.snapshot_of(s.entity).map(|snap| (snap.pos.x, snap.pos.y)), Some((-30.0, 25.0)));
        assert!((w.snapshot_of(s.entity).unwrap().facing - 0.75).abs() < 1e-6, "facing the way it faced");
    }

    #[test]
    fn a_spot_saved_under_another_story_or_not_resumable_is_not_used_and_closes_nothing() {
        let t0 = Instant::now();
        for (story_now, saved) in [
            (vec![6u8], spot(9, YARD, (40.0, 30.0), &[5], true)),
            (vec![5u8], spot(9, YARD, (40.0, 30.0), &[5], false)),
            (vec![5u8], spot(9, "nowhere", (40.0, 30.0), &[5], true)),
        ] {
            let (mut w, _rx) = world();
            let s = join(&mut w, 9, &story_now, Some(saved.clone()), t0);
            assert_eq!(w.transit_target(s.entity), None, "{saved:?}");
            assert_eq!(w.area_of(s.entity), Some((HALL.to_string(), false)));
            assert!(w.take_logs().iter().any(|l| l.text().contains("saved spot not used account=9")), "{saved:?}");
            // Nothing was closed: a walk through the hall's real exit still leads to the yard.
            let mut now = t0;
            walk(&mut w, &s, EXIT_X - 50.0, 0.0, &mut now);
            walk(&mut w, &s, EXIT_X + 50.0, 0.0, &mut now);
            assert_eq!(w.transit_target(s.entity), Some(YARD.to_string()), "{saved:?}");
        }
    }

    #[test]
    fn a_moving_player_is_saved_every_few_seconds_and_when_it_leaves() {
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let s = join(&mut w, 10, &[], None, t0);
        let mut now = t0;
        w.save_locations(now);
        assert_eq!(spots(&mut rx).len(), 1, "the first tick saves where it joined");
        walk(&mut w, &s, -80.0, 10.0, &mut now);
        w.save_locations(now);
        assert!(spots(&mut rx).is_empty(), "not again within {SAVE_EVERY:?}");
        now += SAVE_EVERY;
        w.save_locations(now);
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1);
        assert_eq!((saved[0].pos[0], saved[0].pos[1]), (-80.0, 10.0));
        now += SAVE_EVERY;
        w.save_locations(now);
        assert!(spots(&mut rx).is_empty(), "standing still writes nothing");
        walk(&mut w, &s, -60.0, 10.0, &mut now);
        assert!(w.leave(s.entity));
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1, "leaving saves the last step at once");
        assert_eq!((saved[0].area.as_str(), saved[0].pos[0]), (HALL, -60.0));
    }

    #[test]
    fn a_flag_changes_nothing_but_a_new_entry_does() {
        // Where a story stands is decided by its moment (the first byte here), not by its flags (the second).
        let (mut w, mut rx) = world();
        let entry: StoryEntryFn = Arc::new(|story: &[u8]| match story.first() {
            Some(33) => Some(YARD.to_string()),
            Some(40) => Some("elsewhere".to_string()),
            _ => None,
        });
        w.resume.entry_of = Some(entry);
        let t0 = Instant::now();
        // It reached the yard under story 33 and walked back into the hall: its saved spot there is resumable.
        let s = join(&mut w, 15, &[33, 0], Some(spot(15, HALL, (-30.0, 25.0), &[33, 0], true)), t0);
        assert_eq!(w.transit_target(s.entity), None);
        let mut now = t0;
        // A flag of the story changes (a choice, a battle won), the moment does not: still resumable.
        w.players.get_mut(&s.entity).unwrap().progression = vec![33, 1];
        walk(&mut w, &s, -50.0, 25.0, &mut now);
        w.save_locations(now);
        let saved = spots(&mut rx);
        assert_eq!((saved.len(), saved[0].resumable), (1, true), "{saved:?}");
        // The story moves to a moment that stands somewhere this player has not been: not resumable.
        w.players.get_mut(&s.entity).unwrap().progression = vec![40];
        walk(&mut w, &s, -70.0, 25.0, &mut now);
        w.save_locations(now + SAVE_EVERY);
        let saved = spots(&mut rx);
        assert_eq!((saved.len(), saved[0].resumable), (1, false), "{saved:?}");
    }

    #[test]
    fn a_player_whose_join_never_finished_saves_nothing_when_removed() {
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let s = join(&mut w, 16, &[], None, t0);
        // As a failed scene recovery removes it: before the end of `join` gave it a tracker.
        w.resume.tracked.remove(&s.entity);
        assert!(w.leave(s.entity));
        assert!(spots(&mut rx).is_empty(), "a spawn point never replaces the spot the account saved");
    }

    #[test]
    fn a_story_step_taken_standing_still_is_saved_with_the_spot() {
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let s = join(&mut w, 14, &[2], None, t0);
        w.save_locations(t0);
        assert_eq!(spots(&mut rx).len(), 1);
        // The story moves on (a talk, a scene) while the player does not move: leaving must save the new story with the
        // spot, or the next login would find the spot stale.
        w.players.get_mut(&s.entity).unwrap().progression = vec![3];
        assert!(w.leave(s.entity));
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1, "{saved:?}");
        assert_eq!(saved[0].story, [3]);
    }

    #[test]
    fn where_the_story_stands_elsewhere_the_spot_is_not_resumable_until_the_player_has_been_there() {
        // Story 33 stands in the yard (this test world has no scene host, so nothing sends the player there: it stands
        // in the hall with that story, as a player stands in `nrthmk` right after the reactor's finale).
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let mut s = join(&mut w, 11, &[33], None, t0);
        let mut now = t0;
        w.save_locations(now);
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1);
        assert!(!saved[0].resumable, "its story stands in the yard and it has not been there: a login sends it on");
        // It walks into the yard, where its story stands: the arrival is saved resumable.
        walk(&mut w, &s, EXIT_X - 50.0, 0.0, &mut now);
        walk(&mut w, &s, EXIT_X + 50.0, 0.0, &mut now);
        assert_eq!(w.transit_target(s.entity), Some(YARD.to_string()));
        w.save_locations(now + SAVE_EVERY);
        assert!(spots(&mut rx).is_empty(), "nothing is saved between two areas");
        arrive(&mut w, &mut s, now);
        let saved = spots(&mut rx);
        assert_eq!((saved.len(), saved[0].area.as_str(), saved[0].resumable), (1, YARD, true));
        // It walks back into the hall under the same story (as from `md8_1` on to `md8_2`): having stood where its story
        // stands, it may resume in the hall now, and its last step there is saved so when it leaves.
        walk(&mut w, &s, -EXIT_X + 50.0, 0.0, &mut now);
        walk(&mut w, &s, -EXIT_X - 50.0, 0.0, &mut now);
        assert_eq!(w.transit_target(s.entity), Some(HALL.to_string()));
        arrive(&mut w, &mut s, now);
        walk(&mut w, &s, 150.0, 0.0, &mut now);
        assert!(w.leave(s.entity));
        let saved = spots(&mut rx);
        assert!(!saved.is_empty() && saved.iter().all(|spot| spot.area == HALL && spot.resumable), "{saved:?}");
    }

    #[test]
    fn a_resumed_spot_beyond_the_story_entry_stays_resumable_in_the_next_session() {
        // Story 33 stands in the yard. This player reached it, walked back into the hall and logged out there: its spot
        // was saved resumable. Logging in again puts it in the hall, and saving again keeps it resumable.
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let s = join(&mut w, 13, &[33], Some(spot(13, HALL, (-30.0, 25.0), &[33], true)), t0);
        assert_eq!(w.transit_target(s.entity), None, "resumed in the hall, not sent to the yard");
        let mut now = t0;
        walk(&mut w, &s, -50.0, 25.0, &mut now);
        w.save_locations(now);
        let saved = spots(&mut rx);
        assert_eq!(saved.len(), 1);
        assert!(saved[0].resumable, "the judgement carried over: a third login resumes here too");
    }

    #[test]
    fn a_party_members_spot_is_not_resumable() {
        let (mut w, mut rx) = world();
        let t0 = Instant::now();
        let leader = join(&mut w, 20, &[], None, t0);
        let member = join(&mut w, 21, &[], None, t0);
        assert_eq!(w.party_invite(leader.entity, member.entity, t0), PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(member.entity, invite, t0), PartyOutcome::Done);
        w.save_locations(t0 + SAVE_EVERY);
        let saved = spots(&mut rx);
        let of = |account: u64| saved.iter().find(|s| s.account_id.0 == account).map(|s| s.resumable);
        assert_eq!(of(20), Some(true), "the leader governs its own place");
        assert_eq!(of(21), Some(false), "the member's place was its leader's: {saved:?}");
    }
}
