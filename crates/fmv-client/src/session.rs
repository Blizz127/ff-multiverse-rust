//! A joined session: the local area, the replicated state and the network handle, shared by the
//! headless bot loop and the window loop.

use crate::battle::BattleCmd;
use crate::net::{Joined, NetCommand, NetEvent, NetHandle, PartyCommand};
use crate::state::ClientState;
use fmv_adapter_api::AreaRuntime;
use fmv_core::{AreaId, AvatarAppearance, EntityId, EntitySnapshot, Pos};
use fmv_protocol::BattleRules;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What we know about the joined area (for titles, banners and logs).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Our display name as the SERVER shows it (the stored profile, not the command line).
    pub name: String,
    /// Our tint as the server shows it.
    pub tint: [u8; 3],
    pub area_id: AreaId,
    /// True for synthetic FIXTURE content, which must be labelled everywhere it is surfaced.
    pub fixture: bool,
}

/// Shown for our own entity if the first snapshot (against the protocol) does not contain it.
const UNKNOWN_NAME: &str = "?";
const UNKNOWN_TINT: [u8; 3] = [255, 255, 255];

/// Our display name and tint, as the server describes our own entity in the first snapshot. The
/// server is the authority: after registration it takes both from the stored avatar profile, so
/// the command line says nothing about them. Server text is cleaned like any other.
pub fn own_identity(entities: &[EntitySnapshot], entity: EntityId) -> (String, [u8; 3]) {
    match entities.iter().find(|e| e.id == entity) {
        Some(e) => (
            AvatarAppearance::sanitize_name(&e.avatar.display_name).unwrap_or_else(|| UNKNOWN_NAME.to_string()),
            e.avatar.tint,
        ),
        None => (UNKNOWN_NAME.to_string(), UNKNOWN_TINT),
    }
}

pub struct Session {
    pub net: NetHandle,
    /// The area decoded from this machine's own content (used for local movement validation).
    pub area: Box<dyn AreaRuntime>,
    pub state: ClientState,
    pub info: SessionInfo,
    origin: Instant,
    /// The session already has to end because of a message from the handshake backlog; the first
    /// `pump` reports it.
    pending_end: Option<String>,
}

impl Session {
    /// Build the session from a finished handshake and apply the messages that arrived with it.
    pub fn new(net: NetHandle, joined: Joined) -> Self {
        let Joined {
            entity,
            account,
            area_id,
            fixture,
            area,
            entities,
            backlog,
            ..
        } = joined;
        let spawn = entities
            .iter()
            .find(|e| e.id == entity && e.pos.is_finite())
            .map(|e| e.pos)
            .or_else(|| area.spawn_points().first().copied())
            .unwrap_or(Pos::new(0.0, 0.0, 0.0));
        let origin = Instant::now();
        let mut state = ClientState::new(entity, account, spawn, area_id.clone());
        let mut pending_end = None;
        for msg in backlog {
            // The handshake already refuses a Reject, a foreign snapshot and a second manifest, so
            // nothing here should end the session; if something does, say so on the first pump.
            if let Some(reason) = state.apply(0.0, msg) {
                pending_end.get_or_insert(reason);
            }
        }
        let (name, tint) = own_identity(&entities, entity);
        let info = SessionInfo {
            name,
            tint,
            area_id,
            fixture,
        };
        Self {
            net,
            area,
            state,
            info,
            origin,
            pending_end,
        }
    }

    /// Seconds since the session started (monotonic).
    pub fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    /// Apply everything the network thread has queued. Returns `Some(reason)` when the session
    /// ended abnormally (connection lost, or the server ended it).
    pub fn pump(&mut self) -> Option<String> {
        if let Some(reason) = self.pending_end.take() {
            return Some(reason);
        }
        while let Some(ev) = self.net.try_next() {
            let now = self.now();
            match ev {
                NetEvent::Server(msg) => {
                    if let Some(reason) = self.state.apply(now, msg) {
                        return Some(reason);
                    }
                }
                NetEvent::Rtt(rtt) => self.state.set_rtt(rtt),
                NetEvent::Closed(reason) => return Some(format!("connection to the server lost: {reason}")),
                // Only produced before the session starts.
                NetEvent::Joined(_) | NetEvent::Failed(_) => {}
            }
        }
        None
    }

