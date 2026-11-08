//! The client's mirror of its party situation, fed by what the server tells it.
//!
//! The server owns the truth (`fmv-party`'s `PartyManager`) and announces every change as a
//! [`PartyEvent`] to exactly the accounts it concerns. Applying every event this client receives,
//! in order, reproduces the server's view of this client's party and pending invites (see the
//! "Contract" in `fmv_party::event`):
//!
//! * `PartyFormed` / `MemberJoined` carry the whole roster (and the leader), so they set the
//!   party if this account is in `members`; a new member builds the party from its first event.
//! * `MemberLeft` for this account ends the party; for anyone else it replaces the roster.
//! * `LeaderChanged` replaces the leader (and roster). It arrives BEFORE the `MemberLeft` of a
//!   leader who leaves, so every intermediate state is a valid party.
//! * `Disbanded` ends the party.
//! * `InviteSent` adds a pending invite (to or from this account); each invite ends with exactly
//!   one terminal event: `InviteDeclined`, `InviteExpired`, `InviteCancelled`, or the `invite` of
//!   the `PartyFormed` / `MemberJoined` that consumed it.
//!
//! `seq` counts the events delivered to THIS client (1, 2, ...). An event whose `seq` is not the
//! next one means events were lost (or repeated): the mirror raises a flag ([`PartyMirror::sync_due`])
//! and the loops ask the server for a [`PartyState`](fmv_protocol::ServerMsg::PartyState), which
//! REPLACES the party and the invites and carries the `seq` of the last event it includes.
//!
//! Everything from the server is untrusted: names are cleaned, every table is bounded, and
//! nothing here can panic on what the server sends.

use crate::net::clean;
use fmv_core::{AccountId, AvatarAppearance, EntityId, Pos};
use fmv_protocol::{Invite, InviteId, Party, PartyEvent, PartyFailure, PartyName, Placement};
use std::collections::{BTreeMap, VecDeque};

/// Accounts remembered in the name table (a hostile server cannot make it grow without limit).
const MAX_NAMES: usize = 512;
/// Pending invites tracked at once; beyond this the oldest (lowest id) are forgotten.
const MAX_INVITES: usize = 256;
/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 64;
/// While the mirror is known to be out of step, the full state is asked for at most this often
/// (seconds).
pub const SYNC_RETRY_S: f64 = 2.0;

/// A pending invite to or from this account. (Its expiry is in the server's tick units, which
/// this client cannot compare with any clock: the server announces it with `InviteExpired`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingInvite {
    pub id: InviteId,
    pub from: AccountId,
    pub to: AccountId,
}

impl PendingInvite {
    fn from_wire(i: &Invite) -> Self {
        Self { id: i.id, from: i.from, to: i.to }
    }
}

/// The newest invite addressed to this account, as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingInvite {
    pub id: InviteId,
    /// The inviter's display name (cleaned), `?` if the server never said.
    pub from_name: String,
}

/// One member of this account's party, as the in-game party list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The cleaned display name (`account N` if the server never named the account).
    pub name: String,
    pub leader: bool,
    /// This account itself.
    pub you: bool,
}

/// What the table of names says about an account.
#[derive(Debug, Clone)]
struct KnownPlayer {
    name: String,
    /// The player's entity in this area, if they are in it.
    entity: Option<EntityId>,
}

/// The party situation of this client. See the module docs.
#[derive(Debug)]
pub struct PartyMirror {
    me: AccountId,
    party: Option<Party>,
    invites: BTreeMap<InviteId, PendingInvite>,
    names: BTreeMap<AccountId, KnownPlayer>,
    /// `seq` of the last event applied, or of the last `PartyState` (0: none yet).
    last_seq: u64,
    /// Events were lost or repeated: the full state must be asked for.
    needs_sync: bool,
    last_sync_request: Option<f64>,
    /// The largest party this account was in at any moment.
    peak: usize,
    rejections: u64,
    last_rejection: Option<String>,
    placement: Option<Placement>,
    /// An `InviteSent` with this account as the inviter was seen.
    outgoing_invite_seen: bool,
    notes: VecDeque<String>,
}

impl PartyMirror {
    /// An empty mirror for `me`: no party, no invites (a connection starts with neither: a
    /// player who disconnects leaves their party).
    pub fn new(me: AccountId) -> Self {
        Self {
            me,
            party: None,
            invites: BTreeMap::new(),
            names: BTreeMap::new(),
            last_seq: 0,
            needs_sync: false,
            last_sync_request: None,
            peak: 0,
            rejections: 0,
            last_rejection: None,
            placement: None,
            outgoing_invite_seen: false,
            notes: VecDeque::new(),
        }
    }

    // ----- feeding ------------------------------------------------------------------------

    /// Apply one `PartyEvent` message. `names` says who the accounts in `event` are.
    pub fn on_event(&mut self, seq: u64, event: PartyEvent, names: Vec<PartyName>) {
        self.learn_names(names);
        let expected = self.last_seq.saturating_add(1);
        if seq < expected {
            // Already applied (or already contained in a `PartyState`): do not apply it twice,
            // but the numbering is not what it should be, so ask for the truth.
            self.needs_sync = true;
            self.note(format!("party event {seq} arrived again (expected {expected}); asking for the full state"));
            return;
        }
        if seq > expected {
            self.needs_sync = true;
            self.note(format!("party events {expected}..{} were lost; asking for the full state", seq - 1));
        }
        self.last_seq = seq;
        self.apply(event);
    }

