//! Pure client-side game state: own-player prediction, remote entities, counters.
//!
//! Nothing here touches the network, the clock or the GPU: callers pass `now` (seconds on a
//! monotonic local clock) and server messages, so everything is unit-testable. Every position
//! that comes from the network is untrusted and checked for finiteness before use, and every
//! collection is bounded.
//!
//! # How the own player is reconciled with the server
//!
//! Intents carry absolute positions and the server processes them in order. It applies an intent
//! if the player can reach the position (speed budget, walkable surface) and otherwise answers
//! `MoveRejected { seq, corrected }` with the position it keeps. There is no acknowledgement for
//! an accepted intent; the server's position afterwards is simply the last accepted one. So:
//!
//! * A rejection of the NEWEST intent we sent is the server's final word: nothing newer is in
//!   flight, so we snap to `corrected` and announce facing/animation again (the server only
//!   tracks those from accepted moves).
//! * A rejection of an OLDER intent is stale. Intents sent after it are still being judged, and
//!   the server may well accept them, which makes `corrected` out of date. Applying it would pull
//!   the player back, after which the next intent moves the server's entity backwards too (other
//!   players see it rubber-band). Stale rejections are counted but not applied; whatever the
//!   server finally holds is told by the rejection of the newest intent, by silence (accepted),
//!   or by an `EntityUpdate` of our own entity.
//! * The server's view of our entity (`EntityUpdate`) is only adopted when we have been quiet
//!   for a while and the two diverge. It is remembered until then, because the server sends only
//!   changes: an update that arrives while we are busy would otherwise be lost for good.

use crate::battle::BattleClient;
use crate::interp::{normalize_angle, Pose, Sample, SnapshotBuffer};
use crate::party::{nearest_peer, PartyMirror};
use crate::progress::ProgressMirror;
use crate::story::StoryMirror;
use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, Pos};
use fmv_protocol::{NpcSnapshot, ServerMsg, MAX_NPCS_PER_AREA, NPC_NAME_CHARS, NPC_REF_CHARS};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

/// `MoveIntent` is sent at most this often (20 Hz).
pub const MIN_INTENT_INTERVAL_S: f64 = 0.05;
/// Tolerance so a 3-frame cadence at 60 fps is not defeated by floating point rounding.
const RATE_EPS_S: f64 = 1e-6;
/// Remote entities tracked for rendering (extra ones are ignored).
pub const MAX_REMOTES: usize = 256;
/// Distinct peer ids remembered for the SMOKE `peers_seen` counter.
pub const MAX_PEERS_SEEN: usize = 4096;
/// After this long without sending movement the server's view of our entity may correct ours.
const RECONCILE_IDLE_S: f64 = 0.5;
/// ...if it differs by more than this many units.
const RECONCILE_EPS: f32 = 1.0;
const POS_EPS: f32 = 1e-3;
const FACING_EPS: f32 = 1e-3;

/// One outgoing `MoveIntent`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intent {
    pub seq: u32,
    pub pos: Pos,
    pub facing: f32,
    pub anim: AnimState,
}

/// The locally predicted own player.
#[derive(Debug, Clone)]
pub struct OwnPlayer {
    pos: Pos,
    facing: f32,
    anim: AnimState,
    /// Sequence number of the newest intent sent (0 before the first).
    seq: u32,
    last_intent_t: f64,
    /// What the server was last told (or corrected to); intents are only sent on change.
    last_sent: (Pos, f32, AnimState),
    /// A correction placed us where the server's facing/animation no longer describe us: the next
    /// intent goes out even if nothing else changed.
    resync: bool,
    /// The intent that was sent only to resync, while its outcome is unknown.
    resync_seq: Option<u32>,
    /// The server's latest word on where our entity is (from an `EntityUpdate`), kept until we
    /// are quiet enough to adopt it. Cleared by every new intent, which supersedes it.
    server_pos: Option<Pos>,
}

impl OwnPlayer {
    pub fn new(pos: Pos) -> Self {
        Self::at(pos, 0.0)
    }

    /// A player standing at `pos`, heading `facing`, that the server knows about as it is (nothing
    /// to announce): what it is after arriving in a new area, where the server put it.
    pub fn at(pos: Pos, facing: f32) -> Self {
        let facing = if facing.is_finite() { facing } else { 0.0 };
        Self {
            pos,
            facing,
            anim: AnimState::Idle,
            seq: 0,
            last_intent_t: f64::NEG_INFINITY,
            last_sent: (pos, facing, AnimState::Idle),
            resync: false,
            resync_seq: None,
            server_pos: None,
        }
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }
    pub fn facing(&self) -> f32 {
        self.facing
    }
    pub fn anim(&self) -> AnimState {
        self.anim
    }

    /// Adopt the result of local prediction.
    pub fn set_motion(&mut self, pos: Pos, facing: f32, anim: AnimState) {
        if pos.is_finite() {
            self.pos = pos;
        }
        if facing.is_finite() {
            self.facing = facing;
        }
        self.anim = anim;
    }

    /// Place the player where the server says it is. The position is now known to the server, so
    /// it is not echoed back as movement; the server's facing and animation are NOT known, so the
    /// next intent is sent regardless of what changed.
    fn snap(&mut self, pos: Pos) {
        if pos.is_finite() {
            self.pos = pos;
            self.last_sent.0 = pos;
            self.server_pos = None;
            self.resync = true;
        }
    }

    /// `MoveRejected { seq, corrected }`. Applied (returns true) only when `seq` is the newest
    /// intent we sent; a rejection of an older one is stale (see the module docs) and ignored.
    pub fn on_rejected(&mut self, seq: u32, corrected: Pos) -> bool {
        // Before the first intent no rejection can be about us; `seq` alone cannot tell (both 0).
        let sent_any = self.last_intent_t.is_finite();
        if !sent_any || seq != self.seq || !corrected.is_finite() {
            return false;
        }
        let refused_resync = self.resync_seq == Some(seq);
        self.snap(corrected);
        self.resync_seq = None;
        if refused_resync {
            // Announcing again would only be refused again: never loop on a broken correction.
            self.resync = false;
        }
        true
    }

    /// Predicted walk: move by `speed * dt` in direction `dir`, validated locally with
    /// `AreaRuntime::validate_move`. When the full step is blocked the player slides along the
    /// wall by trying each axis separately. If the adapter says we must stand somewhere else
    /// (`MoveResult::Corrected` to a different spot: we were off the walkable surface, or at the
    /// wrong height) we are placed there, as the server will do. Returns true if the position
    /// changed.
    #[allow(dead_code)] // the plain entry point (the window uses `walk_as`); tests and bots walk with it
    pub fn walk(&mut self, area: &dyn AreaRuntime, dir: (f32, f32), speed: f32, dt: f32) -> bool {
        self.walk_as(area, dir, speed, dt, AnimState::Walk)
    }

    /// [`OwnPlayer::walk`] announcing `moving` (`Walk` or `Run`) as the animation while the player
    /// moves: the window runs by default and walks while Shift is held.
    pub fn walk_as(&mut self, area: &dyn AreaRuntime, dir: (f32, f32), speed: f32, dt: f32, moving: AnimState) -> bool {
        let len = dir.0.hypot(dir.1);
        if !(len.is_finite() && len > 1e-6 && dt.is_finite() && dt > 0.0 && speed.is_finite()) {
            self.anim = AnimState::Idle;
            return false;
        }
        let (ux, uy) = (dir.0 / len, dir.1 / len);
        self.facing = uy.atan2(ux);
        let (dx, dy) = (ux * speed * dt, uy * speed * dt);
        let mut relocated = false;
        for (cx, cy) in [(dx, dy), (dx, 0.0), (0.0, dy)] {
            if cx == 0.0 && cy == 0.0 {
                continue;
            }
            let to = Pos::new(self.pos.x + cx, self.pos.y + cy, self.pos.z);
            match area.validate_move(self.pos, to) {
                MoveResult::Accepted(p) if p.is_finite() => {
                    self.pos = p;
                    self.anim = moving;
                    return true;
                }
                // Refused AND told to stand elsewhere. (A correction onto the spot we already
                // occupy, the usual wall, changes nothing and falls through to the slide.)
                MoveResult::Corrected(p) if p.is_finite() && p != self.pos => {
                    let moved = p.dist_xy(self.pos) > POS_EPS;
                    self.pos = p;
                    relocated = true;
                    if moved {
                        break; // placed somewhere else entirely: no sliding from there this frame
                    }
                    // Only the height changed: keep trying the other axes from the new height.
                }
                _ => {}
            }
        }
        self.anim = AnimState::Idle;
        relocated
    }