    /// Send the next `MoveIntent` if one is due (rate limited, only on change).
    pub fn flush_intent(&mut self) {
        let now = self.now();
        if let Some(intent) = self.state.own.next_intent(now) {
            self.net.send(NetCommand::Intent(intent));
        }
    }

    /// Send a party request (the server decides and answers with `PartyEvent` / `PartyRejected`).
    pub fn send_party(&self, cmd: PartyCommand) {
        self.net.send(NetCommand::Party(cmd));
    }

    /// Ask the server for the full party state if the mirror noticed it is out of step. Cheap to
    /// call every tick: the mirror rate limits the request.
    pub fn flush_party_sync(&mut self) {
        let now = self.now();
        if self.state.party.sync_due(now) {
            self.send_party(PartyCommand::Sync);
        }
    }

    /// Print what happened to the party since the last call, one `party:` line each, on stderr
    /// (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_party_notes(&mut self) {
        for line in self.state.party.drain_notes() {
            eprintln!("party: {line}");
        }
    }

    /// Give the battle mirror this machine's own battle data (`None`: it cannot replay battles).
    pub fn set_battle_rules(&mut self, rules: Option<Arc<dyn BattleRules>>) {
        self.state.battle.set_rules(rules);
    }

    /// Send a battle request or command (the server decides and answers with battle messages or
    /// `BattleRejected`). A command is remembered as in flight, so it is not sent twice.
    pub fn send_battle(&mut self, cmd: BattleCmd) {
        let now = self.now();
        self.state.battle.note_sent(&cmd, now);
        self.net.send(NetCommand::Battle(cmd));
    }

    /// Print what happened in battle since the last call, one `battle:` line each, on stderr
    /// (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_battle_notes(&mut self) {
        for line in self.state.battle.drain_notes() {
            eprintln!("battle: {line}");
        }
    }

    /// Say goodbye and wait (bounded) for the network thread to flush `Leave`.
    pub fn leave(&mut self, wait: Duration) {
        self.net.leave_and_join(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::AnimState;

    fn entity(id: u64, name: &str, tint: [u8; 3]) -> EntitySnapshot {
        EntitySnapshot {
            id: EntityId(id),
            pos: Pos::default(),
            facing: 0.0,
            anim: AnimState::Idle,
            avatar: AvatarAppearance::new(name, tint, None),
        }
    }

    #[test]
    fn our_name_and_tint_come_from_our_own_entity_in_the_snapshot() {
        let entities = [entity(8, "Peer", [9, 9, 9]), entity(7, "Cloud", [1, 2, 3])];
        assert_eq!(own_identity(&entities, EntityId(7)), ("Cloud".to_string(), [1, 2, 3]));
        assert_eq!(own_identity(&entities, EntityId(8)), ("Peer".to_string(), [9, 9, 9]));
    }

    #[test]
    fn a_hostile_or_missing_own_entity_still_gives_a_printable_identity() {
        // Server text is cleaned: no escape sequences or forged lines reach the title or the log.
        let hostile = [entity(7, "\u{1b}]0;PWNED\u{7}Cl\u{200b}oud\nSMOKE", [4, 5, 6])];
        let (name, tint) = own_identity(&hostile, EntityId(7));
        assert!(!name.chars().any(char::is_control), "{name:?}");
        assert!(name.starts_with("]0;PWNED") && name.contains("Cloud"), "{name:?}");
        assert_eq!(tint, [4, 5, 6]);
        // A name that is nothing visible, and an entity the snapshot does not contain.
        assert_eq!(own_identity(&[entity(7, "\u{200b}", [1, 1, 1])], EntityId(7)).0, "?");
        assert_eq!(own_identity(&[], EntityId(7)), ("?".to_string(), [255, 255, 255]));
        assert_eq!(own_identity(&[entity(1, "x", [0, 0, 0])], EntityId(7)).0, "?");
    }
}