    /// Apply a `PartyState`: the party and the invite sets are REPLACED (it answers `PartySync`).
    pub fn on_state(&mut self, seq: u64, party: Option<Party>, invites: Vec<Invite>, names: Vec<PartyName>) {
        self.learn_names(names);
        // A party that does not list this account cannot be ours: the server only describes the
        // requester's own party.
        self.party = party.filter(|p| p.contains(self.me));
        self.invites = invites
            .iter()
            .filter(|i| i.from == self.me || i.to == self.me)
            .map(|i| (i.id, PendingInvite::from_wire(i)))
            .collect();
        while self.invites.len() > MAX_INVITES {
            self.invites.pop_first();
        }
        self.outgoing_invite_seen |= self.invites.values().any(|i| i.from == self.me);
        self.last_seq = seq;
        self.needs_sync = false;
        self.last_sync_request = None;
        self.note_peak();
        let size = self.party_size();
        let n = self.invites.len();
        self.note(format!("state synced: party of {size}, {n} pending invite(s)"));
    }

    /// A party request of ours was refused.
    pub fn on_rejected(&mut self, failure: &PartyFailure) {
        self.rejections += 1;
        // `PartyFailure` is typed, but a few variants carry free text: clean the whole line.
        let text = clean(&failure.to_string());
        self.note(format!("request refused: {text}"));
        self.last_rejection = Some(text);
    }

    /// The server placed this player in an instance (on join, and when party changes alter it).
    pub fn on_placement(&mut self, placement: Placement) {
        self.note(format!("placement: {}", describe_placement(&placement)));
        self.placement = Some(placement);
    }

    // ----- queries ------------------------------------------------------------------------

    /// The party this account is in, if any.
    #[cfg(test)]
    pub fn party(&self) -> Option<&Party> {
        self.party.as_ref()
    }

    /// Size of this account's party (0 if it is in none).
    pub fn party_size(&self) -> usize {
        self.party.as_ref().map_or(0, Party::size)
    }

    /// Is this account the leader of its party?
    pub fn is_leader(&self) -> bool {
        self.party.as_ref().is_some_and(|p| p.is_leader(self.me))
    }

    /// Is `account` in this account's party? (This account itself counts, when it is in one.)
    pub fn is_party_account(&self, account: AccountId) -> bool {
        self.party.as_ref().is_some_and(|p| p.contains(account))
    }

    /// Is the player with this entity in this account's party? (Known through the names the
    /// server attached to its party messages.)
    pub fn is_party_entity(&self, entity: EntityId) -> bool {
        self.names
            .iter()
            .any(|(account, known)| known.entity == Some(entity) && self.is_party_account(*account))
    }

    /// The members of this account's party in roster order, named from what the server said
    /// (empty when this account is in none): what the in-game party list shows.
    pub fn party_members(&self) -> Vec<RosterEntry> {
        let Some(party) = &self.party else { return Vec::new() };
        party
            .members
            .iter()
            .take(16)
            .map(|m| RosterEntry { name: self.name_of(*m), leader: party.is_leader(*m), you: *m == self.me })
            .collect()
    }

    /// The newest (highest id) pending invite addressed to this account.
    pub fn newest_invite_to_me(&self) -> Option<IncomingInvite> {
        self.invites.values().rev().find(|i| i.to == self.me).map(|i| IncomingInvite {
            id: i.id,
            from_name: self.name_of(i.from),
        })
    }