    /// The next `MoveIntent` to send, if one is due: at most every `MIN_INTENT_INTERVAL_S`, and
    /// only when position, facing or animation differ from what the server last knew (or a
    /// correction asked for a resync).
    pub fn next_intent(&mut self, now: f64) -> Option<Intent> {
        // Every call, not only when an intent is due: adoption must not wait for the rate limit.
        self.adopt_server_pos_if_quiet(now);
        if now - self.last_intent_t < MIN_INTENT_INTERVAL_S - RATE_EPS_S {
            return None;
        }
        let (p, f, a) = self.last_sent;
        let changed = self.resync
            || self.pos.dist_xy(p) > POS_EPS
            || (self.pos.z - p.z).abs() > POS_EPS
            || (self.facing - f).abs() > FACING_EPS
            || self.anim != a;
        if !changed {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        self.last_intent_t = now;
        self.last_sent = (self.pos, self.facing, self.anim);
        self.resync_seq = self.resync.then_some(self.seq);
        self.resync = false;
        self.server_pos = None; // this intent supersedes what the server said before it
        Some(Intent {
            seq: self.seq,
            pos: self.pos,
            facing: self.facing,
            anim: self.anim,
        })
    }

    /// The server's view of our entity (from an update). Prediction wins while we are moving;
    /// once we have been quiet for a while a diverged server position is adopted. Until then it
    /// is remembered, since the server will not repeat it.
    fn note_server_pos(&mut self, now: f64, server_pos: Pos) {
        self.server_pos = Some(server_pos);
        self.adopt_server_pos_if_quiet(now);
    }

    fn adopt_server_pos_if_quiet(&mut self, now: f64) {
        let Some(server) = self.server_pos else { return };
        if now - self.last_intent_t <= RECONCILE_IDLE_S {
            return;
        }
        self.server_pos = None;
        if self.pos.dist_xy(server) > RECONCILE_EPS {
            self.pos = server;
            self.last_sent.0 = server;
        }
    }
}

/// A remote entity as the renderer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteView {
    pub id: EntityId,
    pub name: String,
    pub tint: [u8; 3],
    /// The look the avatar names (untrusted text, at most [`MAX_APPEARANCE_REF`] bytes): the
    /// window looks it up in its table of characters and never trusts it for anything else.
    pub appearance: Option<String>,
    pub pose: Pose,
}

#[derive(Debug, Clone)]
struct Remote {
    name: String,
    tint: [u8; 3],
    appearance: Option<String>,
    buf: SnapshotBuffer,
}

/// A non-player character of the area as the renderer sees it: static scenery that stands where
/// the server's snapshot of the area put it. Every text is cleaned and bounded on arrival (it is
/// the server's, so untrusted); `model` and `idle_animation` are references into this machine's
/// own game files that [`crate::npc::npc_appearance`] resolves.
#[derive(Debug, Clone, PartialEq)]
pub struct NpcView {
    /// Index in the area's NPC list (not an entity id).
    pub id: u32,
    /// What the area's data calls it (FF7: the script entity name, `gu0`): its label.
    pub name: String,
    /// Adapter-defined model reference (FF7: the `char.lgp` skeleton stem).
    pub model: String,
    /// The clip that shows it standing, if the data names one.
    pub idle_animation: Option<String>,
    pub pos: Pos,
    pub facing: f32,
}

/// Clean untrusted server text and clamp it to `max` characters.
fn clean_text(s: &str, max: usize) -> String {
    crate::net::clean(s).chars().take(max).collect()
}

impl NpcView {
    /// `None` for an NPC the renderer must not touch: a position or heading that is not finite.
    fn from_snapshot(n: NpcSnapshot) -> Option<Self> {
        if !n.pos.is_finite() || !n.facing.is_finite() {
            return None;
        }
        Some(Self {
            id: n.id,
            name: clean_text(&n.name, NPC_NAME_CHARS),
            model: clean_text(&n.model, NPC_REF_CHARS),
            idle_animation: n.idle_animation.map(|a| clean_text(&a, NPC_REF_CHARS)).filter(|a| !a.is_empty()),
            pos: n.pos,
            facing: normalize_angle(n.facing),
        })
    }
}

/// Longest `appearance_ref` kept from the network (the server's own limit); a longer one is
/// dropped, which draws the default look.
pub const MAX_APPEARANCE_REF: usize = 64;

/// Sightings kept until the loop takes them ([`ClientState::drain_peer_sightings`]); beyond this the oldest are
/// dropped (a window never takes them).
const MAX_PEER_SIGHTINGS: usize = 64;

/// What this client saw of another player the first time it appeared in the area, or when its colour or its
/// binding (the `appearance_ref` the game draws it with) changed: the headless bot says it as one
/// `headless: peer` line, which is how a smoke test sees each player's binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSighting {
    pub entity: EntityId,
    /// The player's name, cleaned.
    pub name: String,
    pub tint: [u8; 3],
    /// The `appearance_ref` of its entity, as the server said it (bounded by [`MAX_APPEARANCE_REF`]).
    pub binding: Option<String>,
    /// The area it was seen in (ours: the one this client joined and checked against its own content).
    pub area: AreaId,
}

/// An `appearance_ref` from the network as stored: at most [`MAX_APPEARANCE_REF`] bytes.
fn kept_appearance(avatar: &AvatarAppearance) -> Option<String> {
    avatar.appearance_ref.as_deref().filter(|r| r.len() <= MAX_APPEARANCE_REF).map(str::to_string)
}

#[derive(Debug)]
pub struct ClientState {
    self_entity: EntityId,
    /// The area we are in (verified against local content: during the handshake for the first
    /// one, when the server offers it for every later one, see [`ClientState::change_area`]).
    area: AreaId,
    pub own: OwnPlayer,
    /// Our party and pending invites, as the server announced them.
    pub party: PartyMirror,
    /// The battle we are in, replayed from the server's messages (see `crate::battle`).
    pub battle: BattleClient,
    /// What the server says our character has earned (level, experience, AP, gil; see
    /// `crate::progress`). Empty until it says (a server without battles never does).
    pub progress: ProgressMirror,
    /// Where the server says our story stands (see `crate::story`). Empty until it says: a new account.
    pub story: StoryMirror,
    remotes: BTreeMap<EntityId, Remote>,
    /// The non-player characters of the current area (from its `AreaSnapshot`).
    npcs: Vec<NpcView>,
    /// Counts the times the NPC list was replaced (a snapshot, a change of area): a renderer that
    /// keeps something per set of NPCs knows by it when to rebuild.
    npc_epoch: u64,
    /// Peers seen in the CURRENT area (cleared when the area changes).
    peers_seen: BTreeSet<EntityId>,
    updates: u64,
    rejected: u64,
    /// How many times the server moved us on to another area (a refused transfer and the way back
    /// to where we were do not count).
    area_changes: u64,
    last_rtt: Option<Duration>,
    /// The look the server shows for our own entity (from its snapshots).
    own_appearance: Option<String>,
    /// The identity colour the server shows for our own entity (from its snapshots).
    own_tint: Option<[u8; 3]>,
    /// What was last reported of each other player's colour and binding in the CURRENT area (a full snapshot
    /// of the area does not forget it: an unchanged player is not reported again; a change of area does).
    peer_looks: BTreeMap<EntityId, ([u8; 3], Option<String>)>,
    /// Players seen or changed, until the loop takes them; bounded by [`MAX_PEER_SIGHTINGS`].
    peer_sightings: VecDeque<PeerSighting>,
}

impl ClientState {
    /// `account` is ours: party events name accounts, so it is how the party mirror recognises
    /// itself in them.
    pub fn new(self_entity: EntityId, account: AccountId, spawn: Pos, area: AreaId) -> Self {
        Self {
            self_entity,
            area,
            own: OwnPlayer::new(spawn),
            party: PartyMirror::new(account),
            battle: BattleClient::new(account),
            progress: ProgressMirror::default(),
            story: StoryMirror::default(),
            remotes: BTreeMap::new(),
            npcs: Vec::new(),
            npc_epoch: 0,
            peers_seen: BTreeSet::new(),
            updates: 0,
            rejected: 0,
            area_changes: 0,
            last_rtt: None,
            own_appearance: None,
            own_tint: None,
            peer_looks: BTreeMap::new(),
            peer_sightings: VecDeque::new(),
        }
    }

    pub fn self_entity(&self) -> EntityId {
        self.self_entity
    }
    /// The area we are in.
    #[cfg(test)]
    pub fn area(&self) -> &AreaId {
        &self.area
    }
    /// How many times the server moved us on to another area.
    pub fn area_changes(&self) -> u64 {
        self.area_changes
    }
    /// The non-player characters of the current area, in the server's order.
    pub fn npc_views(&self) -> &[NpcView] {
        &self.npcs
    }
    /// Changes whenever the list [`ClientState::npc_views`] answers was replaced.
    pub fn npc_epoch(&self) -> u64 {
        self.npc_epoch
    }

    /// The server moved us to `area` and we hold it (the session checked it against local content
    /// and answered `JoinArea`): everything that belongs to the old area goes (the other players,
    /// the NPCs, the peers seen), and we stand at `arrival` heading `facing` (the server's own
    /// placement; it repeats it in the `AreaSnapshot` that follows). The party, the battle mirror
    /// and the counters are ours, not the area's, and stay. `forward` is false for the way back
    /// after a transfer we refused (it is not a change).
    pub fn change_area(&mut self, area: AreaId, arrival: Pos, facing: f32, forward: bool) {
        self.area = area;
        self.remotes.clear();
        self.npcs.clear();
        self.npc_epoch += 1;
        self.peers_seen.clear();
        self.peer_looks.clear();
        self.own = OwnPlayer::at(arrival, facing);
        if forward {
            self.area_changes += 1;
        }
    }
    /// Number of `EntityUpdate` messages received.
    pub fn updates(&self) -> u64 {
        self.updates
    }
    /// Number of `MoveRejected` messages received (every one counts, applied or stale).
    pub fn rejected(&self) -> u64 {
        self.rejected
    }
    /// Distinct entities other than ourselves seen in any snapshot, update or join.
    pub fn peers_seen(&self) -> usize {
        self.peers_seen.len()
    }
    pub fn remote_count(&self) -> usize {
        self.remotes.len()
    }
    pub fn last_rtt(&self) -> Option<Duration> {
        self.last_rtt
    }
    pub fn set_rtt(&mut self, rtt: Duration) {
        self.last_rtt = Some(rtt);
    }
    /// The look the server shows for our own entity (`appearance_ref`), if it said.
    pub fn own_appearance(&self) -> Option<&str> {
        self.own_appearance.as_deref()
    }

