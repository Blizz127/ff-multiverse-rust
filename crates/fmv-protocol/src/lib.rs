//! FF Multiverse wire protocol v3.
//!
//! Framing: `u32` little-endian payload length, then the payload. The payload codec is JSON
//! (serde_json; postcard is not in the local crate cache). The codec is isolated in
//! [`encode`]/[`decode`] so it can be swapped for a binary codec with a version bump.
//!
//! Invariant: no message carries game asset bytes. Areas are agreed via [`AreaId`] +
//! [`Fingerprint`] + path-free [`ProvenanceSummary`]; each peer reads its own owned copy.
//!
//! # Connection flow
//!
//! ```text
//! C -> S  Hello { protocol_version, client_build, auth, tint, appearance_ref }
//! S -> C  Welcome { .., account, issued_token }      (or Reject { reason, code }, then the server closes)
//! S -> C  AreaManifest { area, fingerprint, provenance, fixture, .. }
//! C -> S  JoinArea { area, fingerprint, provenance }  (or CannotJoin { reason })
//! S -> C  AreaSnapshot, InstancePlacement             (always first after the handshake replies)
//! ...     MoveIntent / EntityUpdate / MoveRejected / Ping / Pong / Party* / Battle* ...
//! C -> S  Leave
//! ```
//!
//! # Battles (v3)
//!
//! A battle is started by the server for a whole party (or a solo player), either from a random
//! encounter roll in a field that has an encounter table, or on `RequestEncounter` when the
//! server runs with `--debug-encounters`. Every member receives the same
//! `EncounterStart { battle_id, formation_id, seed, party, .. }`, is FROZEN in the field (its
//! `MoveIntent`s are ignored and nobody else sees it move) and rebuilds the battle from its OWN
//! `scene.bin` and `kernel.bin`: only ids, seeds and levels travel (see [`battle`]). The server
//! steps the engine and announces what happened as `BattleEvent { battle_id, tick, events,
//! command }`; a client commands its own character with `BattleCommand` once its gauge is full
//! and the server answers a refused one with `BattleRejected`. A won or lost battle ends with
//! `BattleEnd { outcome, rewards }`, after which everyone is back in the field where they stood.
//!
//! ```text
//! (C -> S  RequestEncounter { formation_id }            debug servers only)
//! S -> C  EncounterStart { battle_id, formation_id, seed, party, .. }     every member
//! S -> C  BattleEvent { battle_id, tick, events }                         the engine advanced
//! C -> S  BattleCommand { battle_id, actor, command }                     gauge full
//! S -> C  BattleEvent { battle_id, tick, events, command }                every member
//! S -> C  BattleEvent { .., events: [.., Victory] }, then BattleEnd { battle_id, outcome, rewards }
//! ```
//!
//! # Area travel (additive in v3)
//!
//! A server hosts many areas. The one in the handshake is the entry area; a player leaves it by
//! walking across one of its exit lines (`fmv_adapter_api::Exit`), which the SERVER detects on the
//! move it applies. The player is then in transit: nobody sees it, and its `MoveIntent`s are
//! ignored (not answered) until it answers. Nothing new is asked of the client: it answers an
//! [`ServerMsg::AreaChange`] exactly as it answers an [`ServerMsg::AreaManifest`], from its OWN
//! copy of the target area (fingerprint and provenance are checked on both sides, no asset bytes
//! travel).
//!
//! ```text
//! (S -> old peers  PlayerLeft { entity })
//! S -> C  AreaChange { area, display_name, fingerprint, provenance, fixture, arrival, arrival_facing }
//! C -> S  JoinArea { area, fingerprint, provenance }   (or CannotJoin: the server sends it back)
//! S -> C  AreaSnapshot { area, tick, entities, npcs }  (always followed directly by InstancePlacement)
//! (S -> new peers  PlayerJoined { snapshot })
//! ```
//!
//! `arrival` is where the server will put the player (already settled on the target's walkable
//! surface) and `arrival_facing` its heading; the snapshot repeats both in the player's own entity.
//! A client that cannot hold the area answers `CannotJoin`; the server then sends the player back
//! where it was with a second `AreaChange` (for the area it came from), which is answered the same
//! way. A player that does not answer within the join timeout is disconnected. An old v3 client
//! does not know `AreaChange` and disconnects at the first exit it crosses (the protocol version
//! was not bumped; bump it when builds are mixed in a deployment).
//!
//! `AreaSnapshot::npcs` lists the non-player characters the area's data places ([`NpcSnapshot`]):
//! static, with no `EntityUpdate`s of their own, sent with every snapshot of the area. It is left
//! out of the JSON when empty, so a snapshot of an area without any is byte-for-byte what it was.
//!
//! # Accounts
//!
//! `Hello::auth` is [`Auth::Register`] (create an account with that display name) or
//! [`Auth::Token`] (log in with the token a registration issued). A registration is answered by a
//! `Welcome` whose `issued_token` is `Some`: **the only time the token is ever sent**; the server
//! keeps just a hash. A client must store it before doing anything else (a registered name is
//! unique and cannot be recovered without the token). The server creates the account and its avatar
//! in one step and releases the account again if that `Welcome` is never written to the socket, so a
//! connection that dies at that moment does not leave a name taken without a token.
//!
//! After registration the server never trusts client-supplied names: the display name, tint and
//! per-game appearance of an entity come from the stored avatar profile. `Hello::tint` and
//! `Hello::appearance_ref` are only the look requested for a NEW avatar (ignored when logging in
//! with a token).
//!
//! # Rejections
//!
//! A refusal (or the end of a session the server decides to close) is `Reject { reason, code }`:
//! `reason` is a sentence for the screen and the log, `code` is a [`RejectCode`] a program acts
//! on. Never match on the sentence. The field is additive within v3 (see [`reject`] for why old
//! and new peers still understand each other).
//!
//! # Parties
//!
//! Clients ask (`PartyInvite`, `PartyAccept`, ...) and the server decides, acting for the
//! account the SESSION belongs to (an account id in a message is never an actor). Targets are
//! [`EntityId`]s of players in the area. Every state change is announced as
//! [`ServerMsg::PartyEvent`] to exactly the accounts `fmv_party::PartyEvent::audience` names; a
//! client mirror that applies every event it receives, in order, reproduces the server's view of
//! its party and pending invites. `seq` counts the events delivered to THIS client (1, 2, ...), so
//! a gap means loss and the client asks for [`ClientMsg::PartySync`]; [`ServerMsg::PartyState`]
//! carries the `seq` of the last event it includes. A request the server refuses is answered with
//! [`ServerMsg::PartyRejected`] to the requester only. A member who disconnects leaves the party.
//!
//! The party types on the wire are `fmv-party`'s own serde types, pinned by golden JSON tests
//! below: if they change, those tests fail instead of the wire changing silently.