    /// Pending invites addressed to this account, oldest first.
    pub fn invites_to_me(&self) -> impl Iterator<Item = &PendingInvite> + '_ {
        self.invites.values().filter(|i| i.to == self.me)
    }

    /// Has this account sent an invite (seen as an `InviteSent` or in a `PartyState`)?
    pub fn outgoing_invite_seen(&self) -> bool {
        self.outgoing_invite_seen
    }

    /// The largest party this account was in at any moment (0 if it never was in one).
    pub fn peak_party_size(&self) -> usize {
        self.peak
    }

    /// How many party requests of ours the server refused.
    pub fn rejection_count(&self) -> u64 {
        self.rejections
    }

    /// The text of the last refusal, if any.
    pub fn last_rejection(&self) -> Option<&str> {
        self.last_rejection.as_deref()
    }

    /// One line about the whole party situation (the headless bot logs it when it finishes).
    pub fn summary(&self) -> String {
        let now = match self.party_size() {
            0 => "no party".to_string(),
            n => format!("a party of {n}{}", if self.is_leader() { ", leading it" } else { "" }),
        };
        let refusals = match self.last_rejection() {
            Some(last) => format!("{} refusal(s), the last: {last}", self.rejections),
            None => "no refusals".to_string(),
        };
        let placement = self.last_placement().map_or_else(|| "none".to_string(), describe_placement);
        format!("peak party size {}; now {now}; {refusals}; placement: {placement}", self.peak)
    }

    /// The last instance placement the server announced.
    pub fn last_placement(&self) -> Option<&Placement> {
        self.placement.as_ref()
    }

    /// Should the loop ask the server for the full state now? True at most once per
    /// [`SYNC_RETRY_S`] while the mirror is known to be out of step; the caller then sends
    /// `PartySync`, and the answering `PartyState` clears the condition.
    pub fn sync_due(&mut self, now: f64) -> bool {
        if !self.needs_sync {
            return false;
        }
        match self.last_sync_request {
            Some(t) if now - t < SYNC_RETRY_S => false,
            _ => {
                self.last_sync_request = Some(now);
                true
            }
        }
    }

    /// One-line descriptions of what happened since the last call, oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }

    // ----- internals ----------------------------------------------------------------------

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }

    fn note_peak(&mut self) {
        self.peak = self.peak.max(self.party_size());
    }

    /// Cleaned display name of `account` (`account N` if the server never said).
    fn name_of(&self, account: AccountId) -> String {
        self.names.get(&account).map_or_else(|| format!("account {}", account.0), |k| k.name.clone())
    }

    /// Like [`name_of`](Self::name_of), marking this account as `you`.
    fn who(&self, account: AccountId) -> String {
        if account == self.me {
            format!("{} (you)", self.name_of(account))
        } else {
            self.name_of(account)
        }
    }

    fn roster(&self, members: &[AccountId]) -> String {
        members.iter().take(16).map(|m| self.who(*m)).collect::<Vec<_>>().join(", ")
    }

    fn learn_names(&mut self, names: Vec<PartyName>) {
        for n in names {
            if !self.names.contains_key(&n.account) && self.names.len() >= MAX_NAMES {
                continue;
            }
            let name = AvatarAppearance::sanitize_name(&n.name).unwrap_or_else(|| "?".to_string());
            self.names.insert(n.account, KnownPlayer { name, entity: n.entity });
        }
    }

    /// Make `party` ours, if this account is a member of it.
    fn set_party(&mut self, party: Party) {
        if party.contains(self.me) {
            self.party = Some(party);
            self.note_peak();
        }
    }

    fn add_invite(&mut self, invite: PendingInvite) {
        self.invites.insert(invite.id, invite);
        while self.invites.len() > MAX_INVITES {
            self.invites.pop_first();
        }
    }

    /// The roster/leader of the party an event describes disagrees with ours: ask for the truth.
    fn out_of_step(&mut self, what: &str) {
        self.needs_sync = true;
        self.note(format!("{what} does not match this client's party; asking for the full state"));
    }

    fn apply(&mut self, event: PartyEvent) {
        match event {
            PartyEvent::InviteSent { invite, from, to, .. } => {
                self.note(format!("{} invited {} ({invite})", self.who(from), self.who(to)));
                if from == self.me || to == self.me {
                    self.outgoing_invite_seen |= from == self.me;
                    self.add_invite(PendingInvite { id: invite, from, to });
                }
            }
            PartyEvent::InviteDeclined { invite, from, to } => {
                self.invites.remove(&invite);
                self.note(format!("{} declined the invite from {} ({invite})", self.who(to), self.who(from)));
            }
            PartyEvent::InviteExpired { invite, from, to } => {
                self.invites.remove(&invite);
                self.note(format!("the invite from {} to {} expired ({invite})", self.who(from), self.who(to)));
            }
            PartyEvent::InviteCancelled { invite, from, to, reason } => {
                self.invites.remove(&invite);
                self.note(format!(
                    "the invite from {} to {} was cancelled: {reason:?} ({invite})",
                    self.who(from),
                    self.who(to)
                ));
            }
            PartyEvent::PartyFormed { party, invite, leader, members } => {
                self.invites.remove(&invite);
                self.note(format!("{party} formed, led by {}: {}", self.who(leader), self.roster(&members)));
                self.set_party(Party { id: party, leader, members });
            }
            PartyEvent::MemberJoined { party, invite, account, leader, members } => {
                self.invites.remove(&invite);
                self.note(format!("{} joined {party}: {}", self.who(account), self.roster(&members)));
                self.set_party(Party { id: party, leader, members });
            }
            PartyEvent::MemberLeft { party, account, reason, members } => {
                self.note(format!("{} left {party} ({reason:?}): {}", self.who(account), self.roster(&members)));
                if account == self.me {
                    self.party = None;
                } else {
                    match self.party.as_mut() {
                        Some(p) if p.id == party => p.members = members,
                        _ => self.out_of_step("a member left a party that"),
                    }
                }
            }
            PartyEvent::LeaderChanged { party, new_leader, reason, members, .. } => {
                self.note(format!("{} leads {party} now ({reason:?})", self.who(new_leader)));
                match self.party.as_mut() {
                    Some(p) if p.id == party => {
                        p.leader = new_leader;
                        p.members = members;
                    }
                    _ => self.out_of_step("a leader change in a party that"),
                }
            }
            PartyEvent::Disbanded { party, reason, .. } => {
                self.note(format!("{party} disbanded ({reason:?})"));
                match &self.party {
                    Some(p) if p.id != party => self.out_of_step("a disbanded party that"),
                    _ => self.party = None,
                }
            }
        }
    }
}

/// One line about an instance placement (for the log).
pub fn describe_placement(p: &Placement) -> String {
    let party = p.party.map_or_else(|| "solo".to_string(), |id| id.to_string());
    format!(
        "{} ({party}, story advance {})",
        clean(&p.key.area.to_string()),
        if p.story_advance_allowed { "allowed" } else { "not allowed" }
    )
}