    /// The identity colour the server shows for `entity`, ours or another player's in this area, from its snapshots
    /// (`None` for one it has not told us of): what the battle scene draws a party member's body in.
    pub fn tint_of(&self, entity: EntityId) -> Option<[u8; 3]> {
        if entity == self.self_entity {
            self.own_tint
        } else {
            self.remotes.get(&entity).map(|r| r.tint)
        }
    }

    /// The other players first seen (or whose colour or binding changed) since the last call, oldest first.
    pub fn drain_peer_sightings(&mut self) -> Vec<PeerSighting> {
        self.peer_sightings.drain(..).collect()
    }

    /// Apply one server message. Returns `Some(reason)` when the session must end: the server
    /// ended it, or broke the protocol in a way that would mix content (an area is only ever
    /// entered after checking it against local content: the first one in the handshake, every later
    /// one through [`ClientState::change_area`], which the session calls on an `AreaChange`).
    pub fn apply(&mut self, now: f64, msg: ServerMsg) -> Option<String> {
        match msg {
            ServerMsg::AreaSnapshot { area, entities, npcs, .. } => {
                if area != self.area {
                    return Some(format!(
                        "protocol error: the server sent a snapshot of area `{}` while we are in `{}`",
                        crate::net::clean(&area.to_string()),
                        self.area
                    ));
                }
                // A full snapshot replaces the set of remote entities and the area's NPCs.
                self.remotes.clear();
                for e in entities {
                    self.upsert(now, e);
                }
                self.npcs = npcs.into_iter().take(MAX_NPCS_PER_AREA).filter_map(NpcView::from_snapshot).collect();
                self.npc_epoch += 1;
            }
            // Handled before it gets here: the session answers it (JoinArea / CannotJoin) and then
            // calls `change_area`. Nothing for the state to do with the offer itself.
            ServerMsg::AreaChange { .. } => {}
            ServerMsg::EntityUpdate { entities, .. } => {
                self.updates += 1;
                for e in entities {
                    self.upsert(now, e);
                }
            }
            ServerMsg::PlayerJoined { snapshot } => self.upsert(now, snapshot),
            ServerMsg::PlayerLeft { entity } => {
                self.remotes.remove(&entity);
            }
            ServerMsg::MoveRejected { seq, corrected } => {
                self.rejected += 1;
                self.own.on_rejected(seq, corrected);
            }
            ServerMsg::AreaManifest { .. } => {
                // The manifest is a handshake message: honouring a second one would skip the
                // local content check for whatever it offers.
                return Some(format!(
                    "protocol error: the server offered an area manifest after we joined `{}`",
                    self.area
                ));
            }
            // The server's own sentence, cleaned, is all the player is told: nothing here decides
            // anything from the words, and the code has no use yet for a session that is over.
            ServerMsg::Reject { reason, code: _ } => {
                return Some(format!("server ended the session: {}", crate::net::clean(&reason)));
            }
            // `Welcome` and `Pong` carry nothing the game state needs after the handshake.
            ServerMsg::Welcome { .. } | ServerMsg::Pong { .. } => {}
            ServerMsg::InstancePlacement { placement } => self.party.on_placement(placement),
            ServerMsg::PartyEvent { seq, event, names } => self.party.on_event(seq, event, names),
            ServerMsg::PartyState { seq, party, invites, names } => self.party.on_state(seq, party, invites, names),
            ServerMsg::PartyRejected { error } => self.party.on_rejected(&error),
            // Battle traffic never ends the session: the battle mirror judges it (and says so
            // loudly when the replay diverges).
            msg @ (ServerMsg::EncounterStart { .. }
            | ServerMsg::BattleEvent { .. }
            | ServerMsg::BattleEnd { .. }
            | ServerMsg::BattleRejected { .. }) => self.battle.on_message(now, msg),
            // Our own character's numbers (protocol v5). Like battle traffic it never ends the session:
            // it is the server's word about us, shown and counted, and nothing here acts on it.
            msg @ ServerMsg::CharacterProgress { .. } => self.progress.on_message(now, msg),
            // What our account carries (protocol v6): the battle mirror keeps it, because the command
            // menu's Item list is made of it. Like the progress it is the server's word and never ends
            // the session.
            ServerMsg::Inventory { items } => self.battle.on_inventory(now, items),
            // Where our story stands (the server's word, never ends the session): the mirror keeps the
            // blob for whatever reads story variables on this side (see `crate::story`).
            ServerMsg::StoryUpdate { progress, advance } => self.story.on_update(progress, advance),
        }
        None
    }

    fn upsert(&mut self, now: f64, snap: EntitySnapshot) {
        if !snap.pos.is_finite() || !snap.facing.is_finite() {
            return;
        }
        if snap.id == self.self_entity {
            self.own.note_server_pos(now, snap.pos);
            self.own_appearance = kept_appearance(&snap.avatar);
            self.own_tint = Some(snap.avatar.tint);
            return;
        }
        if self.peers_seen.len() < MAX_PEERS_SEEN {
            self.peers_seen.insert(snap.id);
        }
        if !self.remotes.contains_key(&snap.id) && self.remotes.len() >= MAX_REMOTES {
            return;
        }
        let name = AvatarAppearance::sanitize_name(&snap.avatar.display_name).unwrap_or_else(|| "?".to_string());
        // The first time this player is seen in the area, and whenever its colour or binding changes: a note.
        let shown = (snap.avatar.tint, kept_appearance(&snap.avatar));
        if self.peer_looks.get(&snap.id) != Some(&shown) && (self.peer_looks.len() < MAX_PEERS_SEEN || self.peer_looks.contains_key(&snap.id)) {
            self.peer_looks.insert(snap.id, shown.clone());
            if self.peer_sightings.len() >= MAX_PEER_SIGHTINGS {
                self.peer_sightings.pop_front();
            }
            self.peer_sightings.push_back(PeerSighting { entity: snap.id, name: name.clone(), tint: shown.0, binding: shown.1, area: self.area.clone() });
        }
        let r = self.remotes.entry(snap.id).or_insert_with(|| Remote {
            name: String::new(),
            tint: snap.avatar.tint,
            appearance: None,
            buf: SnapshotBuffer::default(),
        });
        r.name = name;
        r.tint = snap.avatar.tint;
        r.appearance = kept_appearance(&snap.avatar);
        r.buf.push(Sample {
            t: now,
            pose: Pose {
                pos: snap.pos,
                // The server relays any finite angle; keep it a plain angle for interpolation.
                facing: normalize_angle(snap.facing),
                anim: snap.anim,
            },
        });
    }

    /// The visible remote player nearest to us (distance in area coordinates) who is not already
    /// in our party: who "invite the nearest player" means. `None` if there is nobody to invite.
    pub fn nearest_invitable(&self, render_t: f64) -> Option<EntityId> {
        let views = self.remote_views(render_t);
        nearest_peer(self.own.pos(), views.iter().map(|v| (v.id, v.pose.pos)), |id| {
            self.party.is_party_entity(id)
        })
    }