use std::fmt;

use fmv_core::{AccountId, AnimState, AreaId, EntityId, EntitySnapshot, Fingerprint, Pos, ProvenanceSummary, SessionId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub mod battle;
pub mod reject;

pub use reject::RejectCode;

/// The battle wire types (the engine's own `BattleEvent`, `Command` and `Outcome` stay under
/// [`battle`], where their names say what they are).
pub use battle::{
    split_rewards, ActorId, AppliedCommand, BattleBuildError, BattleId, BattleMember, BattleRejection, BattleReplay, BattleReward,
    BattleRules, BattleSetup, Divergence, Ff7BattleData, Ff7BattleDataError, FormationInfo, SyntheticRules, MAX_BATTLE_PARTY,
};

/// The party types carried on the wire, re-exported so a client needs no direct `fmv-party`
/// dependency to match on them.
pub use fmv_party::{
    DisbandReason, InstanceKey, Invite, InviteCancelReason, InviteId, LeaderChangeReason, LeaveReason, Party, PartyError,
    PartyEvent, PartyId, Placement,
};

pub const PROTOCOL_VERSION: u16 = 3;
/// Hard cap on a single frame payload (bytes).
pub const MAX_FRAME: u32 = 256 * 1024;
/// Default TCP port for development. Deployment port is UNASSIGNED (see DEPLOYMENT-CONTRACT.md).
pub const DEFAULT_DEV_PORT: u16 = 47_770;

/// A secret string (an account token). It serializes as a plain JSON string, but `Debug` never
/// shows it, so a message printed with `{:?}` (a log line, a failed assertion) cannot leak it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret text. Call it only where the secret is really used (sending it, saving it).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// How a client proves who it is (see the module docs, "Accounts").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Auth {
    /// Log in with the token a registration issued.
    Token(Secret),
    /// Create an account with this display name (cleaned and admitted by
    /// `AvatarAppearance::validate_new_name`, and unique by `fmv_core::name_key`: case, width,
    /// invisible characters and look-alike letters make no difference). A name that is unusable is
    /// refused with `RejectCode::InvalidName`, one that is taken with `RejectCode::NameTaken`. The
    /// reply carries the new token.
    Register { display_name: String },
}

// =============================================================================================
// Area travel (additive in v3): non-player characters.
// =============================================================================================

/// Most NPCs one `AreaSnapshot` carries. With 256 players (about 140 KB) a full snapshot must
/// still fit one frame: a sender clips an NPC's name to 24 characters and its model and animation
/// to 16 ([`NPC_NAME_CHARS`], [`NPC_REF_CHARS`]), which is at most about 350 bytes of JSON per NPC
/// (all characters 4 bytes wide, extreme floats) and about 45 KB for the list. Enforced by the
/// sender; a receiver bounds what it keeps by the same numbers.
pub const MAX_NPCS_PER_AREA: usize = 128;
/// Longest NPC name a sender puts in an [`NpcSnapshot`] (characters).
pub const NPC_NAME_CHARS: usize = 24;
/// Longest model and animation reference a sender puts in an [`NpcSnapshot`] (characters).
pub const NPC_REF_CHARS: usize = 16;

/// A non-player character of an area: placed by the area's own data, standing still where and
/// facing the way it says. Carries references only, never mesh or texture bytes: the client
/// resolves `model` from its own copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NpcSnapshot {
    /// Index in the area's NPC list, stable while the area is hosted (not an `EntityId`: NPCs
    /// are not players and never appear in `EntityUpdate`).
    pub id: u32,
    /// The character's name in the area's data (FF7: the script entity name, `gu0`). For labels.
    pub name: String,
    /// Adapter-defined model reference (FF7: the `char.lgp` skeleton stem, `aaaa`).
    pub model: String,
    /// The clip that shows it standing, if the data names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_animation: Option<String>,
    pub pos: Pos,
    /// Heading in radians, adapter space.
    pub facing: f32,
}

/// Who an account id in a party message is. Entities are only known for accounts that are
/// players in this area right now (everyone in a party is, because a disconnect leaves it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyName {
    pub account: AccountId,
    /// The account's display name (what other players see).
    pub name: String,
    /// The player's entity in this area, so a client can mark it.
    pub entity: Option<EntityId>,
}

/// Why a party request was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyFailure {
    /// The party rules refused it (not the leader, party full, unknown invite, ...).
    Rules(PartyError),
    /// The target entity is not a player in this area.
    UnknownPlayer,
    /// Too many invites too quickly.
    RateLimited,
}

