//! FF Multiverse wire protocol v2.
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
//! S -> C  Welcome { .., account, issued_token }      (or Reject, then the server closes)
//! S -> C  AreaManifest { area, fingerprint, provenance, fixture, .. }
//! C -> S  JoinArea { area, fingerprint, provenance }  (or CannotJoin { reason })
//! S -> C  AreaSnapshot, InstancePlacement             (always first after the handshake replies)
//! ...     MoveIntent / EntityUpdate / MoveRejected / Ping / Pong / Party* ...
//! C -> S  Leave
//! ```
//!
//! # Accounts
//!
//! `Hello::auth` is [`Auth::Register`] (create an account with that display name) or
//! [`Auth::Token`] (log in with the token a registration issued). A registration is answered by a
//! `Welcome` whose `issued_token` is `Some`: **the only time the token is ever sent**; the server
//! keeps just a hash. A client must store it before doing anything else (a registered name is
//! unique and cannot be recovered without the token).
//!
//! After registration the server never trusts client-supplied names: the display name, tint and
//! per-game appearance of an entity come from the stored avatar profile. `Hello::tint` and
//! `Hello::appearance_ref` are only the look requested for a NEW avatar (ignored when logging in
//! with a token).
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

/// The party types carried on the wire, re-exported so a client needs no direct `fmv-party`
/// dependency to match on them.
pub use fmv_party::{
    DisbandReason, InstanceKey, Invite, InviteCancelReason, InviteId, LeaderChangeReason, LeaveReason, Party, PartyError,
    PartyEvent, PartyId, Placement,
};

pub const PROTOCOL_VERSION: u16 = 2;
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
    /// Create an account with this display name (cleaned by `AvatarAppearance::sanitize_name`
    /// and unique, case-insensitively). The reply carries the new token.
    Register { display_name: String },
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
    /// avatar (when `auth` is `Register`).
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
    Reject { reason: String },
    /// Offer of the area the server is hosting. `fixture == true` must be shown to the user.
    AreaManifest {
        area: AreaId,
        display_name: String,
        fingerprint: Fingerprint,
        provenance: ProvenanceSummary,
        fixture: bool,
    },
    AreaSnapshot { area: AreaId, tick: u64, entities: Vec<EntitySnapshot> },
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
    fn the_version_is_two() {
        assert_eq!(PROTOCOL_VERSION, 2);
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
                protocol_version: 2,
                client_build: "c/1".into(),
                auth: Auth::Register { display_name: "Cloud".into() },
                tint: [10, 20, 30],
                appearance_ref: Some("cloud_model".into()),
            },
            r#"{"Hello":{"protocol_version":2,"client_build":"c/1","auth":{"Register":{"display_name":"Cloud"}},"tint":[10,20,30],"appearance_ref":"cloud_model"}}"#,
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
                protocol_version: 2,
                client_build: "c/1".into(),
                auth: Auth::Token(Secret::new("fmvt_x")),
                tint: [0, 0, 0],
                appearance_ref: None,
            },
            r#"{"Hello":{"protocol_version":2,"client_build":"c/1","auth":{"Token":"fmvt_x"},"tint":[0,0,0],"appearance_ref":null}}"#,
        );
    }

    #[test]
    fn golden_server_messages() {
        golden(
            ServerMsg::Welcome {
                protocol_version: 2,
                server_build: "s/1".into(),
                session: SessionId(4),
                entity: EntityId(5),
                account: AccountId(6),
                issued_token: None,
            },
            r#"{"Welcome":{"protocol_version":2,"server_build":"s/1","session":4,"entity":5,"account":6,"issued_token":null}}"#,
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