    /// Interpolated remote entities at `render_t`, in a stable (id) order.
    pub fn remote_views(&self, render_t: f64) -> Vec<RemoteView> {
        self.remotes
            .iter()
            .filter_map(|(id, r)| {
                r.buf.sample(render_t).map(|pose| RemoteView {
                    id: *id,
                    name: r.name.clone(),
                    tint: r.tint,
                    appearance: r.appearance.clone(),
                    pose,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter};
    use fmv_core::{Fingerprint, GameId};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use fmv_protocol::RejectCode;
    use std::f32::consts::PI;

    fn snap(id: u64, x: f32, y: f32) -> EntitySnapshot {
        EntitySnapshot {
            id: EntityId(id),
            pos: Pos::new(x, y, 0.0),
            facing: 0.0,
            anim: AnimState::Walk,
            avatar: AvatarAppearance {
                display_name: format!("p{id}"),
                tint: [1, 2, 3],
                appearance_ref: None,
            },
        }
    }
    fn update(entities: Vec<EntitySnapshot>) -> ServerMsg {
        ServerMsg::EntityUpdate { tick: 1, entities }
    }
    fn area() -> Box<dyn AreaRuntime> {
        FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()
    }
    fn plaza() -> AreaId {
        AreaId {
            game: GameId::Fixture,
            key: "plaza".into(),
        }
    }
    fn state(spawn: Pos) -> ClientState {
        ClientState::new(EntityId(1), AccountId(1), spawn, plaza())
    }
    fn at(x: f32) -> Pos {
        Pos::new(x, 0.0, 0.0)
    }
    fn reject(seq: u32, corrected: Pos) -> ServerMsg {
        ServerMsg::MoveRejected { seq, corrected }
    }

    #[test]
    fn counts_updates_rejections_and_distinct_peers_excluding_self() {
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        s.apply(
            0.0,
            ServerMsg::AreaSnapshot {
                area: plaza(),
                tick: 0,
                entities: vec![snap(1, 0.0, 0.0), snap(2, 5.0, 5.0)],
                npcs: vec![],
            },
        );
        assert_eq!((s.peers_seen(), s.updates(), s.rejected()), (1, 0, 0));
        s.apply(
            0.1,
            update(vec![snap(1, 0.0, 0.0), snap(2, 6.0, 5.0), snap(3, 9.0, 9.0)]),
        );
        s.apply(0.2, update(vec![snap(2, 7.0, 5.0)]));
        s.apply(
            0.3,
            ServerMsg::PlayerJoined {
                snapshot: snap(4, 1.0, 1.0),
            },
        );
        // Counted even though no intent was ever sent (so it cannot be about us).
        s.apply(0.4, reject(9, Pos::new(10.0, 20.0, 0.0)));
        s.apply(0.5, ServerMsg::Pong { nonce: 1, tick: 1 });
        assert_eq!((s.peers_seen(), s.updates(), s.rejected()), (3, 2, 1));
        // A leaving peer disappears from rendering but stays counted as "seen".
        s.apply(0.6, ServerMsg::PlayerLeft { entity: EntityId(3) });
        assert_eq!((s.peers_seen(), s.remote_count()), (3, 2));
    }

    /// Another player's first appearance in the area, and each change of its colour or binding, is one
    /// sighting. An unchanged player in a full snapshot or an update is none (and never every frame); we are
    /// never one; a change of area starts over.
    #[test]
    fn a_player_is_sighted_when_first_seen_and_when_its_colour_or_binding_changes() {
        let with = |id: u64, tint: [u8; 3], binding: Option<&str>| EntitySnapshot {
            avatar: AvatarAppearance { display_name: format!("p{id}"), tint, appearance_ref: binding.map(str::to_string) },
            ..snap(id, 0.0, 0.0)
        };
        let sighting = |id: u64, tint: [u8; 3], binding: Option<&str>, area: &AreaId| PeerSighting {
            entity: EntityId(id),
            name: format!("p{id}"),
            tint,
            binding: binding.map(str::to_string),
            area: area.clone(),
        };
        let full = |entities| ServerMsg::AreaSnapshot { area: plaza(), tick: 0, entities, npcs: vec![] };
        let mut s = state(at(0.0));
        // The snapshot of the area: us (never a sighting) and one peer.
        s.apply(0.0, full(vec![snap(1, 0.0, 0.0), with(2, [9, 9, 9], Some("ff7/body/sturdy"))]));
        assert_eq!(s.drain_peer_sightings(), [sighting(2, [9, 9, 9], Some("ff7/body/sturdy"), &plaza())]);
        assert!(s.drain_peer_sightings().is_empty(), "taken once");
        // The same peer again, in a full snapshot and in updates: nothing, however often.
        s.apply(0.1, full(vec![snap(1, 0.0, 0.0), with(2, [9, 9, 9], Some("ff7/body/sturdy"))]));
        for t in 1..5 {
            s.apply(0.2 + f64::from(t), update(vec![with(2, [9, 9, 9], Some("ff7/body/sturdy"))]));
        }
        assert!(s.drain_peer_sightings().is_empty());
        // A player who joins, one whose binding changes, one whose colour changes, one whose appearance goes.
        s.apply(1.0, ServerMsg::PlayerJoined { snapshot: with(3, [1, 2, 3], None) });
        s.apply(1.1, update(vec![with(2, [9, 9, 9], Some("ff7/body/slim"))]));
        s.apply(1.2, update(vec![with(3, [7, 7, 7], None)]));
        s.apply(1.3, update(vec![with(2, [9, 9, 9], None)]));
        assert_eq!(
            s.drain_peer_sightings(),
            [
                sighting(3, [1, 2, 3], None, &plaza()),
                sighting(2, [9, 9, 9], Some("ff7/body/slim"), &plaza()),
                sighting(3, [7, 7, 7], None, &plaza()),
                sighting(2, [9, 9, 9], None, &plaza()),
            ]
        );
        // Leaving and coming back as the same thing is not a new sighting; coming back as something else is.
        s.apply(2.0, ServerMsg::PlayerLeft { entity: EntityId(3) });
        s.apply(2.1, ServerMsg::PlayerJoined { snapshot: with(3, [7, 7, 7], None) });
        assert!(s.drain_peer_sightings().is_empty());
        s.apply(2.2, ServerMsg::PlayerLeft { entity: EntityId(3) });
        s.apply(2.3, ServerMsg::PlayerJoined { snapshot: with(3, [7, 7, 7], Some("ff7/body/tall")) });
        assert_eq!(s.drain_peer_sightings(), [sighting(3, [7, 7, 7], Some("ff7/body/tall"), &plaza())]);
        // A binding too long for the protocol is not kept (the default look is drawn), as for every look.
        s.apply(3.0, update(vec![with(4, [5, 5, 5], Some(&"x".repeat(MAX_APPEARANCE_REF + 1)))]));
        assert_eq!(s.drain_peer_sightings(), [sighting(4, [5, 5, 5], None, &plaza())]);
        // The server moves us to another area: whoever is there is seen afresh, with that area named.
        let yard = AreaId { game: GameId::Fixture, key: "yard".into() };
        s.change_area(yard.clone(), at(0.0), 0.0, true);
        s.apply(4.0, ServerMsg::AreaSnapshot { area: yard.clone(), tick: 1, entities: vec![snap(1, 0.0, 0.0), with(2, [9, 9, 9], None)], npcs: vec![] });
        assert_eq!(s.drain_peer_sightings(), [sighting(2, [9, 9, 9], None, &yard)]);
    }

    /// A window never takes the sightings: they are bounded, the oldest go.
    #[test]
    fn the_sightings_are_bounded() {
        let mut s = state(at(0.0));
        for id in 2..(2 + 3 * MAX_PEER_SIGHTINGS as u64) {
            s.apply(0.0, update(vec![snap(id, 0.0, 0.0)]));
        }
        let kept = s.drain_peer_sightings();
        assert_eq!(kept.len(), MAX_PEER_SIGHTINGS);
        assert_eq!(kept.last().map(|p| p.entity), Some(EntityId(1 + 3 * MAX_PEER_SIGHTINGS as u64)), "the newest are kept");
    }

    #[test]
    fn the_newest_rejection_snaps_the_own_player() {
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        s.own.set_motion(Pos::new(50.0, 0.0, 0.0), 0.0, AnimState::Walk);
        let sent = s.own.next_intent(1.0).unwrap();
        assert_eq!(sent.seq, 1);
        s.apply(1.05, reject(1, Pos::new(10.0, 20.0, 0.0)));
        assert_eq!(s.own.pos(), Pos::new(10.0, 20.0, 0.0));
        assert_eq!(s.rejected(), 1);

        // The snap is announced once more (so the server learns our facing/animation too); a
        // non-finite correction from a broken server changes nothing.
        let resync = s.own.next_intent(1.2).expect("resync intent");
        assert_eq!((resync.seq, resync.pos), (2, Pos::new(10.0, 20.0, 0.0)));
        s.apply(1.25, reject(2, Pos::new(f32::NAN, 0.0, 0.0)));
        assert_eq!(s.own.pos(), Pos::new(10.0, 20.0, 0.0));
        assert_eq!(s.rejected(), 2);
        // Rejections of intents that were never sent are not about us either.
        s.apply(1.3, reject(77, Pos::new(1.0, 1.0, 0.0)));
        assert_eq!(s.own.pos(), Pos::new(10.0, 20.0, 0.0));
    }

    /// Reviewer scenario: seq1@24 Walk (t=1.00), seq2@48 Walk (1.05), seq3@48 Idle (1.10). The
    /// server rejects seq1 (corrected x = 0) and accepts 2 and 3, so it holds x = 48. The
    /// rejection arrives at 1.12, the update of our own entity at 1.15.
    #[test]
    fn a_stale_rejection_does_not_yank_the_player_back_over_newer_intents() {
        let mut s = state(at(0.0));
        for (t, x, anim) in [
            (1.00, 24.0, AnimState::Walk),
            (1.05, 48.0, AnimState::Walk),
            (1.10, 48.0, AnimState::Idle),
        ] {
            s.own.set_motion(at(x), 0.0, anim);
            assert!(s.own.next_intent(t).is_some(), "t {t}");
        }
        s.apply(1.12, reject(1, at(0.0)));
        s.apply(1.15, update(vec![snap(1, 48.0, 0.0)]));
        assert_eq!(s.own.pos().x, 48.0, "the server holds 48: we must not go back to 0");
        assert_eq!(s.rejected(), 1, "every rejection is counted, applied or not");
        // Client and server agree, so there is nothing to announce, now or ever.
        for t in [1.2, 3.0, 4.0, 60.0] {
            assert!(s.own.next_intent(t).is_none(), "t {t}");
            assert_eq!(s.own.pos().x, 48.0, "t {t}");
        }
        // When the player moves again the intent carries the real position, not a stale offset.
        s.own.set_motion(at(60.0), 0.0, AnimState::Walk);
        let i = s.own.next_intent(61.0).unwrap();
        assert_eq!((i.seq, i.pos.x), (4, 60.0));
    }

    /// Reviewer scenario: intent 1 Walk@10 is accepted; the stop, intent 2 Idle@20, is rejected
    /// (the server keeps x = 10 and still has Walk for a player that is standing). Returns the
    /// state right after the rejection was applied, with the clock at 1.15.
    fn after_a_rejected_stop() -> ClientState {
        let mut s = state(at(0.0));
        s.own.set_motion(at(10.0), 0.0, AnimState::Walk);
        assert_eq!(s.own.next_intent(1.0).unwrap().seq, 1);
        s.own.set_motion(at(20.0), 0.0, AnimState::Idle);
        let stop = s.own.next_intent(1.1).unwrap();
        assert_eq!((stop.seq, stop.anim), (2, AnimState::Idle));
        s.apply(1.15, reject(2, at(10.0)));
        assert_eq!(s.own.pos().x, 10.0);
        s
    }

    #[test]
    fn a_rejected_stop_is_announced_again_exactly_once() {
        let mut s = after_a_rejected_stop();
        // Rate limited like any intent (the last one went out at 1.1), but not forgotten.
        assert!(s.own.next_intent(1.12).is_none());
        let again = s.own.next_intent(1.2).expect("the rejected stop must be sent again");
        assert_eq!((again.seq, again.pos.x, again.anim), (3, 10.0, AnimState::Idle));
        for t in [1.3, 3.0, 4.0, 60.0] {
            assert!(s.own.next_intent(t).is_none(), "announced only once (t {t})");
        }
        // Normal movement afterwards is announced as usual.
        s.own.set_motion(at(12.0), 0.0, AnimState::Walk);
        assert_eq!(s.own.next_intent(61.0).unwrap().seq, 4);
    }

    /// If the server refuses the resync as well, we snap again but never send it a third time:
    /// announcing the same thing again would only be refused again, forever.
    #[test]
    fn a_refused_resync_is_not_repeated() {
        let mut s = after_a_rejected_stop();
        assert_eq!(s.own.next_intent(1.2).unwrap().seq, 3, "the resync intent");
        s.apply(1.25, reject(3, at(10.0)));
        assert_eq!(s.own.pos().x, 10.0);
        assert_eq!(s.rejected(), 2);
        for t in [1.5, 3.0, 60.0] {
            assert!(s.own.next_intent(t).is_none(), "t {t}");
        }
        s.own.set_motion(at(12.0), 0.0, AnimState::Walk);
        assert_eq!(s.own.next_intent(61.0).unwrap().seq, 4, "movement is announced as usual");
    }

    #[test]
    fn untrusted_snapshots_are_filtered_and_bounded() {
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), plaza());
        let mut bad = snap(2, f32::NAN, 0.0);
        s.apply(0.0, update(vec![bad.clone()]));
        bad.pos = Pos::new(0.0, f32::INFINITY, 0.0);
        s.apply(0.0, update(vec![bad]));
        let mut nan_facing = snap(2, 0.0, 0.0);
        nan_facing.facing = f32::NAN;
        s.apply(0.0, update(vec![nan_facing]));
        assert_eq!((s.peers_seen(), s.remote_count()), (0, 0));

        let many: Vec<_> = (100..100 + 3 * MAX_REMOTES as u64).map(|i| snap(i, 0.0, 0.0)).collect();
        s.apply(0.1, update(many));
        assert_eq!(s.remote_count(), MAX_REMOTES);
        assert_eq!(s.peers_seen(), 3 * MAX_REMOTES);

        // Hostile names are cleaned before they can reach a terminal or a window title.
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), plaza());
        let mut e = snap(2, 0.0, 0.0);
        e.avatar.display_name = "\u{1b}[31mEvil\u{7}\n".into();
        s.apply(0.0, update(vec![e]));
        let v = s.remote_views(0.0);
        assert_eq!(v.len(), 1);
        assert!(!v[0].name.chars().any(|c| c.is_control()), "{:?}", v[0].name);
    }