impl fmt::Display for PartyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartyFailure::Rules(e) => write!(f, "{e}"),
            PartyFailure::UnknownPlayer => f.write_str("that player is not here"),
            PartyFailure::RateLimited => f.write_str("too many invites; wait a moment"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message of a connection. `tint` and `appearance_ref` are used only to create a NEW
    /// avatar (when `auth` is `Register`). The server keeps `appearance_ref` only if it is one of the
    /// look ids the hosted game offers (for FF7 the `ff7/<character>` ids); any other value is not an
    /// error, the avatar just keeps the default look.
    Hello {
        protocol_version: u16,
        client_build: String,
        auth: Auth,
        tint: [u8; 3],
        appearance_ref: Option<String>,
    },
    /// Sent after the client confirmed it holds the area with this exact fingerprint locally.
    /// `provenance` is the client's OWN path-free provenance: the server requires it to be
    /// `compatible_with` the area's, in addition to the fingerprint matching.
    JoinArea { area: AreaId, fingerprint: Fingerprint, provenance: ProvenanceSummary },
    /// Client cannot join: it lacks the area or has an incompatible edition.
    CannotJoin { area: AreaId, reason: String },
    MoveIntent { seq: u32, pos: Pos, facing: f32, anim: AnimState },
    Ping { nonce: u64 },
    Leave,
    /// Invite the player with this entity into your party (or to form one).
    PartyInvite { target: EntityId },
    /// Accept an invite addressed to you (an id from a `PartyEvent::InviteSent`).
    PartyAccept { invite: InviteId },
    PartyDecline { invite: InviteId },
    PartyLeave,
    /// Leader only: remove a member.
    PartyKick { target: EntityId },
    /// Leader only: hand leadership to a member.
    PartyPromote { target: EntityId },
    /// Ask for the full party state (answered by `PartyState`).
    PartySync,
    /// Command one of YOUR characters in a battle: `actor` must be the party member the server
    /// assigned to this session, and its ATB gauge must be full. Refused with `BattleRejected`.
    BattleCommand { battle_id: BattleId, actor: ActorId, command: battle::Command },
    /// Debug only (the server must run with `--debug-encounters`; refused with `BattleRejected`
    /// otherwise): start a battle for your party (or just you). `formation_id` names the enemy
    /// formation (`scene * 4 + slot`); `None` lets the server choose.
    RequestEncounter { formation_id: Option<u16> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        protocol_version: u16,
        server_build: String,
        session: SessionId,
        /// This player's entity in the area (the one `AreaSnapshot`/`EntityUpdate` describe).
        entity: EntityId,
        /// The authenticated account. Party events name accounts, so this is how a client
        /// recognises itself in them.
        account: AccountId,
        /// `Some` only in the answer to `Auth::Register`: the new account's token, **shown
        /// once**. Never log this message with `{:?}` without redaction (`Secret` redacts).
        issued_token: Option<Secret>,
    },
    /// The server refuses or ends the connection and closes it. `reason` is for people, `code` for
    /// programs (`Unspecified`, and then absent from the JSON, from a server that sends none).
    Reject {
        reason: String,
        #[serde(default, skip_serializing_if = "RejectCode::is_unspecified")]
        code: RejectCode,
    },
    /// Offer of the area the server is hosting. `fixture == true` must be shown to the user.
    AreaManifest {
        area: AreaId,
        display_name: String,
        fingerprint: Fingerprint,
        provenance: ProvenanceSummary,
        fixture: bool,
    },
    AreaSnapshot {
        area: AreaId,
        tick: u64,
        entities: Vec<EntitySnapshot>,
        /// The area's non-player characters (static scenery; see the module docs, "Area travel").
        /// At most [`MAX_NPCS_PER_AREA`]; absent from the JSON when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        npcs: Vec<NpcSnapshot>,
    },
    /// The server moves this player to another area (see the module docs, "Area travel"). Offers
    /// the target exactly like `AreaManifest` does, plus where the player will stand.
    /// The client must answer `JoinArea` or `CannotJoin`.
    AreaChange {
        area: AreaId,
        display_name: String,
        fingerprint: Fingerprint,
        provenance: ProvenanceSummary,
        fixture: bool,
        /// Where the player will stand in the new area (already on its walkable surface).
        arrival: Pos,
        /// The heading it will have there, radians in adapter space.
        arrival_facing: f32,
    },
    EntityUpdate { tick: u64, entities: Vec<EntitySnapshot> },
    MoveRejected { seq: u32, corrected: Pos },
    PlayerJoined { snapshot: EntitySnapshot },
    PlayerLeft { entity: EntityId },
    Pong { nonce: u64, tick: u64 },
    /// Which instance (area + story phase) this player is in and whether story events may
    /// advance them there. Sent on join and again whenever party changes alter it.
    /// `story_advance_allowed` is informational (nothing story-related runs in free roam).
    InstancePlacement { placement: Placement },
    /// One party state change, replayable (see the module docs). `names` says who the accounts
    /// in `event` are.
    PartyEvent { seq: u64, event: PartyEvent, names: Vec<PartyName> },
    /// The requester's whole party situation, in answer to `PartySync`: its party (if any) and
    /// every pending invite to or from it.
    PartyState { seq: u64, party: Option<Party>, invites: Vec<Invite>, names: Vec<PartyName> },
    /// A party request of this client was refused.
    PartyRejected { error: PartyFailure },
    /// A battle starts, for every member of `party` at once. Rebuild it from your OWN game files:
    /// `party[i]` fights as `ActorId(i)` and the formation's enemies follow, `seed` seeds the
    /// engine, `preemptive` / `back_attack` choose the gauge start (never both), `battle_speed` and
    /// `ticks_per_second` set the engine's pace, and `data_fingerprint` is the fingerprint of the
    /// server's battle data (a client whose own differs cannot replay this battle). Until
    /// `BattleEnd` the receiving player is frozen in the field.
    EncounterStart {
        battle_id: BattleId,
        /// `scene * 4 + slot` in `scene.bin`.
        formation_id: u16,
        seed: u64,
        party: Vec<BattleMember>,
        preemptive: bool,
        back_attack: bool,
        battle_speed: u8,
        ticks_per_second: u32,
        data_fingerprint: Fingerprint,
    },
    /// One operation on the battle engine and what it produced; `tick` is the engine's tick counter
    /// after it. With `command` the engine first advanced silently to `tick` and then applied that
    /// party command; without it the engine advanced to `tick`. See [`battle`] ("The input log").
    /// `command` is left out of the JSON when absent.
    BattleEvent {
        battle_id: BattleId,
        tick: u64,
        events: Vec<battle::BattleEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<AppliedCommand>,
    },
    /// The battle is over (the message before it carried `Victory` or `Defeat`). `rewards` has one
    /// entry per member of the party when the outcome is `Victory` (see [`split_rewards`]) and is
    /// empty on `Defeat`. The receiving player is back in the field.
    BattleEnd { battle_id: BattleId, outcome: battle::Outcome, rewards: Vec<BattleReward> },
    /// A battle request or command of this client was refused. `battle_id` is the battle it was
    /// about, if any.
    BattleRejected { battle_id: Option<BattleId>, reason: BattleRejection },
}