/// The peer closest to `from` in area coordinates (horizontal distance; ties go to the lowest
/// id), among `peers` that `skip` does not reject. `None` if there is none (or no finite
/// distance).
pub fn nearest_peer(
    from: Pos,
    peers: impl IntoIterator<Item = (EntityId, Pos)>,
    skip: impl Fn(EntityId) -> bool,
) -> Option<EntityId> {
    peers
        .into_iter()
        .filter(|(id, _)| !skip(*id))
        .map(|(id, pos)| (from.dist_xy(pos), id))
        .filter(|(d, _)| d.is_finite())
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
        .map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AreaId, GameId, PhaseKey};
    use fmv_protocol::{DisbandReason, InstanceKey, InviteCancelReason, LeaderChangeReason, LeaveReason, PartyError, PartyId};

    const ME: AccountId = AccountId(1);
    const TIFA: AccountId = AccountId(2);
    const BARRET: AccountId = AccountId(3);
    const AERITH: AccountId = AccountId(4);

    fn pn(account: AccountId, name: &str, entity: Option<u64>) -> PartyName {
        PartyName { account, name: name.into(), entity: entity.map(EntityId) }
    }

    fn everyone() -> Vec<PartyName> {
        vec![pn(ME, "Cloud", Some(10)), pn(TIFA, "Tifa", Some(20)), pn(BARRET, "Barret", Some(30)), pn(AERITH, "Aerith", None)]
    }

    /// A scripted server-side stream of events for ONE client: numbers them 1, 2, ...
    struct Feed {
        mirror: PartyMirror,
        seq: u64,
    }

    impl Feed {
        fn new(me: AccountId) -> Self {
            Feed { mirror: PartyMirror::new(me), seq: 0 }
        }
        fn send(&mut self, event: PartyEvent) {
            self.seq += 1;
            self.mirror.on_event(self.seq, event, everyone());
        }
        fn members(&self) -> Vec<AccountId> {
            self.mirror.party().map(|p| p.members.clone()).unwrap_or_default()
        }
    }

    fn invite_sent(id: u64, from: AccountId, to: AccountId) -> PartyEvent {
        PartyEvent::InviteSent { invite: InviteId(id), from, to, expires_at: 60_000 }
    }

    fn formed(id: u64, invite: u64, leader: AccountId, other: AccountId) -> PartyEvent {
        PartyEvent::PartyFormed { party: PartyId(id), invite: InviteId(invite), leader, members: vec![leader, other] }
    }

    fn joined(party: u64, invite: u64, account: AccountId, leader: AccountId, members: &[AccountId]) -> PartyEvent {
        PartyEvent::MemberJoined { party: PartyId(party), invite: InviteId(invite), account, leader, members: members.to_vec() }
    }

    #[test]
    fn a_party_of_two_forms_when_an_invite_from_me_is_accepted() {
        let mut f = Feed::new(ME);
        assert_eq!((f.mirror.party_size(), f.mirror.is_leader(), f.mirror.peak_party_size()), (0, false, 0));
        f.send(invite_sent(1, ME, TIFA));
        assert!(f.mirror.outgoing_invite_seen());
        assert_eq!(f.mirror.invites_to_me().count(), 0, "an invite from me is not addressed to me");
        f.send(formed(1, 1, ME, TIFA));
        assert_eq!(f.members(), vec![ME, TIFA]);
        assert_eq!((f.mirror.party_size(), f.mirror.is_leader(), f.mirror.peak_party_size()), (2, true, 2));
        assert!(f.mirror.is_party_account(TIFA) && f.mirror.is_party_account(ME) && !f.mirror.is_party_account(BARRET));
        assert!(f.mirror.is_party_entity(EntityId(20)), "Tifa's entity is in my party");
        assert!(!f.mirror.is_party_entity(EntityId(30)), "Barret's is not");
        assert!(!f.mirror.is_party_entity(EntityId(99)), "an unknown entity is not");
        assert_eq!(f.mirror.invites.len(), 0, "the accepted invite is gone");
        assert!(!f.mirror.needs_sync);
    }

    #[test]
    fn an_invitee_builds_the_whole_party_from_its_first_event() {
        // The invitee sees the invite, then the party formed by accepting it.
        let mut f = Feed::new(ME);
        f.send(invite_sent(7, TIFA, ME));
        let incoming = f.mirror.newest_invite_to_me().expect("an invite for me");
        assert_eq!((incoming.id, incoming.from_name.as_str()), (InviteId(7), "Tifa"));
        assert!(!f.mirror.outgoing_invite_seen());
        f.send(formed(1, 7, TIFA, ME));
        assert_eq!(f.members(), vec![TIFA, ME]);
        assert!(!f.mirror.is_leader());
        assert!(f.mirror.newest_invite_to_me().is_none(), "the consumed invite is gone");
        assert_eq!(f.mirror.peak_party_size(), 2);
    }

    #[test]
    fn a_third_member_joins_and_the_roster_follows_every_event() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        f.send(invite_sent(2, ME, BARRET));
        assert_eq!(f.mirror.invites.len(), 1);
        f.send(joined(1, 2, BARRET, ME, &[ME, TIFA, BARRET]));
        assert_eq!(f.members(), vec![ME, TIFA, BARRET]);
        assert_eq!(f.mirror.party_size(), 3);
        assert_eq!(f.mirror.peak_party_size(), 3);
        assert!(f.mirror.invites.is_empty());
        assert!(f.mirror.is_party_entity(EntityId(30)));
    }

    #[test]
    fn a_leader_who_leaves_changes_the_leader_first_and_then_the_member_list() {
        // I am the second member of [TIFA (leader), ME, BARRET]; the leader leaves.
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, TIFA, ME));
        f.send(formed(1, 1, TIFA, ME));
        f.send(joined(1, 2, BARRET, TIFA, &[TIFA, ME, BARRET]));
        assert!(!f.mirror.is_leader());
        f.send(PartyEvent::LeaderChanged {
            party: PartyId(1),
            old_leader: TIFA,
            new_leader: ME,
            reason: LeaderChangeReason::LeaderLeft,
            members: vec![TIFA, ME, BARRET],
        });
        // The intermediate state is valid: the old leader is still a member of the party.
        assert!(f.mirror.is_leader());
        assert_eq!(f.members(), vec![TIFA, ME, BARRET]);
        assert!(f.mirror.party().unwrap().members.contains(&f.mirror.party().unwrap().leader));
        f.send(PartyEvent::MemberLeft { party: PartyId(1), account: TIFA, reason: LeaveReason::Left, members: vec![ME, BARRET] });
        assert_eq!(f.members(), vec![ME, BARRET]);
        assert!(f.mirror.is_leader());
        assert_eq!(f.mirror.peak_party_size(), 3, "the peak is kept after the party shrinks");
        assert_eq!(f.mirror.party_size(), 2);
        assert!(!f.mirror.is_party_entity(EntityId(20)), "Tifa left");
        assert!(!f.mirror.needs_sync);
    }

    #[test]
    fn being_kicked_ends_my_party_and_a_kick_of_someone_else_shrinks_it() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        f.send(joined(1, 2, BARRET, ME, &[ME, TIFA, BARRET]));
        // Someone else is kicked: I stay, with the new roster.
        f.send(PartyEvent::MemberLeft { party: PartyId(1), account: BARRET, reason: LeaveReason::Kicked, members: vec![ME, TIFA] });
        assert_eq!(f.members(), vec![ME, TIFA]);
        // I am kicked (by the new leader after a promotion): the party is gone for me.
        f.send(PartyEvent::LeaderChanged {
            party: PartyId(1),
            old_leader: ME,
            new_leader: TIFA,
            reason: LeaderChangeReason::Promoted,
            members: vec![ME, TIFA],
        });
        assert!(!f.mirror.is_leader());
        f.send(PartyEvent::MemberLeft { party: PartyId(1), account: ME, reason: LeaveReason::Kicked, members: vec![TIFA] });
        assert_eq!(f.mirror.party_size(), 0);
        assert!(f.mirror.party().is_none());
        assert_eq!(f.mirror.peak_party_size(), 3);
        assert!(!f.mirror.is_party_entity(EntityId(20)), "no party, nobody is in it");
        assert!(!f.mirror.needs_sync);
    }

    #[test]
    fn promote_changes_only_the_leader() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        assert!(f.mirror.is_leader());
        f.send(PartyEvent::LeaderChanged {
            party: PartyId(1),
            old_leader: ME,
            new_leader: TIFA,
            reason: LeaderChangeReason::Promoted,
            members: vec![ME, TIFA],
        });
        assert!(!f.mirror.is_leader());
        assert_eq!(f.mirror.party().unwrap().leader, TIFA);
        assert_eq!(f.members(), vec![ME, TIFA]);
    }

    #[test]
    fn declined_expired_and_cancelled_invites_are_removed() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, TIFA, ME));
        f.send(invite_sent(2, BARRET, ME));
        f.send(invite_sent(3, ME, AERITH));
        assert_eq!(f.mirror.invites.len(), 3);
        // I decline invite 1: both sides are told.
        f.send(PartyEvent::InviteDeclined { invite: InviteId(1), from: TIFA, to: ME });
        assert_eq!(f.mirror.invites_to_me().map(|i| i.id).collect::<Vec<_>>(), vec![InviteId(2)]);
        // The invite I sent timed out.
        f.send(PartyEvent::InviteExpired { invite: InviteId(3), from: ME, to: AERITH });
        assert_eq!(f.mirror.invites.len(), 1);
        // The inviter left their party / joined another: the system cancels the invite.
        f.send(PartyEvent::InviteCancelled {
            invite: InviteId(2),
            from: BARRET,
            to: ME,
            reason: InviteCancelReason::InviterLeftParty,
        });
        assert!(f.mirror.invites.is_empty());
        assert!(f.mirror.newest_invite_to_me().is_none());
        assert!(f.mirror.outgoing_invite_seen(), "it stays true: an invite of mine was seen");
        // A terminal event for an invite we never saw changes nothing (and is not a gap).
        f.send(PartyEvent::InviteDeclined { invite: InviteId(99), from: TIFA, to: ME });
        assert!(!f.mirror.needs_sync);
    }

    #[test]
    fn the_newest_invite_to_me_is_the_one_with_the_highest_id() {
        let mut f = Feed::new(ME);
        // Out-of-order ids on purpose: "newest" is by invite id, not by arrival.
        f.send(invite_sent(9, TIFA, ME));
        f.send(invite_sent(4, BARRET, ME));
        f.send(invite_sent(11, ME, AERITH)); // not addressed to me
        let n = f.mirror.newest_invite_to_me().unwrap();
        assert_eq!((n.id, n.from_name.as_str()), (InviteId(9), "Tifa"));
        f.send(PartyEvent::InviteDeclined { invite: InviteId(9), from: TIFA, to: ME });
        assert_eq!(f.mirror.newest_invite_to_me().unwrap().id, InviteId(4));
    }

    #[test]
    fn accepting_one_invite_cancels_the_others_through_their_own_events() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, TIFA, ME));
        f.send(invite_sent(2, BARRET, ME));
        // I accept invite 1: the party forms, and invite 2 is cancelled in the same operation.
        f.send(formed(1, 1, TIFA, ME));
        f.send(PartyEvent::InviteCancelled {
            invite: InviteId(2),
            from: BARRET,
            to: ME,
            reason: InviteCancelReason::InviteeJoinedParty,
        });
        assert!(f.mirror.invites.is_empty());
        assert_eq!(f.members(), vec![TIFA, ME]);
    }

    #[test]
    fn disbanding_clears_the_party_and_keeps_the_peak() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        f.send(PartyEvent::Disbanded { party: PartyId(1), members: vec![ME], reason: DisbandReason::LastMemberLeft });
        assert_eq!(f.mirror.party_size(), 0);
        assert!(!f.mirror.is_leader());
        assert_eq!(f.mirror.peak_party_size(), 2);
        assert!(!f.mirror.needs_sync);
        // A new party afterwards works as usual.
        f.send(invite_sent(2, ME, BARRET));
        f.send(formed(2, 2, ME, BARRET));
        assert_eq!(f.members(), vec![ME, BARRET]);
    }

    #[test]
    fn a_party_state_replaces_everything() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, TIFA, ME));
        f.send(invite_sent(2, ME, AERITH));
        f.send(formed(1, 1, TIFA, ME));
        assert_eq!((f.mirror.party_size(), f.mirror.invites.len()), (2, 1));
        // The server's truth: a different party, one invite to me, none from me.
        let party = Party { id: PartyId(5), leader: ME, members: vec![ME, BARRET, AERITH] };
        let invites = vec![Invite { id: InviteId(8), from: TIFA, to: ME, created_at: 5, expires_at: 65 }];
        f.mirror.on_state(40, Some(party), invites, everyone());
        assert_eq!(f.members(), vec![ME, BARRET, AERITH]);
        assert!(f.mirror.is_leader());
        assert_eq!(f.mirror.invites.keys().copied().collect::<Vec<_>>(), vec![InviteId(8)]);
        assert_eq!(f.mirror.newest_invite_to_me().unwrap().id, InviteId(8));
        assert_eq!(f.mirror.peak_party_size(), 3);
        assert_eq!(f.mirror.last_seq, 40);
        // ...and a state with no party and no invites clears them.
        f.mirror.on_state(41, None, vec![], vec![]);
        assert_eq!((f.mirror.party_size(), f.mirror.invites.len()), (0, 0));
        assert_eq!(f.mirror.peak_party_size(), 3, "the peak is history");
        // Events continue from the state's seq.
        f.mirror.on_event(42, invite_sent(9, TIFA, ME), everyone());
        assert_eq!(f.mirror.invites.len(), 1);
        assert!(!f.mirror.needs_sync);
    }

    #[test]
    fn a_party_state_only_keeps_a_party_that_lists_me_and_invites_that_concern_me() {
        let mut m = PartyMirror::new(ME);
        let foreign = Party { id: PartyId(2), leader: TIFA, members: vec![TIFA, BARRET] };
        let invites = vec![
            Invite { id: InviteId(1), from: TIFA, to: BARRET, created_at: 0, expires_at: 9 },
            Invite { id: InviteId(2), from: ME, to: AERITH, created_at: 0, expires_at: 9 },
        ];
        m.on_state(3, Some(foreign), invites, vec![]);
        assert_eq!(m.party_size(), 0, "somebody else's party is not mine");
        assert_eq!(m.invites.keys().copied().collect::<Vec<_>>(), vec![InviteId(2)]);
        assert!(m.outgoing_invite_seen(), "an invite from me in the state counts as seen");
        assert_eq!(m.peak_party_size(), 0);
    }

    #[test]
    fn a_gap_in_seq_asks_for_a_sync_once_and_the_state_clears_it() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        assert!(!f.mirror.sync_due(0.0), "in step: nothing to ask");
        // Events 2 and 3 are lost; 4 arrives.
        f.mirror.on_event(4, formed(1, 1, ME, TIFA), everyone());
        assert!(f.mirror.needs_sync);
        // The event itself is still applied (best effort until the state arrives).
        assert_eq!(f.members(), vec![ME, TIFA]);
        // Rate limited: asked now, not again until SYNC_RETRY_S has passed.
        assert!(f.mirror.sync_due(10.0));
        assert!(!f.mirror.sync_due(10.5));
        assert!(!f.mirror.sync_due(10.0 + SYNC_RETRY_S - 0.01));
        assert!(f.mirror.sync_due(10.0 + SYNC_RETRY_S), "still out of step: ask again");
        // The server answers with its state as of event 5.
        let party = Party { id: PartyId(1), leader: ME, members: vec![ME, TIFA] };
        f.mirror.on_state(5, Some(party), vec![], everyone());
        assert!(!f.mirror.needs_sync);
        assert!(!f.mirror.sync_due(100.0));
        // Events after the state continue from its seq; older ones are not applied again.
        f.mirror.on_event(6, joined(1, 2, BARRET, ME, &[ME, TIFA, BARRET]), everyone());
        assert_eq!(f.members(), vec![ME, TIFA, BARRET]);
        assert!(!f.mirror.needs_sync);
        let before = f.mirror.party().cloned();
        f.mirror.on_event(5, PartyEvent::Disbanded { party: PartyId(1), members: vec![ME], reason: DisbandReason::LastMemberLeft }, everyone());
        assert_eq!(f.mirror.party().cloned(), before, "an event the state already included is not applied");
        assert!(f.mirror.needs_sync, "but the numbering was off, so the truth is asked for");
    }

    #[test]
    fn the_first_event_missing_is_a_gap_too() {
        let mut m = PartyMirror::new(ME);
        m.on_event(2, invite_sent(1, TIFA, ME), everyone());
        assert!(m.needs_sync && m.sync_due(0.0));
        assert_eq!(m.invites.len(), 1);
    }

    #[test]
    fn an_event_that_contradicts_the_mirror_asks_for_a_sync() {
        // A roster change for a party I never heard of.
        let mut f = Feed::new(ME);
        f.send(PartyEvent::MemberLeft { party: PartyId(9), account: TIFA, reason: LeaveReason::Left, members: vec![ME] });
        assert!(f.mirror.needs_sync);
        assert_eq!(f.mirror.party_size(), 0, "nothing is invented");
        let mut f = Feed::new(ME);
        f.send(PartyEvent::LeaderChanged {
            party: PartyId(9),
            old_leader: TIFA,
            new_leader: ME,
            reason: LeaderChangeReason::LeaderLeft,
            members: vec![TIFA, ME],
        });
        assert!(f.mirror.needs_sync);
        // A disband of another party than mine.
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        f.send(PartyEvent::Disbanded { party: PartyId(77), members: vec![ME], reason: DisbandReason::LastMemberLeft });
        assert!(f.mirror.needs_sync);
        assert_eq!(f.mirror.party_size(), 2, "my party is untouched");
    }

    #[test]
    fn a_party_that_does_not_list_me_is_not_mine() {
        let mut f = Feed::new(ME);
        f.send(formed(1, 1, TIFA, BARRET));
        assert_eq!(f.mirror.party_size(), 0);
        assert_eq!(f.mirror.peak_party_size(), 0);
    }

    #[test]
    fn rejections_are_counted_and_their_text_is_clean() {
        let mut m = PartyMirror::new(ME);
        assert_eq!((m.rejection_count(), m.last_rejection()), (0, None));
        m.on_rejected(&PartyFailure::UnknownPlayer);
        assert_eq!((m.rejection_count(), m.last_rejection()), (1, Some("that player is not here")));
        m.on_rejected(&PartyFailure::RateLimited);
        assert_eq!(m.rejection_count(), 2);
        assert!(m.last_rejection().unwrap().contains("too many invites"));
        let full = PartyFailure::Rules(PartyError::PartyFull { party: PartyId(2), max_size: 3 });
        m.on_rejected(&full);
        assert!(m.last_rejection().unwrap().contains("max 3"), "{:?}", m.last_rejection());
        // A rule error that carries server text is cleaned like any server text.
        m.on_rejected(&PartyFailure::Rules(PartyError::CorruptState("bad\u{1b}[31m\nSMOKE name=forged".into())));
        let text = m.last_rejection().unwrap();
        assert!(!text.chars().any(char::is_control), "{text:?}");
        let notes = m.drain_notes();
        assert_eq!(notes.len(), 4);
        assert!(notes.iter().all(|n| n.starts_with("request refused: ") && !n.contains('\n')), "{notes:?}");
    }

    #[test]
    fn the_last_placement_is_remembered() {
        let mut m = PartyMirror::new(ME);
        assert!(m.last_placement().is_none());
        let placement = |party: Option<u64>, allowed: bool| Placement {
            account: ME,
            key: InstanceKey::new(AreaId { game: GameId::Fixture, key: "plaza".into() }, PhaseKey(vec![])),
            party: party.map(PartyId),
            phase_owner: ME,
            story_advance_allowed: allowed,
        };
        m.on_placement(placement(None, true));
        assert_eq!(m.last_placement().unwrap().party, None);
        m.on_placement(placement(Some(3), false));
        let last = m.last_placement().unwrap();
        assert_eq!((last.party, last.story_advance_allowed), (Some(PartyId(3)), false));
        let line = describe_placement(last);
        assert!(line.contains("Fixture/plaza") && line.contains("party#3") && line.contains("not allowed"), "{line}");
        assert!(describe_placement(&placement(None, true)).contains("solo"));
    }

    #[test]
    fn the_summary_is_one_line_about_peak_now_refusals_and_placement() {
        let mut f = Feed::new(ME);
        let line = f.mirror.summary();
        assert!(line.contains("peak party size 0") && line.contains("no party") && line.contains("no refusals"), "{line}");
        assert!(line.contains("placement: none"), "{line}");
        f.send(invite_sent(1, ME, TIFA));
        f.send(formed(1, 1, ME, TIFA));
        f.mirror.on_rejected(&PartyFailure::RateLimited);
        let line = f.mirror.summary();
        assert!(line.contains("peak party size 2") && line.contains("a party of 2, leading it"), "{line}");
        assert!(line.contains("1 refusal(s), the last: too many invites"), "{line}");
        assert!(!line.contains('\n'), "{line:?}");
    }

    #[test]
    fn names_are_cleaned_bounded_and_updated() {
        let mut m = PartyMirror::new(ME);
        m.on_event(1, invite_sent(1, TIFA, ME), vec![pn(TIFA, "\u{1b}[31mTi\u{200b}fa\u{7}\n", Some(5)), pn(ME, "\u{200b}\u{202e}", None)]);
        assert_eq!(m.name_of(TIFA), "[31mTifa");
        assert_eq!(m.name_of(ME), "?", "a name that is nothing visible is `?`");
        assert_eq!(m.name_of(AccountId(77)), "account 77", "an account the server never named");
        // A later list updates the entry (the player moved out of the area: no entity any more).
        m.on_event(2, invite_sent(2, TIFA, ME), vec![pn(TIFA, "Tifa", None)]);
        assert_eq!(m.name_of(TIFA), "Tifa");
        assert_eq!(m.names[&TIFA].entity, None);
        // The table is bounded: new accounts beyond the cap are ignored, known ones still update.
        let flood: Vec<PartyName> = (1000..1000 + 2 * MAX_NAMES as u64).map(|i| pn(AccountId(i), "x", None)).collect();
        m.on_event(3, invite_sent(3, TIFA, ME), flood);
        assert_eq!(m.names.len(), MAX_NAMES);
        m.on_event(4, invite_sent(4, TIFA, ME), vec![pn(TIFA, "Renamed", Some(1))]);
        assert_eq!(m.name_of(TIFA), "Renamed");
    }

    #[test]
    fn invites_and_notes_are_bounded() {
        let mut f = Feed::new(ME);
        for i in 0..(MAX_INVITES as u64 + 50) {
            f.send(invite_sent(i + 1, TIFA, ME));
        }
        assert_eq!(f.mirror.invites.len(), MAX_INVITES);
        assert_eq!(f.mirror.invites.keys().next().copied(), Some(InviteId(51)), "the oldest were forgotten");
        assert_eq!(f.mirror.drain_notes().len(), MAX_NOTES);
        assert!(f.mirror.drain_notes().is_empty(), "drained");
        // Hostile content never panics: empty rosters, huge ids.
        f.send(PartyEvent::PartyFormed { party: PartyId(u64::MAX), invite: InviteId(u64::MAX), leader: ME, members: vec![] });
        f.send(PartyEvent::MemberLeft { party: PartyId(u64::MAX), account: AccountId(u64::MAX), reason: LeaveReason::Left, members: vec![] });
        f.mirror.on_event(u64::MAX, invite_sent(1, TIFA, ME), vec![]);
        f.mirror.on_event(0, invite_sent(1, TIFA, ME), vec![]);
        f.mirror.on_state(u64::MAX, None, vec![], vec![]);
        f.mirror.on_event(1, invite_sent(1, TIFA, ME), vec![]);
        assert!(f.mirror.needs_sync);
    }

    #[test]
    fn every_note_is_a_single_clean_line() {
        let mut f = Feed::new(ME);
        f.send(invite_sent(1, TIFA, ME));
        f.send(formed(1, 1, TIFA, ME));
        f.send(joined(1, 2, BARRET, TIFA, &[TIFA, ME, BARRET]));
        f.send(PartyEvent::LeaderChanged {
            party: PartyId(1),
            old_leader: TIFA,
            new_leader: ME,
            reason: LeaderChangeReason::LeaderLeft,
            members: vec![TIFA, ME, BARRET],
        });
        f.send(PartyEvent::MemberLeft { party: PartyId(1), account: TIFA, reason: LeaveReason::Left, members: vec![ME, BARRET] });
        f.send(PartyEvent::Disbanded { party: PartyId(1), members: vec![ME], reason: DisbandReason::LastMemberLeft });
        let notes = f.mirror.drain_notes();
        assert_eq!(notes.len(), 6);
        assert!(notes.iter().all(|n| !n.contains('\n') && !n.chars().any(char::is_control)), "{notes:?}");
        assert!(notes[0].contains("Tifa invited Cloud (you)"), "{}", notes[0]);
        assert!(notes[1].contains("party#1 formed") && notes[1].contains("Tifa"), "{}", notes[1]);
    }

    #[test]
    fn the_nearest_peer_is_chosen_by_area_distance_and_ties_go_to_the_lowest_id() {
        let at = |x: f32, y: f32| Pos::new(x, y, 0.0);
        let me = at(0.0, 0.0);
        let peers = vec![(EntityId(9), at(30.0, 40.0)), (EntityId(3), at(0.0, -20.0)), (EntityId(5), at(20.0, 0.0)), (EntityId(7), at(-100.0, 0.0))];
        // 3 and 5 are both 20 away: the lower id wins.
        assert_eq!(nearest_peer(me, peers.clone(), |_| false), Some(EntityId(3)));
        // Skipping the closest ones.
        assert_eq!(nearest_peer(me, peers.clone(), |id| id == EntityId(3)), Some(EntityId(5)));
        assert_eq!(nearest_peer(me, peers.clone(), |id| id.0 < 8), Some(EntityId(9)));
        assert_eq!(nearest_peer(me, peers.clone(), |_| true), None);
        assert_eq!(nearest_peer(me, Vec::new(), |_| false), None);
        // Height does not count, and non-finite positions are never chosen.
        let tall = vec![(EntityId(1), Pos::new(10.0, 0.0, 9999.0)), (EntityId(2), at(f32::NAN, 0.0)), (EntityId(3), at(f32::INFINITY, 0.0))];
        assert_eq!(nearest_peer(me, tall, |_| false), Some(EntityId(1)));
        assert_eq!(nearest_peer(at(f32::NAN, 0.0), peers, |_| false), None);
    }
}