    /// The server relays any finite facing. Peers must not end up with NaN from interpolating it.
    #[test]
    fn huge_remote_facings_are_normalised_on_arrival() {
        let mut s = state(at(0.0));
        let mut a = snap(2, 0.0, 0.0);
        a.facing = 3.0e38;
        let mut b = snap(2, 10.0, 0.0);
        b.facing = -3.0e38;
        s.apply(1.00, update(vec![a]));
        s.apply(1.05, update(vec![b]));
        for t in [0.5, 1.0, 1.025, 1.05, 2.0] {
            let v = s.remote_views(t);
            let f = v[0].pose.facing;
            assert!(f.is_finite() && f.abs() <= PI, "t {t}: {f}");
        }
    }

    #[test]
    fn remote_views_interpolate_with_the_render_delay() {
        let mut s = state(Pos::default());
        s.apply(1.00, update(vec![snap(2, 0.0, 0.0)]));
        s.apply(1.10, update(vec![snap(2, 10.0, 0.0)]));
        let v = s.remote_views(1.05);
        assert_eq!(v.len(), 1);
        assert!((v[0].pose.pos.x - 5.0).abs() < 1e-4);
        assert_eq!(v[0].tint, [1, 2, 3]);
        assert_eq!(v[0].name, "p2");
    }

    /// Through the whole state: a peer that stood still and then takes a step glides in (the
    /// server sent nothing while it stood, so the two samples are 10 s apart).
    #[test]
    fn a_peer_that_starts_walking_after_standing_glides_instead_of_popping() {
        let mut s = state(Pos::default());
        let mut standing = snap(2, 0.0, 0.0);
        standing.anim = AnimState::Idle;
        s.apply(0.0, update(vec![standing]));
        s.apply(10.0, update(vec![snap(2, 24.0, 0.0)]));
        // 100 ms of render delay: at the moment the step arrives it is still drawn at x = 0.
        assert_eq!(s.remote_views(crate::interp::render_time(10.0))[0].pose.pos.x, 0.0);
        let done = s.remote_views(crate::interp::render_time(10.1))[0].pose.pos.x;
        assert!((done - 24.0).abs() < 1e-4, "{done}");
    }

    #[test]
    fn full_snapshot_replaces_remote_set() {
        let mut s = state(Pos::default());
        s.apply(0.0, update(vec![snap(2, 0.0, 0.0), snap(3, 0.0, 0.0)]));
        let r = s.apply(
            1.0,
            ServerMsg::AreaSnapshot {
                area: plaza(),
                tick: 5,
                entities: vec![snap(3, 1.0, 1.0)],
                npcs: vec![],
            },
        );
        assert!(r.is_none(), "a resync of the joined area is fine");
        let ids: Vec<_> = s.remote_views(1.0).iter().map(|v| v.id).collect();
        assert_eq!(ids, vec![EntityId(3)]);
    }

    /// The area is agreed (and checked against local content) once, in the handshake. Anything
    /// that would switch it later ends the session instead of drawing foreign entities on our mesh.
    #[test]
    fn the_area_cannot_change_after_joining() {
        let mut s = state(Pos::default());
        s.apply(0.0, update(vec![snap(2, 0.0, 0.0)]));

        let other = AreaId {
            game: GameId::Ff7,
            key: "md1stin\u{1b}[2J\nSMOKE name=forged".into(),
        };
        let why = s
            .apply(
                1.0,
                ServerMsg::AreaSnapshot {
                    area: other,
                    tick: 9,
                    entities: vec![snap(3, 1.0, 1.0)],
                    npcs: vec![],
                },
            )
            .expect("a snapshot of another area must end the session");
        assert!(why.contains("protocol error") && why.contains("Fixture/plaza"), "{why}");
        assert!(!why.chars().any(char::is_control), "server text must be cleaned: {why:?}");
        assert_eq!(s.remote_count(), 1, "nothing from the foreign snapshot was applied");

        let info = FixtureAdapter::new().list_areas().remove(0);
        let manifest = |fingerprint: Fingerprint| ServerMsg::AreaManifest {
            area: plaza(),
            display_name: "again".into(),
            fingerprint,
            provenance: info.provenance.summary(),
            fixture: true,
        };
        for fp in [info.fingerprint, Fingerprint([0; 32])] {
            let why = s.apply(2.0, manifest(fp)).expect("a second manifest must end the session");
            assert!(why.contains("protocol error") && why.contains("manifest"), "{why}");
        }
        assert_eq!(s.remote_count(), 1);
    }

    #[test]
    fn reject_ends_the_session() {
        let mut s = state(Pos::default());
        let r = s.apply(0.0, ServerMsg::reject(RejectCode::ServerFull, "server full"));
        assert_eq!(r.as_deref(), Some("server ended the session: server full"));
        // Whatever the code, the session ends and the player is told the server's own words.
        for code in RejectCode::ALL {
            let mut s = state(Pos::default());
            let r = s.apply(0.0, ServerMsg::reject(code, "gone\u{1b}[0m for now"));
            assert_eq!(r.as_deref(), Some("server ended the session: gone[0m for now"), "{code:?}");
        }
    }