impl ServerMsg {
    /// A `Reject` with a code and its sentence.
    pub fn reject(code: RejectCode, reason: impl Into<String>) -> Self {
        ServerMsg::Reject { reason: reason.into(), code }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(u32),
    #[error("codec: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("connection closed")]
    Closed,
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = serde_json::to_vec(msg)?;
    let len = u32::try_from(payload.len()).map_err(|_| ProtoError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(ProtoError::FrameTooLarge(len));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtoError> {
    Ok(serde_json::from_slice(payload)?)
}

/// The `protocol_version` a `Hello` of ANY protocol version carries, read without decoding the
/// rest of the message (whose shape changed between versions). Lets a server answer an old or
/// new client with "protocol version mismatch" instead of "malformed message". `None` if the
/// payload is not a `Hello` with a version.
pub fn probe_hello_version(payload: &[u8]) -> Option<u16> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "Hello")]
        hello: ProbeHello,
    }
    #[derive(Deserialize)]
    struct ProbeHello {
        protocol_version: u16,
    }
    serde_json::from_slice::<Probe>(payload).ok().map(|p| p.hello.protocol_version)
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<(), ProtoError> {
    let buf = encode(msg)?;
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame's payload (without decoding it). `Err(ProtoError::Closed)` on clean EOF at a
/// frame boundary.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, ProtoError> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(ProtoError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(ProtoError::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Read one framed message. Returns `Err(ProtoError::Closed)` on clean EOF at a frame boundary.
pub async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<T, ProtoError> {
    decode(&read_frame(r).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{GameId, PhaseKey, Platform};

    #[tokio::test]
    async fn roundtrip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let m = ClientMsg::MoveIntent { seq: 3, pos: Pos::new(1.0, 2.0, 0.0), facing: 0.5, anim: AnimState::Walk };
        write_msg(&mut a, &m).await.unwrap();
        let got: ClientMsg = read_msg(&mut b).await.unwrap();
        assert_eq!(got, m);
        drop(a);
        assert!(matches!(read_msg::<_, ClientMsg>(&mut b).await, Err(ProtoError::Closed)));
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME + 1).to_le_bytes()).await.unwrap();
        assert!(matches!(read_msg::<_, ClientMsg>(&mut b).await, Err(ProtoError::FrameTooLarge(_))));
    }

    #[tokio::test]
    async fn frames_can_be_read_raw_and_decoded_later() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_msg(&mut a, &ClientMsg::Leave).await.unwrap();
        let frame = read_frame(&mut b).await.unwrap();
        assert_eq!(frame, br#""Leave""#);
        assert_eq!(decode::<ClientMsg>(&frame).unwrap(), ClientMsg::Leave);
    }

    #[test]
    fn the_version_is_three() {
        assert_eq!(PROTOCOL_VERSION, 3);
    }

    // -----------------------------------------------------------------------------------------
    // Secrets never reach a log line
    // -----------------------------------------------------------------------------------------

    #[test]
    fn tokens_serialize_plainly_but_never_debug_print() {
        let hello = ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "t".into(),
            auth: Auth::Token(Secret::new("fmvt_supersecret")),
            tint: [1, 2, 3],
            appearance_ref: None,
        };
        let json = String::from_utf8(encode(&hello).unwrap()[4..].to_vec()).unwrap();
        assert!(json.contains(r#""Token":"fmvt_supersecret""#), "{json}");
        assert!(!format!("{hello:?}").contains("supersecret"), "{hello:?}");

        let welcome = ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "b".into(),
            session: SessionId(1),
            entity: EntityId(2),
            account: AccountId(3),
            issued_token: Some(Secret::new("fmvt_issuedsecret")),
        };
        let json = String::from_utf8(encode(&welcome).unwrap()[4..].to_vec()).unwrap();
        assert!(json.contains(r#""issued_token":"fmvt_issuedsecret""#), "{json}");
        assert!(!format!("{welcome:?}").contains("issuedsecret"), "{welcome:?}");
        assert_eq!(decode::<ServerMsg>(json.as_bytes()).unwrap(), welcome);
        assert_eq!(Secret::new("x").expose(), "x");
        assert_eq!(Secret::from("y".to_string()).expose(), "y");
    }

    // -----------------------------------------------------------------------------------------
    // Golden JSON: the wire shape of the messages and of fmv-party's embedded types
    // -----------------------------------------------------------------------------------------

    fn json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap()
    }

    /// Serialize, compare with the pinned text, parse back and compare with the original.
    fn golden<T: Serialize + DeserializeOwned + PartialEq + fmt::Debug>(value: T, expected: &str) {
        assert_eq!(json(&value), expected, "wire format changed");
        assert_eq!(serde_json::from_str::<T>(expected).unwrap(), value);
    }

    #[test]
    fn golden_client_messages() {
        golden(
            ClientMsg::Hello {
                protocol_version: 3,
                client_build: "c/1".into(),
                auth: Auth::Register { display_name: "Cloud".into() },
                tint: [10, 20, 30],
                appearance_ref: Some("cloud_model".into()),
            },
            r#"{"Hello":{"protocol_version":3,"client_build":"c/1","auth":{"Register":{"display_name":"Cloud"}},"tint":[10,20,30],"appearance_ref":"cloud_model"}}"#,
        );
        golden(
            ClientMsg::JoinArea {
                area: AreaId { game: GameId::Ff7, key: "md1stin".into() },
                fingerprint: Fingerprint([0xab; 32]),
                provenance: ProvenanceSummary {
                    game: GameId::Ff7,
                    platform: Platform::Pc,
                    edition: "Steam".into(),
                    region: "unknown".into(),
                    language: "en".into(),
                    revision: "r1".into(),
                    modded: false,
                },
            },
            concat!(
                r#"{"JoinArea":{"area":{"game":"Ff7","key":"md1stin"},"fingerprint":""#,
                "abababababababababababababababababababababababababababababababab",
                r#"","provenance":{"game":"Ff7","platform":"Pc","edition":"Steam","region":"unknown","language":"en","revision":"r1","modded":false}}}"#
            ),
        );
        golden(ClientMsg::PartyInvite { target: EntityId(7) }, r#"{"PartyInvite":{"target":7}}"#);
        golden(ClientMsg::PartyAccept { invite: InviteId(3) }, r#"{"PartyAccept":{"invite":3}}"#);
        golden(ClientMsg::PartyDecline { invite: InviteId(3) }, r#"{"PartyDecline":{"invite":3}}"#);
        golden(ClientMsg::PartyKick { target: EntityId(9) }, r#"{"PartyKick":{"target":9}}"#);
        golden(ClientMsg::PartyPromote { target: EntityId(9) }, r#"{"PartyPromote":{"target":9}}"#);
        golden(ClientMsg::PartyLeave, r#""PartyLeave""#);
        golden(ClientMsg::PartySync, r#""PartySync""#);
        golden(
            ClientMsg::Hello {
                protocol_version: 3,
                client_build: "c/1".into(),
                auth: Auth::Token(Secret::new("fmvt_x")),
                tint: [0, 0, 0],
                appearance_ref: None,
            },
            r#"{"Hello":{"protocol_version":3,"client_build":"c/1","auth":{"Token":"fmvt_x"},"tint":[0,0,0],"appearance_ref":null}}"#,
        );
    }

    #[test]
    fn golden_server_messages() {
        golden(
            ServerMsg::Welcome {
                protocol_version: 3,
                server_build: "s/1".into(),
                session: SessionId(4),
                entity: EntityId(5),
                account: AccountId(6),
                issued_token: None,
            },
            r#"{"Welcome":{"protocol_version":3,"server_build":"s/1","session":4,"entity":5,"account":6,"issued_token":null}}"#,
        );
        golden(
            ServerMsg::PartyRejected { error: PartyFailure::Rules(PartyError::PartyFull { party: PartyId(2), max_size: 3 }) },
            r#"{"PartyRejected":{"error":{"Rules":{"PartyFull":{"party":2,"max_size":3}}}}}"#,
        );
        golden(ServerMsg::PartyRejected { error: PartyFailure::UnknownPlayer }, r#"{"PartyRejected":{"error":"UnknownPlayer"}}"#);
        golden(ServerMsg::PartyRejected { error: PartyFailure::RateLimited }, r#"{"PartyRejected":{"error":"RateLimited"}}"#);
        golden(
            ServerMsg::PartyEvent {
                seq: 1,
                event: PartyEvent::InviteSent { invite: InviteId(1), from: AccountId(1), to: AccountId(2), expires_at: 60_000 },
                names: vec![
                    PartyName { account: AccountId(1), name: "Cloud".into(), entity: Some(EntityId(10)) },
                    PartyName { account: AccountId(2), name: "Tifa".into(), entity: None },
                ],
            },
            concat!(
                r#"{"PartyEvent":{"seq":1,"event":{"InviteSent":{"invite":1,"from":1,"to":2,"expires_at":60000}},"#,
                r#""names":[{"account":1,"name":"Cloud","entity":10},{"account":2,"name":"Tifa","entity":null}]}}"#
            ),
        );
        golden(
            ServerMsg::PartyState {
                seq: 4,
                party: Some(Party { id: PartyId(1), leader: AccountId(1), members: vec![AccountId(1), AccountId(2)] }),
                invites: vec![Invite { id: InviteId(2), from: AccountId(1), to: AccountId(3), created_at: 5, expires_at: 65 }],
                names: vec![],
            },
            concat!(
                r#"{"PartyState":{"seq":4,"party":{"id":1,"leader":1,"members":[1,2]},"#,
                r#""invites":[{"id":2,"from":1,"to":3,"created_at":5,"expires_at":65}],"names":[]}}"#
            ),
        );
        golden(
            ServerMsg::InstancePlacement {
                placement: Placement {
                    account: AccountId(2),
                    key: InstanceKey::new(AreaId { game: GameId::Ff7, key: "md1stin".into() }, PhaseKey(vec![])),
                    party: Some(PartyId(1)),
                    phase_owner: AccountId(1),
                    story_advance_allowed: true,
                },
            },
            concat!(
                r#"{"InstancePlacement":{"placement":{"account":2,"key":{"area":{"game":"Ff7","key":"md1stin"},"phase":[]},"#,
                r#""party":1,"phase_owner":1,"story_advance_allowed":true}}}"#
            ),
        );
    }

    // -----------------------------------------------------------------------------------------
    // Area travel (protocol v4)
    // -----------------------------------------------------------------------------------------

    fn md1stin() -> AreaId {
        AreaId { game: GameId::Ff7, key: "md1stin".into() }
    }

    fn npc(id: u32) -> NpcSnapshot {
        NpcSnapshot {
            id,
            name: "gu0".into(),
            model: "aaaa".into(),
            idle_animation: Some("acfe".into()),
            pos: Pos::new(3700.0, 29000.0, 353.0),
            facing: 0.5,
        }
    }

    #[test]
    fn golden_area_travel_messages() {
        let provenance = ProvenanceSummary {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: "Steam".into(),
            region: "unknown".into(),
            language: "en".into(),
            revision: "r1".into(),
            modded: false,
        };
        golden(
            ServerMsg::AreaChange {
                area: AreaId { game: GameId::Ff7, key: "md1_1".into() },
                display_name: "FF7 field md1_1".into(),
                fingerprint: Fingerprint([0xab; 32]),
                provenance,
                fixture: false,
                arrival: Pos::new(1049.0, 400.0, 90.0),
                arrival_facing: 1.5,
            },
            concat!(
                r#"{"AreaChange":{"area":{"game":"Ff7","key":"md1_1"},"display_name":"FF7 field md1_1","fingerprint":""#,
                "abababababababababababababababababababababababababababababababab",
                r#"","provenance":{"game":"Ff7","platform":"Pc","edition":"Steam","region":"unknown","language":"en","revision":"r1","modded":false},"#,
                r#""fixture":false,"arrival":{"x":1049.0,"y":400.0,"z":90.0},"arrival_facing":1.5}}"#
            ),
        );
        golden(
            ServerMsg::AreaSnapshot { area: md1stin(), tick: 7, entities: vec![], npcs: vec![npc(0), npc(1)] },
            concat!(
                r#"{"AreaSnapshot":{"area":{"game":"Ff7","key":"md1stin"},"tick":7,"entities":[],"npcs":["#,
                r#"{"id":0,"name":"gu0","model":"aaaa","idle_animation":"acfe","pos":{"x":3700.0,"y":29000.0,"z":353.0},"facing":0.5},"#,
                r#"{"id":1,"name":"gu0","model":"aaaa","idle_animation":"acfe","pos":{"x":3700.0,"y":29000.0,"z":353.0},"facing":0.5}]}}"#
            ),
        );
        // An NPC without a known idle clip leaves the key out.
        let mut bare = npc(2);
        bare.idle_animation = None;
        golden(
            bare,
            r#"{"id":2,"name":"gu0","model":"aaaa","pos":{"x":3700.0,"y":29000.0,"z":353.0},"facing":0.5}"#,
        );
    }

    /// A snapshot of an area without NPCs is exactly what it was before NPCs existed, and one that
    /// an older server sent (no `npcs` key) still reads, as an area without any.
    #[test]
    fn a_snapshot_without_npcs_keeps_its_old_wire_shape() {
        let old = r#"{"AreaSnapshot":{"area":{"game":"Ff7","key":"md1stin"},"tick":3,"entities":[]}}"#;
        golden(ServerMsg::AreaSnapshot { area: md1stin(), tick: 3, entities: vec![], npcs: vec![] }, old);
        let spelled: ServerMsg = decode(br#"{"AreaSnapshot":{"area":{"game":"Ff7","key":"md1stin"},"tick":3,"entities":[],"npcs":[]}}"#).unwrap();
        assert!(matches!(spelled, ServerMsg::AreaSnapshot { npcs, .. } if npcs.is_empty()));
    }

    /// The NPC list is bounded so that a full server's snapshot still fits one frame: the budget
    /// documented on [`MAX_NPCS_PER_AREA`] holds for the worst-case NPC.
    #[test]
    fn the_worst_case_npc_list_fits_its_budget() {
        let worst = NpcSnapshot {
            id: u32::MAX,
            name: "\u{10FFFF}".repeat(NPC_NAME_CHARS),
            model: "\u{10FFFF}".repeat(NPC_REF_CHARS),
            idle_animation: Some("\u{10FFFF}".repeat(NPC_REF_CHARS)),
            pos: Pos::new(f32::MIN, f32::MIN, f32::MIN),
            facing: f32::MIN,
        };
        let one = serde_json::to_vec(&worst).unwrap().len();
        // `\u{10FFFF}` is written as a raw 4-byte character; serde_json does not escape it.
        assert!(one < 400, "{one} bytes per NPC");
        assert!(MAX_NPCS_PER_AREA * (one + 1) < 51_000, "{} bytes of NPCs", MAX_NPCS_PER_AREA * (one + 1));
    }

    // -----------------------------------------------------------------------------------------
    // Rejection codes (additive within v3)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn golden_rejections() {
        golden(
            ServerMsg::reject(RejectCode::NameTaken, "that display name is already taken"),
            r#"{"Reject":{"reason":"that display name is already taken","code":"NameTaken"}}"#,
        );
        // A reject without a code has exactly the shape it always had.
        golden(
            ServerMsg::Reject { reason: "server full".into(), code: RejectCode::Unspecified },
            r#"{"Reject":{"reason":"server full"}}"#,
        );
        // Every code keeps its wire name (clients match on them).
        let names = [
            (RejectCode::ProtocolMismatch, "ProtocolMismatch"),
            (RejectCode::ServerFull, "ServerFull"),
            (RejectCode::TooManyConnections, "TooManyConnections"),
            (RejectCode::RegistrationClosed, "RegistrationClosed"),
            (RejectCode::RateLimited, "RateLimited"),
            (RejectCode::NameTaken, "NameTaken"),
            (RejectCode::InvalidName, "InvalidName"),
            (RejectCode::InvalidToken, "InvalidToken"),
            (RejectCode::AlreadyOnline, "AlreadyOnline"),
            (RejectCode::ContentMismatch, "ContentMismatch"),
            (RejectCode::BadRequest, "BadRequest"),
            (RejectCode::Timeout, "Timeout"),
            (RejectCode::IdleTimeout, "IdleTimeout"),
            (RejectCode::ShuttingDown, "ShuttingDown"),
            (RejectCode::Internal, "Internal"),
        ];
        for (code, name) in names {
            golden(code, &format!("\"{name}\""));
        }
        assert_eq!(names.len() + 1, RejectCode::ALL.len(), "a new code needs a golden line here");
        assert!(RejectCode::ALL.contains(&RejectCode::Unspecified));
    }

    /// The compatibility argument of `reject.rs`, as tests: old and new peers read each other.
    #[test]
    fn a_reject_with_or_without_a_code_is_read_by_old_and_new_peers() {
        // A server that predates the codes: the new client reads `Unspecified`.
        let text = "too many registrations on this server right now; try again in a minute";
        let old: ServerMsg = decode(format!(r#"{{"Reject":{{"reason":"{text}"}}}}"#).as_bytes()).unwrap();
        assert_eq!(old, ServerMsg::Reject { reason: text.into(), code: RejectCode::Unspecified });
        // A newer server's code this build has never heard of is `Unspecified`, not "malformed".
        let future: ServerMsg = decode(br#"{"Reject":{"reason":"x","code":"SomethingNew"}}"#).unwrap();
        assert_eq!(future, ServerMsg::Reject { reason: "x".into(), code: RejectCode::Unspecified });
        // An old client (the v3 shape without `code`) reads the new message by ignoring the field.
        #[derive(Deserialize, Debug, PartialEq)]
        enum OldServerMsg {
            Reject { reason: String },
        }
        let new_json = serde_json::to_vec(&ServerMsg::reject(RejectCode::ServerFull, "server full")).unwrap();
        assert_eq!(decode::<OldServerMsg>(&new_json).unwrap(), OldServerMsg::Reject { reason: "server full".into() });
    }

    #[test]
    fn retryable_codes_are_about_the_servers_state_not_the_request() {
        for code in RejectCode::ALL {
            let retry = code.is_retryable();
            match code {
                RejectCode::ServerFull
                | RejectCode::TooManyConnections
                | RejectCode::RateLimited
                | RejectCode::AlreadyOnline
                | RejectCode::Timeout
                | RejectCode::IdleTimeout
                | RejectCode::ShuttingDown
                | RejectCode::Internal => assert!(retry, "{code:?}"),
                RejectCode::Unspecified
                | RejectCode::ProtocolMismatch
                | RejectCode::RegistrationClosed
                | RejectCode::NameTaken
                | RejectCode::InvalidName
                | RejectCode::InvalidToken
                | RejectCode::ContentMismatch
                | RejectCode::BadRequest => assert!(!retry, "{code:?}"),
            }
        }
    }

    /// Every `PartyEvent` variant keeps its shape (the wire embeds fmv-party's type).
    #[test]
    fn golden_party_events() {
        let (a, b, c) = (AccountId(1), AccountId(2), AccountId(3));
        let cases: Vec<(PartyEvent, &str)> = vec![
            (PartyEvent::InviteDeclined { invite: InviteId(1), from: a, to: b }, r#"{"InviteDeclined":{"invite":1,"from":1,"to":2}}"#),
            (PartyEvent::InviteExpired { invite: InviteId(1), from: a, to: b }, r#"{"InviteExpired":{"invite":1,"from":1,"to":2}}"#),
            (
                PartyEvent::InviteCancelled { invite: InviteId(1), from: a, to: b, reason: InviteCancelReason::InviterLeftParty },
                r#"{"InviteCancelled":{"invite":1,"from":1,"to":2,"reason":"InviterLeftParty"}}"#,
            ),
            (
                PartyEvent::PartyFormed { party: PartyId(1), invite: InviteId(1), leader: a, members: vec![a, b] },
                r#"{"PartyFormed":{"party":1,"invite":1,"leader":1,"members":[1,2]}}"#,
            ),
            (
                PartyEvent::MemberJoined { party: PartyId(1), invite: InviteId(2), account: c, leader: a, members: vec![a, b, c] },
                r#"{"MemberJoined":{"party":1,"invite":2,"account":3,"leader":1,"members":[1,2,3]}}"#,
            ),
            (
                PartyEvent::MemberLeft { party: PartyId(1), account: b, reason: LeaveReason::Kicked, members: vec![a, c] },
                r#"{"MemberLeft":{"party":1,"account":2,"reason":"Kicked","members":[1,3]}}"#,
            ),
            (
                PartyEvent::LeaderChanged {
                    party: PartyId(1),
                    old_leader: a,
                    new_leader: b,
                    reason: LeaderChangeReason::Promoted,
                    members: vec![a, b],
                },
                r#"{"LeaderChanged":{"party":1,"old_leader":1,"new_leader":2,"reason":"Promoted","members":[1,2]}}"#,
            ),
            (
                PartyEvent::Disbanded { party: PartyId(1), members: vec![a], reason: DisbandReason::LastMemberLeft },
                r#"{"Disbanded":{"party":1,"members":[1],"reason":"LastMemberLeft"}}"#,
            ),
        ];
        for (event, expected) in cases {
            golden(event, expected);
        }
    }

    /// Every `PartyError` variant keeps its shape (a refused request names the exact reason).
    #[test]
    fn golden_party_errors() {
        let (a, b) = (AccountId(1), AccountId(2));
        let cases: Vec<(PartyError, &str)> = vec![
            (PartyError::SelfInvite, r#""SelfInvite""#),
            (PartyError::AlreadyInParty { account: a }, r#"{"AlreadyInParty":{"account":1}}"#),
            (PartyError::NotLeader { account: a }, r#"{"NotLeader":{"account":1}}"#),
            (PartyError::NotInParty { account: a }, r#"{"NotInParty":{"account":1}}"#),
            (PartyError::TargetNotInParty { account: a }, r#"{"TargetNotInParty":{"account":1}}"#),
            (PartyError::DuplicateInvite { from: a, to: b }, r#"{"DuplicateInvite":{"from":1,"to":2}}"#),
            (PartyError::PartyFull { party: PartyId(1), max_size: 3 }, r#"{"PartyFull":{"party":1,"max_size":3}}"#),
            (PartyError::UnknownInvite { invite: InviteId(9) }, r#"{"UnknownInvite":{"invite":9}}"#),
            (PartyError::InviteExpired { invite: InviteId(9) }, r#"{"InviteExpired":{"invite":9}}"#),
            (PartyError::NotInvitee { invite: InviteId(9), account: a }, r#"{"NotInvitee":{"invite":9,"account":1}}"#),
            (PartyError::InviterCannotInvite { invite: InviteId(9) }, r#"{"InviterCannotInvite":{"invite":9}}"#),
            (PartyError::CannotTargetSelf { account: a }, r#"{"CannotTargetSelf":{"account":1}}"#),
            (PartyError::AlreadyLeader { account: a }, r#"{"AlreadyLeader":{"account":1}}"#),
        ];
        for (error, expected) in cases {
            golden(error, expected);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Battle (v3): every new message, and the engine types that travel inside them
    // -----------------------------------------------------------------------------------------

    fn members() -> Vec<BattleMember> {
        vec![
            BattleMember { entity: EntityId(10), account: AccountId(20), character: 0, level: 6, weapon: None, armor: None },
            BattleMember { entity: EntityId(11), account: AccountId(21), character: 1, level: 7, weapon: Some(3), armor: Some(4) },
        ]
    }

    #[test]
    fn golden_battle_client_messages() {
        golden(
            ClientMsg::BattleCommand {
                battle_id: BattleId(1),
                actor: ActorId(0),
                command: battle::Command::Attack { target: ActorId(2) },
            },
            r#"{"BattleCommand":{"battle_id":1,"actor":0,"command":{"Attack":{"target":2}}}}"#,
        );
        golden(ClientMsg::RequestEncounter { formation_id: None }, r#"{"RequestEncounter":{"formation_id":null}}"#);
        golden(ClientMsg::RequestEncounter { formation_id: Some(517) }, r#"{"RequestEncounter":{"formation_id":517}}"#);
    }

    #[test]
    fn golden_battle_server_messages() {
        golden(
            ServerMsg::EncounterStart {
                battle_id: BattleId(1),
                formation_id: 517,
                seed: 12_345,
                party: members(),
                preemptive: false,
                back_attack: true,
                battle_speed: 128,
                ticks_per_second: 60,
                data_fingerprint: Fingerprint([0xcd; 32]),
            },
            concat!(
                r#"{"EncounterStart":{"battle_id":1,"formation_id":517,"seed":12345,"party":["#,
                r#"{"entity":10,"account":20,"character":0,"level":6,"weapon":null,"armor":null},"#,
                r#"{"entity":11,"account":21,"character":1,"level":7,"weapon":3,"armor":4}],"#,
                r#""preemptive":false,"back_attack":true,"battle_speed":128,"ticks_per_second":60,"data_fingerprint":""#,
                "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                r#""}}"#
            ),
        );
        // An engine step: no command, and the JSON has no `command` key at all.
        golden(
            ServerMsg::BattleEvent {
                battle_id: BattleId(1),
                tick: 120,
                events: vec![battle::BattleEvent::GaugeReady { actor: ActorId(0) }],
                command: None,
            },
            r#"{"BattleEvent":{"battle_id":1,"tick":120,"events":[{"GaugeReady":{"actor":0}}]}}"#,
        );
        // A party command and what it did.
        golden(
            ServerMsg::BattleEvent {
                battle_id: BattleId(1),
                tick: 130,
                events: vec![
                    battle::BattleEvent::Damage { actor: ActorId(0), target: ActorId(2), amount: 42, critical: false },
                    battle::BattleEvent::Death { actor: ActorId(2) },
                    battle::BattleEvent::Victory,
                ],
                command: Some(AppliedCommand { actor: ActorId(0), command: battle::Command::Attack { target: ActorId(2) } }),
            },
            concat!(
                r#"{"BattleEvent":{"battle_id":1,"tick":130,"events":["#,
                r#"{"Damage":{"actor":0,"target":2,"amount":42,"critical":false}},{"Death":{"actor":2}},"Victory"],"#,
                r#""command":{"actor":0,"command":{"Attack":{"target":2}}}}}"#
            ),
        );
        // An absent `command` may also be spelled out (`null`) by a sender; it reads as absent.
        let spelled: ServerMsg =
            decode(br#"{"BattleEvent":{"battle_id":1,"tick":120,"events":[],"command":null}}"#).expect("null command");
        assert_eq!(spelled, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 120, events: vec![], command: None });
        golden(
            ServerMsg::BattleEnd {
                battle_id: BattleId(1),
                outcome: battle::Outcome::Victory,
                rewards: vec![
                    BattleReward { account: AccountId(20), exp: 6, gil: 10, ap: 2 },
                    BattleReward { account: AccountId(21), exp: 0, gil: 0, ap: 0 },
                ],
            },
            concat!(
                r#"{"BattleEnd":{"battle_id":1,"outcome":"Victory","rewards":["#,
                r#"{"account":20,"exp":6,"gil":10,"ap":2},{"account":21,"exp":0,"gil":0,"ap":0}]}}"#
            ),
        );
        golden(
            ServerMsg::BattleEnd { battle_id: BattleId(2), outcome: battle::Outcome::Defeat, rewards: vec![] },
            r#"{"BattleEnd":{"battle_id":2,"outcome":"Defeat","rewards":[]}}"#,
        );
        golden(
            ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::GaugeNotFull },
            r#"{"BattleRejected":{"battle_id":1,"reason":"GaugeNotFull"}}"#,
        );
        golden(
            ServerMsg::BattleRejected { battle_id: None, reason: BattleRejection::Unsupported },
            r#"{"BattleRejected":{"battle_id":null,"reason":"Unsupported"}}"#,
        );
    }

    /// The engine's own events, commands and outcomes are embedded in the messages, so their shape is
    /// part of the wire: a change in `fmv-ff7-battle` fails here instead of changing the protocol.
    #[test]
    fn golden_engine_types_on_the_wire() {
        let (a, t) = (ActorId(0), ActorId(3));
        let events: Vec<(battle::BattleEvent, &str)> = vec![
            (battle::BattleEvent::GaugeReady { actor: a }, r#"{"GaugeReady":{"actor":0}}"#),
            (
                battle::BattleEvent::Damage { actor: a, target: t, amount: 1234, critical: true },
                r#"{"Damage":{"actor":0,"target":3,"amount":1234,"critical":true}}"#,
            ),
            (battle::BattleEvent::Miss { actor: t, target: a }, r#"{"Miss":{"actor":3,"target":0}}"#),
            (battle::BattleEvent::Death { actor: t }, r#"{"Death":{"actor":3}}"#),
            (battle::BattleEvent::Victory, r#""Victory""#),
            (battle::BattleEvent::Defeat, r#""Defeat""#),
        ];
        for (event, expected) in events {
            golden(event, expected);
        }
        golden(battle::Command::Attack { target: t }, r#"{"Attack":{"target":3}}"#);
        golden(battle::Outcome::Victory, r#""Victory""#);
        golden(battle::Outcome::Defeat, r#""Defeat""#);
        golden(ActorId(7), "7");
        golden(BattleId(9), "9");
    }

    /// Every refusal keeps its name (clients show them and bots match on them).
    #[test]
    fn golden_battle_rejections() {
        let cases = [
            (BattleRejection::Unsupported, r#""Unsupported""#),
            (BattleRejection::DebugDisabled, r#""DebugDisabled""#),
            (BattleRejection::AlreadyInBattle, r#""AlreadyInBattle""#),
            (BattleRejection::UnknownFormation, r#""UnknownFormation""#),
            (BattleRejection::NotInBattle, r#""NotInBattle""#),
            (BattleRejection::NotYourActor, r#""NotYourActor""#),
            (BattleRejection::GaugeNotFull, r#""GaugeNotFull""#),
            (BattleRejection::ActorDead, r#""ActorDead""#),
            (BattleRejection::InvalidTarget, r#""InvalidTarget""#),
            (BattleRejection::TargetDead, r#""TargetDead""#),
            (BattleRejection::BattleOver, r#""BattleOver""#),
        ];
        for (reason, expected) in cases {
            golden(reason, expected);
        }
    }

    /// No battle message can carry anything but ids, seeds, numbers and the engine's events: the
    /// fields of the largest one are all fixed-size numbers, flags, ids and a fingerprint.
    #[test]
    fn a_battle_start_is_small_and_carries_no_game_data() {
        let msg = ServerMsg::EncounterStart {
            battle_id: BattleId(u64::MAX),
            formation_id: u16::MAX,
            seed: u64::MAX,
            party: (0..MAX_BATTLE_PARTY as u64)
                .map(|i| BattleMember {
                    entity: EntityId(u64::MAX - i),
                    account: AccountId(u64::MAX - i),
                    character: u8::MAX,
                    level: u8::MAX,
                    weapon: Some(u8::MAX),
                    armor: Some(u8::MAX),
                })
                .collect(),
            preemptive: false,
            back_attack: false,
            battle_speed: u8::MAX,
            ticks_per_second: u32::MAX,
            data_fingerprint: Fingerprint([0xff; 32]),
        };
        let frame = encode(&msg).unwrap();
        assert!(frame.len() < 2_048, "{} bytes", frame.len());
        // A hundred events in one message still fits a frame with room to spare.
        let busy = ServerMsg::BattleEvent {
            battle_id: BattleId(u64::MAX),
            tick: u64::MAX,
            events: (0..100)
                .map(|i| battle::BattleEvent::Damage { actor: ActorId(i), target: ActorId(255), amount: u32::MAX, critical: true })
                .collect(),
            command: Some(AppliedCommand { actor: ActorId(255), command: battle::Command::Attack { target: ActorId(255) } }),
        };
        assert!(encode(&busy).unwrap().len() < MAX_FRAME as usize / 8);
    }

    #[test]
    fn a_v3_hello_reads_as_a_v3_hello_and_a_v2_one_is_still_readable_as_a_version() {
        let v3 = br#"{"Hello":{"protocol_version":3,"client_build":"x","auth":{"Register":{"display_name":"n"}},"tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v3).is_ok());
        assert_eq!(probe_hello_version(v3), Some(3));
    }

    #[test]
    fn a_hellos_version_can_be_read_whatever_else_it_contains() {
        // A v1 Hello (display_name at the top level) is not a v2 `ClientMsg`, but its version is readable.
        let v1 = br#"{"Hello":{"protocol_version":1,"client_build":"x","display_name":"Old","tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v1).is_err());
        assert_eq!(probe_hello_version(v1), Some(1));
        let v2 = br#"{"Hello":{"protocol_version":2,"client_build":"x","auth":{"Register":{"display_name":"n"}},"tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v2).is_ok());
        assert_eq!(probe_hello_version(v2), Some(2));
        let future = br#"{"Hello":{"protocol_version":9,"anything":{"at":"all"}}}"#;
        assert_eq!(probe_hello_version(future), Some(9));
        for not_hello in [&br#""Leave""#[..], br#"{"Ping":{"nonce":1}}"#, b"not json", b"{}", br#"{"Hello":{}}"#, br#"{"Hello":{"protocol_version":70000}}"#] {
            assert_eq!(probe_hello_version(not_hello), None, "{}", String::from_utf8_lossy(not_hello));
        }
    }
}