    #[test]
    fn intents_are_rate_limited_sequenced_and_only_sent_on_change() {
        let area = area();
        let mut o = OwnPlayer::new(Pos::new(-390.0, 0.0, 0.0));
        assert!(o.next_intent(0.0).is_none(), "nothing changed yet");
        // Walk right for two simulated seconds at 60 fps (570 units, stays on the mesh).
        let mut intents = Vec::new();
        let mut t = 0.0;
        for frame in 0..120 {
            t = f64::from(frame) / 60.0;
            o.walk(area.as_ref(), (1.0, 0.0), 480.0, 1.0 / 60.0);
            if let Some(i) = o.next_intent(t) {
                intents.push((t, i));
            }
        }
        assert!(
            intents.len() >= 30 && intents.len() <= 41,
            "~20 Hz expected, got {}",
            intents.len()
        );
        for w in intents.windows(2) {
            assert_eq!(w[1].1.seq, w[0].1.seq + 1, "seq must increase by one");
            assert!(
                w[1].0 - w[0].0 >= MIN_INTENT_INTERVAL_S - 2.0 * RATE_EPS_S,
                "rate limit violated"
            );
        }
        assert_eq!(intents[0].1.seq, 1);
        // Stop: one final Idle intent, then silence.
        o.walk(area.as_ref(), (0.0, 0.0), 480.0, 1.0 / 60.0);
        let last = o.next_intent(t + 1.0).expect("the stop must be announced");
        assert_eq!(last.anim, AnimState::Idle);
        assert!(o.next_intent(t + 2.0).is_none());
        assert!(o.next_intent(t + 3.0).is_none());
    }

    /// The window runs by default and walks while Shift is held: `walk_as` announces the pace it
    /// is given, `walk` stays the plain walk, and standing still is still Idle.
    #[test]
    fn walk_as_announces_run_or_walk_and_a_blocked_step_is_idle() {
        let area = area();
        for moving in [AnimState::Run, AnimState::Walk] {
            let mut s = state(Pos::new(0.0, 0.0, 0.0));
            assert!(s.own.walk_as(area.as_ref(), (1.0, 0.0), 240.0, 1.0 / 20.0, moving));
            assert_eq!(s.own.anim(), moving);
            let intent = s.own.next_intent(1.0).expect("moving is announced");
            assert_eq!(intent.anim, moving, "the intent a server sees carries the pace");
            assert!(intent.pos.x > 0.0);
            // Stopping announces Idle, whatever pace was asked for.
            assert!(!s.own.walk_as(area.as_ref(), (0.0, 0.0), 240.0, 1.0 / 20.0, moving));
            assert_eq!(s.own.anim(), AnimState::Idle);
            assert_eq!(s.own.next_intent(2.0).expect("the stop is announced").anim, AnimState::Idle);
        }
        // The plain walk is a Walk.
        let mut o = OwnPlayer::new(Pos::new(0.0, 0.0, 0.0));
        assert!(o.walk(area.as_ref(), (1.0, 0.0), 240.0, 1.0 / 20.0));
        assert_eq!(o.anim(), AnimState::Walk);
        // A blocked step (the corner) is Idle even when running was asked for.
        let mut o = OwnPlayer::new(Pos::new(-400.0, 300.0, 0.0));
        assert!(!o.walk_as(area.as_ref(), (-1.0, 1.0), 240.0, 1.0 / 60.0, AnimState::Run));
        assert_eq!(o.anim(), AnimState::Idle);
    }

    fn avatar_snap(id: u64, appearance: Option<&str>) -> EntitySnapshot {
        let mut e = snap(id, 0.0, 0.0);
        e.avatar.appearance_ref = appearance.map(str::to_string);
        e
    }

    /// The look a peer names reaches the renderer (untrusted text, bounded), and our own look is
    /// taken from the server's snapshot of our own entity.
    #[test]
    fn appearances_from_the_network_are_kept_bounded_and_the_own_one_comes_from_our_snapshot() {
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        assert_eq!(s.own_appearance(), None, "nothing is known before the first snapshot");
        let long = "x".repeat(MAX_APPEARANCE_REF + 1);
        let exact = "y".repeat(MAX_APPEARANCE_REF);
        s.apply(
            0.0,
            ServerMsg::AreaSnapshot {
                area: plaza(),
                tick: 1,
                entities: vec![
                    avatar_snap(1, Some("ff7/barret")), // our own entity
                    avatar_snap(2, Some("ff7/tifa")),
                    avatar_snap(3, None),
                    avatar_snap(4, Some(&long)),
                    avatar_snap(5, Some(&exact)),
                ],
                npcs: Vec::new(),
            },
        );
        assert_eq!(s.own_appearance(), Some("ff7/barret"));
        let looks = |s: &ClientState| -> Vec<(u64, Option<String>)> { s.remote_views(1.0).iter().map(|v| (v.id.0, v.appearance.clone())).collect() };
        assert_eq!(
            looks(&s),
            [(2, Some("ff7/tifa".to_string())), (3, None), (4, None), (5, Some(exact.clone()))],
            "an over-long ref is dropped (the default look is drawn), one at the limit is kept"
        );
        // A later update replaces it (and an update without one clears it).
        s.apply(0.1, update(vec![avatar_snap(2, Some("ff7/aerith")), avatar_snap(3, Some("ff7/cid")), avatar_snap(1, None)]));
        let now = looks(&s);
        assert_eq!(now[0], (2, Some("ff7/aerith".to_string())));
        assert_eq!(now[1], (3, Some("ff7/cid".to_string())));
        assert_eq!(s.own_appearance(), None);
        // Players that leave take their look with them.
        s.apply(0.2, ServerMsg::PlayerLeft { entity: EntityId(2) });
        assert!(looks(&s).iter().all(|(id, _)| *id != 2));
    }

    /// The identity colour of a player is what the server's snapshots say about its entity: ours from our own, a
    /// peer's from its own, nothing for one that has not been mentioned or has left; it follows an update.
    #[test]
    fn the_colour_of_a_player_is_what_the_snapshots_say_about_its_entity() {
        let tinted = |id: u64, tint: [u8; 3]| {
            let mut e = snap(id, 0.0, 0.0);
            e.avatar.tint = tint;
            e
        };
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        assert_eq!((s.tint_of(EntityId(1)), s.tint_of(EntityId(2))), (None, None), "nothing is known before the first snapshot");
        s.apply(0.0, ServerMsg::AreaSnapshot { area: plaza(), tick: 1, entities: vec![tinted(1, [70, 130, 245]), tinted(2, [240, 120, 40])], npcs: Vec::new() });
        assert_eq!(s.tint_of(EntityId(1)), Some([70, 130, 245]), "ours, from our own entity");
        assert_eq!(s.tint_of(EntityId(2)), Some([240, 120, 40]), "a peer's, from its");
        assert_eq!(s.tint_of(EntityId(3)), None, "one the server has not mentioned");
        s.apply(0.1, update(vec![tinted(2, [10, 200, 30])]));
        assert_eq!(s.tint_of(EntityId(2)), Some([10, 200, 30]));
        s.apply(0.2, ServerMsg::PlayerLeft { entity: EntityId(2) });
        assert_eq!(s.tint_of(EntityId(2)), None);
        assert_eq!(s.tint_of(EntityId(1)), Some([70, 130, 245]));
    }

    #[test]
    fn walking_stays_on_the_mesh_and_slides_along_walls() {
        let area = area();
        // Start next to the top wall (y max = 300) and push diagonally up-right.
        let mut o = OwnPlayer::new(Pos::new(0.0, 295.0, 0.0));
        let mut moved_x = 0.0;
        for _ in 0..30 {
            let before = o.pos();
            o.walk(area.as_ref(), (1.0, 1.0), 480.0, 1.0 / 60.0);
            assert!(matches!(area.validate_move(before, o.pos()), MoveResult::Accepted(_)));
            moved_x += o.pos().x - before.x;
            assert!(o.pos().y <= 300.0 + 1e-3);
        }
        assert!(moved_x > 50.0, "should slide along the wall, moved {moved_x}");
        // Fully blocked (corner): no movement, Idle.
        let mut o = OwnPlayer::new(Pos::new(-400.0, 300.0, 0.0));
        assert!(!o.walk(area.as_ref(), (-1.0, 1.0), 480.0, 1.0 / 60.0));
        assert_eq!(o.anim(), AnimState::Idle);
        // Garbage input never panics or moves.
        assert!(!o.walk(area.as_ref(), (f32::NAN, 0.0), 480.0, 1.0 / 60.0));
        assert!(!o.walk(area.as_ref(), (1.0, 0.0), 480.0, f32::NAN));
        assert!(!o.walk(area.as_ref(), (1.0, 0.0), 480.0, -1.0));
    }

    /// `AreaRuntime` for the tests below: the fixture, except that every answer is at height 7
    /// (as an adapter that snaps to the walkable surface would give), refused steps included.
    struct SnapsHeight(Box<dyn AreaRuntime>);

    impl AreaRuntime for SnapsHeight {
        fn info(&self) -> &AreaInfo {
            self.0.info()
        }
        fn spawn_points(&self) -> Vec<Pos> {
            self.0.spawn_points()
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            match self.0.validate_move(from, to) {
                MoveResult::Accepted(p) => MoveResult::Accepted(Pos::new(p.x, p.y, 7.0)),
                MoveResult::Corrected(p) => MoveResult::Corrected(Pos::new(p.x, p.y, 7.0)),
            }
        }
        fn bounds(&self) -> Bounds {
            self.0.bounds()
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            self.0.walk_triangles()
        }
        fn max_speed(&self) -> f32 {
            self.0.max_speed()
        }
    }

    /// `Corrected(p)` means "place the entity at p". The usual case (a wall, p is where we
    /// already stand) must keep sliding; a different p must be obeyed.
    #[test]
    fn walk_obeys_an_adapter_that_places_the_player_elsewhere() {
        let area = area();
        // Standing off the walkable surface: the adapter says to stand at the first spawn point.
        let mut o = OwnPlayer::new(Pos::new(5000.0, 5000.0, 0.0));
        assert!(o.walk(area.as_ref(), (1.0, 0.0), 480.0, 1.0 / 60.0), "the position changed");
        assert_eq!(o.pos(), area.spawn_points()[0]);
        assert_eq!(o.anim(), AnimState::Idle, "being placed is not walking");
        let i = o.next_intent(1.0).expect("the placement is reported like any move");
        assert_eq!(i.pos, area.spawn_points()[0]);
        // Back on the mesh, ordinary walking resumes.
        assert!(o.walk(area.as_ref(), (1.0, 0.0), 480.0, 1.0 / 60.0));
        assert_eq!(o.anim(), AnimState::Walk);

        // A correction that only changes the height is adopted but must not end the slide: along
        // the top wall, the refused diagonal step corrects z, then the x step is still taken.
        let area = SnapsHeight(area);
        let mut o = OwnPlayer::new(Pos::new(0.0, 295.0, 0.0));
        let mut moved_x = 0.0;
        for _ in 0..30 {
            let before = o.pos();
            assert!(o.walk(&area, (1.0, 1.0), 480.0, 1.0 / 60.0));
            moved_x += o.pos().x - before.x;
            assert_eq!(o.pos().z, 7.0);
            assert!(o.pos().y <= 300.0 + 1e-3);
        }
        assert!(moved_x > 50.0, "still slides after a height correction, moved {moved_x}");
    }

    #[test]
    fn idle_reconciliation_adopts_diverged_server_position_only_when_quiet() {
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        // Moving: a lagging server position must not yank us back.
        s.own.set_motion(Pos::new(100.0, 0.0, 0.0), 0.0, AnimState::Walk);
        let i = s.own.next_intent(10.0).unwrap();
        assert_eq!(i.pos.x, 100.0);
        s.apply(10.1, update(vec![snap(1, 76.0, 0.0)]));
        assert_eq!(s.own.pos().x, 100.0);
        // Quiet for more than 0.5 s and still diverged: adopt the server's position.
        s.apply(10.8, update(vec![snap(1, 76.0, 0.0)]));
        assert_eq!(s.own.pos().x, 76.0);
        // ...without echoing the adoption back as a new intent.
        assert!(s.own.next_intent(11.0).is_none());
        // Our own entity never counts as a peer.
        assert_eq!(s.peers_seen(), 0);
    }

    /// The server only sends changes, so an update of our own entity that arrives while we are
    /// busy is the only one we will get: it must still be adopted once we are quiet.
    #[test]
    fn a_server_position_seen_while_busy_is_adopted_once_quiet_without_another_update() {
        let mut s = state(at(0.0));
        s.own.set_motion(at(100.0), 0.0, AnimState::Walk);
        s.own.next_intent(10.0).unwrap();
        s.apply(10.1, update(vec![snap(1, 76.0, 0.0)]));
        assert_eq!(s.own.pos().x, 100.0, "busy: kept, not applied");
        assert!(s.own.next_intent(10.3).is_none());
        assert_eq!(s.own.pos().x, 100.0, "still busy");
        // Quiet now (checked on every call, also while the rate limit would hold an intent back).
        assert!(s.own.next_intent(10.7).is_none(), "adopting is not echoed back");
        assert_eq!(s.own.pos().x, 76.0);
        assert!(s.own.next_intent(11.0).is_none());

        // A new intent supersedes what the server said before it: it is never adopted later.
        s.own.set_motion(at(120.0), 0.0, AnimState::Walk);
        s.own.next_intent(20.0).unwrap();
        s.apply(20.1, update(vec![snap(1, 100.0, 0.0)]));
        s.own.set_motion(at(130.0), 0.0, AnimState::Walk);
        s.own.next_intent(20.2).unwrap();
        assert!(s.own.next_intent(21.0).is_none());
        assert_eq!(s.own.pos().x, 130.0, "the superseded server position must not come back");
    }

    fn party_name(account: u64, name: &str, entity: u64) -> fmv_protocol::PartyName {
        fmv_protocol::PartyName { account: AccountId(account), name: name.into(), entity: Some(EntityId(entity)) }
    }

    /// The party messages reach the mirror through `apply`, none of them ends the session, and
    /// the mirror knows which account is ours.
    #[test]
    fn party_messages_feed_the_mirror_and_never_end_the_session() {
        use fmv_protocol::{InviteId, PartyEvent, PartyFailure, PartyId, Placement};
        let mut s = state(Pos::default()); // we are account 1, entity 1
        let names = vec![party_name(1, "Me", 1), party_name(2, "Tifa", 2)];
        let ev = |seq: u64, event: PartyEvent| ServerMsg::PartyEvent { seq, event, names: names.clone() };
        let sent = PartyEvent::InviteSent { invite: InviteId(1), from: AccountId(2), to: AccountId(1), expires_at: 9 };
        assert!(s.apply(0.0, ev(1, sent)).is_none());
        assert_eq!(s.party.newest_invite_to_me().map(|i| (i.id, i.from_name)), Some((InviteId(1), "Tifa".to_string())));
        let formed = PartyEvent::PartyFormed {
            party: PartyId(1),
            invite: InviteId(1),
            leader: AccountId(2),
            members: vec![AccountId(2), AccountId(1)],
        };
        assert!(s.apply(0.1, ev(2, formed)).is_none());
        assert_eq!((s.party.party_size(), s.party.peak_party_size()), (2, 2));
        assert!(s.party.is_party_entity(EntityId(2)));

        assert!(s.apply(0.2, ServerMsg::PartyRejected { error: PartyFailure::UnknownPlayer }).is_none());
        assert_eq!(s.party.rejection_count(), 1);

        let placement = Placement {
            account: AccountId(1),
            key: fmv_protocol::InstanceKey::new(plaza(), fmv_core::PhaseKey(vec![])),
            party: Some(PartyId(1)),
            phase_owner: AccountId(2),
            story_advance_allowed: true,
        };
        assert!(s.apply(0.3, ServerMsg::InstancePlacement { placement }).is_none());
        assert_eq!(s.party.last_placement().map(|p| p.party), Some(Some(PartyId(1))));

        // A `PartyState` replaces the party; the mirror's seq follows it.
        let none = ServerMsg::PartyState { seq: 9, party: None, invites: vec![], names: vec![] };
        assert!(s.apply(0.4, none).is_none());
        assert_eq!(s.party.party_size(), 0);
        // None of this counts as game traffic.
        assert_eq!((s.updates(), s.rejected(), s.peers_seen()), (0, 0, 0));
    }

    /// "Invite the nearest player" means the nearest VISIBLE player who is not in our party yet.
    #[test]
    fn the_nearest_invitable_player_is_the_closest_visible_one_outside_the_party() {
        use fmv_protocol::{InviteId, PartyEvent, PartyId};
        let mut s = state(Pos::new(0.0, 0.0, 0.0));
        assert_eq!(s.nearest_invitable(1.0), None, "nobody is here");
        s.apply(0.0, update(vec![snap(1, 0.0, 0.0), snap(2, 10.0, 0.0), snap(3, 50.0, 0.0), snap(4, -30.0, 0.0)]));
        assert_eq!(s.nearest_invitable(1.0), Some(EntityId(2)), "our own entity is never a candidate");
        // The distance is measured from where WE are now.
        s.own.set_motion(Pos::new(60.0, 0.0, 0.0), 0.0, AnimState::Idle);
        assert_eq!(s.nearest_invitable(1.0), Some(EntityId(3)));
        s.own.set_motion(Pos::new(0.0, 0.0, 0.0), 0.0, AnimState::Idle);
        // Entity 2 (account 2) joins our party: the next closest is chosen instead.
        let names = vec![party_name(1, "Me", 1), party_name(2, "Tifa", 2)];
        let formed = PartyEvent::PartyFormed {
            party: PartyId(1),
            invite: InviteId(1),
            leader: AccountId(1),
            members: vec![AccountId(1), AccountId(2)],
        };
        s.apply(0.1, ServerMsg::PartyEvent { seq: 1, event: formed, names });
        assert_eq!(s.nearest_invitable(1.0), Some(EntityId(4)));
        s.apply(0.2, ServerMsg::PlayerLeft { entity: EntityId(4) });
        s.apply(0.2, ServerMsg::PlayerLeft { entity: EntityId(3) });
        assert_eq!(s.nearest_invitable(1.0), None, "everybody left is already in the party");
    }

    // ---- areas: NPCs and moving on to another area ----

    fn npc_snapshot(id: u32, name: &str) -> NpcSnapshot {
        NpcSnapshot {
            id,
            name: name.into(),
            model: "aaaa".into(),
            idle_animation: Some("acfe".into()),
            pos: Pos::new(10.0 * id as f32, 5.0, 0.0),
            facing: 0.5,
        }
    }

    fn snapshot_of(area: AreaId, entities: Vec<EntitySnapshot>, npcs: Vec<NpcSnapshot>) -> ServerMsg {
        ServerMsg::AreaSnapshot { area, tick: 1, entities, npcs }
    }

    #[test]
    fn a_snapshot_brings_the_areas_npcs_and_the_next_one_replaces_them() {
        let mut s = state(Pos::default());
        assert!(s.npc_views().is_empty());
        let r = s.apply(0.0, snapshot_of(plaza(), vec![snap(1, 0.0, 0.0)], vec![npc_snapshot(0, "gu0"), npc_snapshot(1, "gu1")]));
        assert!(r.is_none());
        let names: Vec<&str> = s.npc_views().iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["gu0", "gu1"]);
        assert_eq!((s.npc_views()[1].pos, s.npc_views()[1].model.as_str(), s.npc_views()[1].idle_animation.as_deref()), (Pos::new(10.0, 5.0, 0.0), "aaaa", Some("acfe")));
        // NPCs never count as players: they are not remotes and not peers.
        assert_eq!((s.remote_count(), s.peers_seen()), (0, 0));
        // A new snapshot is the whole truth about the area's NPCs.
        s.apply(1.0, snapshot_of(plaza(), vec![], vec![npc_snapshot(0, "only")]));
        assert_eq!(s.npc_views().len(), 1);
        s.apply(2.0, snapshot_of(plaza(), vec![], vec![]));
        assert!(s.npc_views().is_empty());
    }

    /// A renderer that keeps something per set of NPCs rebuilds when the epoch moves: on every
    /// snapshot and every change of area, and not on the traffic in between.
    #[test]
    fn the_npc_epoch_moves_when_the_list_is_replaced_and_only_then() {
        let mut s = state(Pos::default());
        let start = s.npc_epoch();
        s.apply(0.0, snapshot_of(plaza(), vec![snap(1, 0.0, 0.0)], vec![npc_snapshot(0, "gu0")]));
        let first = s.npc_epoch();
        assert_ne!(first, start);
        // Updates, joins and leaves do not touch the NPCs.
        s.apply(0.1, update(vec![snap(1, 1.0, 1.0)]));
        s.apply(0.2, ServerMsg::PlayerLeft { entity: EntityId(9) });
        assert_eq!(s.npc_epoch(), first);
        // Another snapshot (even of the same list) is a new list; so is leaving the area.
        s.apply(1.0, snapshot_of(plaza(), vec![], vec![npc_snapshot(0, "gu0")]));
        let second = s.npc_epoch();
        assert_ne!(second, first);
        s.change_area(plaza(), Pos::default(), 0.0, true);
        assert_ne!(s.npc_epoch(), second);
        assert!(s.npc_views().is_empty());
    }

    #[test]
    fn npc_text_is_cleaned_and_bounded_and_unusable_ones_are_dropped() {
        let mut s = state(Pos::default());
        let mut hostile = npc_snapshot(0, "\u{1b}[2JSMOKE\nforged\u{202e}");
        hostile.model = "m".repeat(500);
        hostile.idle_animation = Some("\u{1b}".into());
        let mut nan = npc_snapshot(1, "nan");
        nan.pos = Pos::new(f32::NAN, 0.0, 0.0);
        let mut inf_facing = npc_snapshot(2, "inf");
        inf_facing.facing = f32::INFINITY;
        let mut huge_facing = npc_snapshot(3, "huge");
        huge_facing.facing = 3.0e38;
        s.apply(0.0, snapshot_of(plaza(), vec![], vec![hostile, nan, inf_facing, huge_facing]));
        let views = s.npc_views();
        assert_eq!(views.len(), 2, "the two with a position or heading that is not finite are dropped");
        assert!(!views[0].name.chars().any(|c| c.is_control() || c == '\u{202e}'), "{:?}", views[0].name);
        assert_eq!(views[0].model.chars().count(), NPC_REF_CHARS);
        assert_eq!(views[0].idle_animation, None, "an animation that is nothing visible is none");
        assert!(views[1].facing.is_finite() && views[1].facing.abs() <= std::f32::consts::PI, "{}", views[1].facing);
        // The list is bounded however much the server sends.
        let many: Vec<NpcSnapshot> = (0..3 * MAX_NPCS_PER_AREA as u32).map(|i| npc_snapshot(i, "n")).collect();
        s.apply(1.0, snapshot_of(plaza(), vec![], many));
        assert_eq!(s.npc_views().len(), MAX_NPCS_PER_AREA);
    }

    fn yard() -> AreaId {
        AreaId { game: GameId::Fixture, key: "yard".into() }
    }

    #[test]
    fn changing_area_drops_the_old_areas_things_and_keeps_ours() {
        let mut s = state(Pos::new(-100.0, 0.0, 0.0));
        s.apply(0.0, snapshot_of(plaza(), vec![snap(1, -100.0, 0.0), snap(2, 5.0, 5.0)], vec![npc_snapshot(0, "gu0")]));
        s.apply(0.1, update(vec![snap(3, 9.0, 9.0)]));
        s.apply(0.2, ServerMsg::MoveRejected { seq: 9, corrected: Pos::new(1.0, 1.0, 0.0) });
        assert_eq!((s.peers_seen(), s.remote_count(), s.npc_views().len(), s.area_changes()), (2, 2, 1, 0));
        let (updates, rejected) = (s.updates(), s.rejected());

        s.change_area(yard(), Pos::new(-200.0, 0.0, 0.0), 1.25, true);
        assert_eq!(s.area(), &yard());
        assert_eq!(s.area_changes(), 1);
        assert_eq!((s.peers_seen(), s.remote_count(), s.npc_views().len()), (0, 0, 0), "peers are counted per area");
        assert_eq!((s.updates(), s.rejected()), (updates, rejected), "the other counters are the session's");
        assert_eq!((s.own.pos(), s.own.facing()), (Pos::new(-200.0, 0.0, 0.0), 1.25));
        assert_eq!(s.self_entity(), EntityId(1), "the entity is the same in every area");
        // Standing where the server put us announces nothing; the first step does.
        assert!(s.own.next_intent(10.0).is_none());
        s.own.set_motion(Pos::new(-190.0, 0.0, 0.0), 1.25, AnimState::Walk);
        let first = s.own.next_intent(10.1).expect("the first step is announced");
        assert_eq!(first.seq, 1, "a new area starts a new sequence");

        // The snapshot of the NEW area is accepted, the old one is not any more.
        let r = s.apply(11.0, snapshot_of(yard(), vec![snap(1, -190.0, 0.0), snap(7, 5.0, 5.0)], vec![npc_snapshot(0, "guard")]));
        assert!(r.is_none());
        assert_eq!((s.peers_seen(), s.npc_views().len()), (1, 1));
        let why = s.apply(12.0, snapshot_of(plaza(), vec![], vec![])).expect("the old area's snapshot is a protocol error now");
        assert!(why.contains("protocol error"), "{why}");

        // The way back after a refused transfer is not a change.
        s.change_area(plaza(), Pos::new(250.0, 0.0, 0.0), 0.0, false);
        assert_eq!(s.area_changes(), 1);
        s.change_area(yard(), Pos::new(-200.0, 0.0, 0.0), 0.0, true);
        assert_eq!(s.area_changes(), 2);
        // A non-finite heading from a broken server does not poison the player.
        s.change_area(plaza(), Pos::default(), f32::NAN, true);
        assert_eq!(s.own.facing(), 0.0);
    }

    /// The party mirror and the battle mirror are the player's, not the area's.
    #[test]
    fn the_party_survives_a_change_of_area() {
        use fmv_protocol::{InviteId, PartyEvent, PartyId};
        let mut s = state(Pos::default());
        let formed = PartyEvent::PartyFormed {
            party: PartyId(1),
            invite: InviteId(1),
            leader: AccountId(1),
            members: vec![AccountId(1), AccountId(2)],
        };
        s.apply(0.0, ServerMsg::PartyEvent { seq: 1, event: formed, names: vec![party_name(1, "Me", 1), party_name(2, "Tifa", 2)] });
        assert_eq!(s.party.party_size(), 2);
        s.change_area(yard(), Pos::default(), 0.0, true);
        assert_eq!(s.party.party_size(), 2);
        assert!(!s.battle.in_battle());
    }

    /// An `AreaChange` is the session's to answer, never the state's: applied to the state alone
    /// it changes nothing (and above all never moves us without the local content check).
    #[test]
    fn the_state_alone_ignores_an_area_change_offer() {
        let mut s = state(Pos::default());
        let info = FixtureAdapter::new().list_areas().remove(0);
        let offer = ServerMsg::AreaChange {
            area: yard(),
            display_name: "yard".into(),
            fingerprint: info.fingerprint,
            provenance: info.provenance.summary(),
            fixture: true,
            arrival: Pos::new(-200.0, 0.0, 0.0),
            arrival_facing: 0.0,
        };
        assert!(s.apply(0.0, offer).is_none());
        assert_eq!((s.area(), s.area_changes()), (&plaza(), 0));
    }

    /// What our account carries (protocol v6) is the battle mirror's: it keeps the last message, which
    /// is the whole inventory every time, notes it, and an inventory is never a reason to end the session.
    #[test]
    fn an_inventory_message_reaches_the_battle_mirror_and_never_ends_the_session() {
        use fmv_protocol::InventoryEntry;
        let mut s = state(Pos::default());
        assert_eq!((s.battle.inventory_total(), s.battle.inventory_updates()), (0, 0));
        let said = vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)];
        assert_eq!(s.apply(0.1, ServerMsg::Inventory { items: said.clone() }), None);
        assert_eq!(s.battle.inventory(), said.as_slice());
        assert_eq!((s.battle.inventory_total(), s.battle.inventory_updates()), (5, 1));
        assert_eq!(s.battle.drain_notes(), ["inventory: stacks=3 total=5 items=0:3,3:1,7:1"]);
        assert_eq!(s.apply(0.2, ServerMsg::Inventory { items: vec![] }), None, "the next message replaces it, an empty one too");
        assert_eq!((s.battle.inventory_total(), s.battle.inventory_updates()), (0, 2));
    }
}
