//! FF Multiverse wire protocol v9.
//!
//! v9 is v8 plus the battle party's CONTROLLERS (see [`battle`], "The party: canon companions, then the players"):
//! a battle's roster ([`BattleMember`]) is now the story's canon party first, commanded by the server
//! ([`Controller::Companion`]: no account, no entity, never paid), and then every player's own character
//! ([`Controller::Player`]: its entity and account), so a player's original character is ADDED to the party and
//! nobody is put in a canon member's place. The `entity` and `account` of a v8 member moved into the controller, so a
//! v8 member is not a v9 member and the other way round; players also have no limit break
//! ([`PLAYER_LIMIT_BREAKS`]), which every peer's own engine applies when it builds the battle. Nothing else about the
//! messages changed (the commands a companion gives travel as ordinary `BattleEvent`s with the command attached),
//! but a v8 peer would stop at the first roster it cannot read, so the version was bumped: a v8 client or server is
//! refused at the handshake. A stored record is not touched by any of it: the `slot` of a character record is now
//! documented as the character's stat curve ("fighting style"), under the same key and with the same numbers.
//!
//! v8 is v7 plus the player's own original character (see [`character`] and "Characters" below): `Hello` asks
//! for a character to create ([`NewCharacter`]: colour, body, combat slot) instead of a colour and a look,
//! `Welcome` tells the player what the server holds ([`CharacterCard`]) and a battle's [`BattleMember`] carries
//! the player's own name. The meaning of `AvatarAppearance::appearance_ref` changed with it: it is the
//! game's per-player binding, no longer the look that also named the battle character, and `BattleMember::character`
//! is the player's combat slot. A v7 peer cannot decode a `Hello` without `tint`, a `Welcome` with a card or a
//! member with a name, so the version was bumped: a v7 client or server is refused at the handshake
//! (`probe_hello_version` reads the version of any `Hello`, so the refusal is a clean `ProtocolMismatch`).
//!
//! v7 is v6 plus limit breaks and escape: [`battle::Command`] gains `Limit { target }` and `Escape`,
//! [`battle::ActionRef`] gains `Limit(id)` and `Escape`, [`battle::Outcome`] gains `Escaped`, battle
//! events gain `LimitGauge` and `EscapeFailed`, [`BattleMember`] carries the character's limit gauge
//! at the start of the battle (`limit`) and [`BattleRejection`] gains `NoLimit`, `LimitNotReady` and
//! `CannotEscape`. A v6 peer cannot decode any of them (and a v6 `EncounterStart` has no `limit`), so
//! the version was bumped: a v6 client or server is refused at the handshake instead of being cut off
//! at the first limit or escape (see [`battle`], "Limit breaks and escape").
//!
//! v6 is v5 (character progress: [`ServerMsg::CharacterProgress`], see "Character progress") plus real
//! battle commands and an inventory: [`battle::Command`] gains `Magic`, `Item` and `Defend`, battle
//! events gain `Action`, `Heal`, `MpSpent`, `MpRestored` and `Revive`, [`ServerMsg::Inventory`] tells a
//! player what it carries and [`BattleReward`] carries the items a won battle dropped (see "Inventory").
//! v5 was v4 (area travel: [`ServerMsg::AreaChange`] and `AreaSnapshot::npcs`, see "Area travel") plus
//! character progress; v4 was v3 (accounts, battles, rejection codes) plus area travel. A peer cannot
//! decode a message variant it does not know (an unknown message is a decode error), so each of these
//! changes bumped the version: a server answers an older `Hello` with `Reject { code: ProtocolMismatch }`
//! at the handshake, a clean refusal instead of a dropped connection at the first exit a player crosses
//! (v3), the moment a player joins (v4: a v4 client would choke on the progress message that follows
//! every join) or the first spell a player casts (v6: a v5 client cannot decode the new events, and
//! would choke on the inventory that follows every join too).
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
//! C -> S  Hello { protocol_version, client_build, auth, character }
//! S -> C  Welcome { .., account, issued_token, character }   (or Reject { reason, code }, then the server closes)
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
//! S -> C  BattleEvent { .., events: [Action { Escape }], command: Escape }, then BattleEnd { outcome: Escaped }
//! ```
//!
//! A character whose limit gauge is full gives `Limit { target }` instead of Attack; any character
//! may give `Escape`, which ends the battle for the whole party when it succeeds (nothing is paid) and
//! otherwise only costs the turn (`EscapeFailed`); see [`battle`], "Limit breaks and escape".
//!
//! # Area travel (v4)
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
//! way. A player that does not answer within the join timeout is disconnected. A v3 client never
//! gets this far: it does not know `AreaChange`, so the version check at `Hello` refuses it with
//! `RejectCode::ProtocolMismatch` (see the top of this page).
//!
//! `AreaSnapshot::npcs` lists the non-player characters the area's data places ([`NpcSnapshot`]):
//! static, with no `EntityUpdate`s of their own, sent with every snapshot of the area. It is left
//! out of the JSON when empty, so a snapshot of an area without any is byte-for-byte what it was.
//!
//! # Character progress (v5)
//!
//! Each account has its own progression in the game it plays, separate from the story phase: the
//! character it plays (FF7: the one its avatar looks like), that character's level, experience, AP
//! and gil. The SERVER keeps it (and stores it, see `fmv-persist`'s `CharacterProgress`); the
//! client only mirrors what it is told. Three things use it:
//!
//! * `EncounterStart::party` names each member's own `character` and `level` (a client replays
//!   the battle from them, as before: the numbers were always on the wire),
//! * a victory pays each member that fought on (`BattleEnd::rewards`, see [`split_rewards`]): the
//!   server adds it to that member's progress, applying the game's own level-up rule,
//! * [`ServerMsg::CharacterProgress`] tells the player what its progress is now: on joining, and after
//!   every won battle it was a member of (also when it was paid nothing, being dead at the end: then
//!   the numbers are what they were and `leveled_up` is false). A lost battle pays nothing and sends
//!   nothing.
//!
//! ```text
//! S -> C  AreaSnapshot, InstancePlacement, CharacterProgress { .., leveled_up: false }   on join
//! S -> C  BattleEnd { battle_id, outcome: Victory, rewards }
//! S -> C  CharacterProgress { .., leveled_up }       right after, to each member that is still online
//! ```
//!
//! `exp_to_next` is the experience still missing for the next level (0 at the highest level), so
//! the next threshold is `exp + exp_to_next`. A server without battle data (the FIXTURE one) has no
//! characters to level and sends none. Nothing in the message is a game datum: levels, experience,
//! AP and gil are the player's own numbers, and the tables behind them stay in each peer's files.
//!
//! # Inventory (v6)
//!
//! Each account carries items (FF7: the kernel's item table indexes, `kernel.bin` section 4). The SERVER
//! keeps the inventory (and stores it with the character progress, see `fmv-persist`) and is the only
//! authority: a battle `Item` command is accepted only when the account has one, and the item is taken
//! only when the battle engine accepted the command. The engine itself knows no inventory (see
//! [`battle`]), which is what lets every client replay every battle without knowing anyone's items.
//!
//! * [`ServerMsg::Inventory`] tells a player what it carries: on joining (right after
//!   `CharacterProgress`), after every item it used in a battle (right after the `BattleEvent` that
//!   applied it) and after a won battle that dropped it items (after `CharacterProgress`). It is the whole
//!   inventory every time, at most [`MAX_INVENTORY_STACKS`] entries, one per item, no entry of zero.
//! * A new account, and an old one that has never had an inventory, starts with the game's starting stock
//!   (FF7: the kernel's own: Potion x3, Ether x1 and Phoenix Down x1 on the Steam copy), once.
//! * A won battle drops items ([`BattleReward::items`]): rolled by the server from the battle's seed after
//!   the outcome, dealt to the living members, and not part of the replay (a client cannot check them).
//!
//! ```text
//! S -> C  AreaSnapshot, InstancePlacement, CharacterProgress, Inventory { items }     on join
//! C -> S  BattleCommand { battle_id, actor, command: Item { item, target } }
//! S -> C  BattleEvent { .., command: Some(..), events: [Action(Item), Heal ..] }      every member
//! S -> C  Inventory { items }                                                         the user, one fewer
//! S -> C  BattleEnd { .., rewards: [.., items] }, CharacterProgress, Inventory        after a win with drops
//! ```
//!
//! A server without battle data (the FIXTURE one) has no items and sends none.
//!
//! # Story, phases and instances
//!
//! Each account has ONE story progression per game (`fmv-persist`'s `Progression` blob; only the
//! game's adapter reads it, an empty one is a new account). It is the account's, not the player's
//! character's: it does not depend on the look or the combat slot. The adapter turns it into a story
//! PHASE for each area (`AreaRuntime::phase_key`, an opaque key that the empty progression maps to the
//! empty key in every area), and the world keeps one INSTANCE of an area per phase that somebody is
//! in: players see, hear and fight with only the players of their own instance.
//!
//! * Solo, a player is in the instance of its own phase. In a party every member is in the instance
//!   of the LEADER's phase (for the area the member stands in) and keeps its own progression;
//!   [`Placement::story_advance_allowed`] is whether the story may advance for it there (its own phase
//!   is the one that chose the instance).
//! * The server moves a player between two instances of the SAME area when its placement changes (it
//!   advanced the story, a party formed, changed leader or broke up). Everyone left behind gets
//!   `PlayerLeft`, the player gets a fresh `AreaSnapshot` of the instance it is in (the same area, the
//!   entities and NPCs of that instance) and then its `InstancePlacement`, and the instance it enters
//!   gets `PlayerJoined`. A client already replaces its whole picture of an area on a snapshot.
//! * The story advances only by something the server verifies for itself: the player's validated
//!   position reaches a story point of the area (for FF7 the loop of a field script that sets the
//!   main progress counter when a player's character stands on a walkmesh triangle; see
//!   `fmv-ff7`'s `story`). The server logs it as one line,
//!   `story advanced account=<n> entity=<n> area=<AreaId> point=<id> moment=<from>-><to>`, saves it,
//!   and tells the player with [`ServerMsg::StoryUpdate`] and, if its phase changed, the move above.
//!
//! ```text
//! S -> C  AreaSnapshot, InstancePlacement, CharacterProgress, Inventory, StoryUpdate { advance: None }   join, non-default story only
//! S -> C  StoryUpdate { progress, advance: Some { point, summary } }       the story advanced
//! S -> C  AreaSnapshot { same area, the new instance's entities and NPCs }, InstancePlacement   its phase changed
//! (S -> old instance  PlayerLeft { entity })    (S -> new instance  PlayerJoined { snapshot })
//! ```
//!
//! An account that has never advanced gets exactly the messages it always got.
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
//! per-game appearance of an entity come from the stored player character. `Hello::character` is only
//! the character requested for a NEW account (ignored when logging in with a token).
//!
//! # Characters (v8)
//!
//! Every player has ONE original character that travels across games. In FF7 it fights with the stat curve
//! of a canon party member (its "fighting style"; chosen at creation, fixed) while the canon party stays in the
//! story and fights beside the player (v9: as server-controlled companions, the player's character is an extra
//! member); dialogue is never name-swapped; identity and appearance persist across games and each game
//! draws them its own way. Three things are separate on the wire:
//!
//! * the **identity**: name, colour and a game-agnostic body choice, asked for in `Hello::character` and
//!   told back in `Welcome::character` ([`CharacterCard`]);
//! * the **binding**: how the hosted game draws the character. It is the entity's `appearance_ref` in
//!   every snapshot (FF7: a generic placeholder body of the owner's own `char.lgp`, tinted with the colour;
//!   an account that predates v8 keeps the canon look it had) and has nothing to do with the slot;
//! * the **slot** (v9: the character's STAT CURVE, shown to players as a "fighting style"): the canon character
//!   whose combat template and experience curve the character fights with (`BattleMember::character`,
//!   `CharacterProgress::character`). Chosen once at creation, never changed, and not Cloud's for a new
//!   character; two players may hold the same one. It is a number of the stats, never an identity.
//!
//! ```text
//! C -> S  Hello { .., auth: Register { display_name }, character: { tint, body, slot } }
//! S -> C  Welcome { .., character: { name, tint, body, binding, slot, migrated } }
//! S -> C  EncounterStart { .., party: [{ controller, name, character, level, .. }] }       (v9: see above)
//! ```
//!
//! The battle roster carries each fighter's own `name`: the party panel, the HP list and the banners show it
//! instead of the canon character's, while the engine uses the stat curve's combat template. Nothing here is game
//! data (ids, a colour, a name).
//!
//! The story lane's [`ServerMsg::StoryUpdate`] (see "Story, phases and instances" above) was added to v7 without
//! a version of its own, because only an account that has advanced a story is sent one; v8 is the first version
//! every client of which knows it. A player's story is the account's, never the character's or the slot's.
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

pub use fmv_core::{InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_core::{AccountId, AnimState, AreaId, EntityId, EntitySnapshot, Fingerprint, Pos, ProvenanceSummary, SessionId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub mod battle;
pub mod character;
pub mod reject;

pub use character::{clip_character_name, CharacterCard, NewCharacter, MAX_CHARACTER_NAME_CHARS, MAX_CHARACTER_REF_CHARS};
pub use reject::RejectCode;

/// The battle wire types (the engine's own `BattleEvent`, `Command` and `Outcome` stay under
/// [`battle`], where their names say what they are).
pub use battle::{
    assign_drops, reward_roster, split_rewards, ActionRef, ActorId, AppliedCommand, BattleBuildError, BattleId, BattleMember,
    BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup, Controller, Divergence, Ff7BattleData,
    Ff7BattleDataError, FormationInfo, ProgressLimits, SyntheticRules, Target, MAX_BATTLE_PARTY, PLAYER_LIMIT_BREAKS,
};

/// The party types carried on the wire, re-exported so a client needs no direct `fmv-party`
/// dependency to match on them.
pub use fmv_party::{
    DisbandReason, InstanceKey, Invite, InviteCancelReason, InviteId, LeaderChangeReason, LeaveReason, Party, PartyError,
    PartyEvent, PartyId, Placement,
};

pub const PROTOCOL_VERSION: u16 = 9;
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
// Area travel (v4): non-player characters.
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
    /// First message of a connection. `character` is used only to create a NEW character (when `auth` is
    /// `Register`; a token login ignores it, except that its `tint` colours an account that has no avatar
    /// yet). The server checks every choice against what the hosted game offers: an unknown `body` is not an
    /// error (the game's default body is used), a `slot` the game does not offer to a new character is
    /// refused with `RejectCode::BadRequest` (see [`NewCharacter`]).
    Hello { protocol_version: u16, client_build: String, auth: Auth, character: NewCharacter },
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
    /// assigned to this session, and its ATB gauge must be full. The command is an attack, a spell the
    /// character knows (it costs MP), an item the ACCOUNT owns (the server checks the count) or a guard
    /// (see [`battle`]). Refused with `BattleRejected`.
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
        /// What the server holds of this player's own character (v8): its identity, how the hosted game draws
        /// it, which combat slot it fills.
        character: CharacterCard,
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
    /// The battle is over (the message before it carried `Victory` or `Defeat`, or the `Escape` command
    /// that got the party away, v7: `Escaped`). `rewards` has one entry per member of the party when the
    /// outcome is `Victory` (see [`split_rewards`]; the items in it are the drops, see [`assign_drops`])
    /// and is empty on `Defeat` and `Escaped`. The receiving player is back in the field.
    BattleEnd { battle_id: BattleId, outcome: battle::Outcome, rewards: Vec<BattleReward> },
    /// A battle request or command of this client was refused. `battle_id` is the battle it was
    /// about, if any.
    BattleRejected { battle_id: Option<BattleId>, reason: BattleRejection },
    /// This player's own character progress, as the server holds it: sent after the join messages
    /// and again right after `BattleEnd` of every won battle this player was a member of, whether or
    /// not it was paid (see the module docs, "Character progress").
    /// `character` is the game's character id (FF7: Cloud 0, Barret 1, ...,
    /// `fmv_ff7_battle::kernel::CharacterId::index`), `exp` the total experience, `exp_to_next` what is
    /// still missing for the next level (0 at the highest), `gil` and `ap` what the player holds, and
    /// `leveled_up` whether this message follows a level gained (never on the join message).
    CharacterProgress { character: u8, level: u8, exp: u32, exp_to_next: u32, gil: u32, ap: u32, leveled_up: bool },
    /// This player's whole inventory, as the server holds it: sent after the join messages (right after
    /// `CharacterProgress`), after every item the player used in a battle and after a won battle that
    /// dropped it items (see the module docs, "Inventory"). `items` is at most
    /// [`MAX_INVENTORY_STACKS`] entries, one per item, none of zero.
    Inventory { items: Vec<InventoryEntry> },
    /// This player's story progress, as the server holds it (see the module docs, "Story"): sent right
    /// after the join messages when the account's progression is not a new account's (the empty
    /// blob), and again whenever the server advances it (`advance` says which story point did). A
    /// client that never receives one is a new account. `progress` is the game's own opaque blob
    /// (FF7: `[1, moment_lo, moment_hi]`): the client's adapter reads it, nothing else may.
    StoryUpdate {
        progress: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        advance: Option<StoryAdvance>,
    },
}

/// What moved a player's story (see [`ServerMsg::StoryUpdate`]): the story point the server saw it
/// reach and what it changed, in the adapter's own `key=value` words (FF7: `moment=1->6`; numbers
/// and names of the adapter's own, never text of the game).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryAdvance {
    /// The area's id of the story point (`AreaRuntime::story_points`).
    pub point: u32,
    pub summary: String,
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
    fn the_version_is_nine() {
        assert_eq!(PROTOCOL_VERSION, 9);
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
            character: NewCharacter::with_tint([1, 2, 3]),
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
            character: CharacterCard::plain("Cloud", [1, 2, 3]),
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
                protocol_version: 9,
                client_build: "c/1".into(),
                auth: Auth::Register { display_name: "Blizz".into() },
                character: NewCharacter { tint: [10, 20, 30], body: Some("sturdy".into()), slot: Some(1) },
            },
            r#"{"Hello":{"protocol_version":9,"client_build":"c/1","auth":{"Register":{"display_name":"Blizz"}},"character":{"tint":[10,20,30],"body":"sturdy","slot":1}}}"#,
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
                protocol_version: 9,
                client_build: "c/1".into(),
                auth: Auth::Token(Secret::new("fmvt_x")),
                character: NewCharacter::with_tint([0, 0, 0]),
            },
            r#"{"Hello":{"protocol_version":9,"client_build":"c/1","auth":{"Token":"fmvt_x"},"character":{"tint":[0,0,0],"body":null,"slot":null}}}"#,
        );
    }

    #[test]
    fn golden_server_messages() {
        golden(
            ServerMsg::Welcome {
                protocol_version: 9,
                server_build: "s/1".into(),
                session: SessionId(4),
                entity: EntityId(5),
                account: AccountId(6),
                issued_token: None,
                character: CharacterCard {
                    name: "Blizz".into(),
                    tint: [10, 20, 30],
                    body: Some("sturdy".into()),
                    binding: Some("ff7/body/sturdy".into()),
                    slot: Some(1),
                    migrated: false,
                },
            },
            concat!(
                r#"{"Welcome":{"protocol_version":9,"server_build":"s/1","session":4,"entity":5,"account":6,"issued_token":null,"#,
                r#""character":{"name":"Blizz","tint":[10,20,30],"body":"sturdy","binding":"ff7/body/sturdy","slot":1,"migrated":false}}}"#
            ),
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
        // A phase key is just bytes in the placement: a non-default one changes nothing about its shape.
        golden(
            ServerMsg::InstancePlacement {
                placement: Placement {
                    account: AccountId(2),
                    key: InstanceKey::new(AreaId { game: GameId::Ff7, key: "md1stin".into() }, PhaseKey(vec![0x4D, 2, 0])),
                    party: None,
                    phase_owner: AccountId(2),
                    story_advance_allowed: true,
                },
            },
            concat!(
                r#"{"InstancePlacement":{"placement":{"account":2,"key":{"area":{"game":"Ff7","key":"md1stin"},"phase":[77,2,0]},"#,
                r#""party":null,"phase_owner":2,"story_advance_allowed":true}}}"#
            ),
        );
    }

    #[test]
    fn golden_story_update() {
        // A step: the whole progression (the game's opaque blob) and what moved it.
        golden(
            ServerMsg::StoryUpdate { progress: vec![1, 6, 0], advance: Some(StoryAdvance { point: 0, summary: "moment=1->6".into() }) },
            r#"{"StoryUpdate":{"progress":[1,6,0],"advance":{"point":0,"summary":"moment=1->6"}}}"#,
        );
        // Said at join: no `advance` key at all.
        golden(ServerMsg::StoryUpdate { progress: vec![1, 6, 0], advance: None }, r#"{"StoryUpdate":{"progress":[1,6,0]}}"#);
        golden(ServerMsg::StoryUpdate { progress: vec![], advance: None }, r#"{"StoryUpdate":{"progress":[]}}"#);
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
            BattleMember::player(EntityId(10), AccountId(20), "Blizz", 0, 6, 0),
            BattleMember {
                weapon: Some(3),
                armor: Some(4),
                ..BattleMember::player(EntityId(11), AccountId(21), "Red XIII", 1, 7, 137)
            },
        ]
    }

    /// The party of a battle on the first reactor mission with two players: the canon party first, commanded by the
    /// server, then each player's character (v9).
    fn canon_and_players() -> Vec<BattleMember> {
        let mut party = vec![BattleMember::companion("Cloud", 0, 7, 0), BattleMember::companion("Barret", 1, 7, 0)];
        party.extend(members());
        party
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
        // v6: the other commands.
        golden(
            ClientMsg::BattleCommand {
                battle_id: BattleId(1),
                actor: ActorId(1),
                command: battle::Command::Magic { spell: 27, target: Target::One(ActorId(3)) },
            },
            r#"{"BattleCommand":{"battle_id":1,"actor":1,"command":{"Magic":{"spell":27,"target":{"One":3}}}}}"#,
        );
        golden(
            ClientMsg::BattleCommand {
                battle_id: BattleId(1),
                actor: ActorId(0),
                command: battle::Command::Item { item: 7, target: Target::One(ActorId(1)) },
            },
            r#"{"BattleCommand":{"battle_id":1,"actor":0,"command":{"Item":{"item":7,"target":{"One":1}}}}}"#,
        );
        golden(
            ClientMsg::BattleCommand { battle_id: BattleId(1), actor: ActorId(0), command: battle::Command::Defend },
            r#"{"BattleCommand":{"battle_id":1,"actor":0,"command":"Defend"}}"#,
        );
        // v7: the limit break and the escape.
        golden(
            ClientMsg::BattleCommand { battle_id: BattleId(1), actor: ActorId(0), command: battle::Command::Limit { target: Target::One(ActorId(3)) } },
            r#"{"BattleCommand":{"battle_id":1,"actor":0,"command":{"Limit":{"target":{"One":3}}}}}"#,
        );
        golden(
            ClientMsg::BattleCommand { battle_id: BattleId(1), actor: ActorId(2), command: battle::Command::Escape },
            r#"{"BattleCommand":{"battle_id":1,"actor":2,"command":"Escape"}}"#,
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
                r#"{"controller":{"Player":{"entity":10,"account":20}},"name":"Blizz","character":0,"level":6,"weapon":null,"armor":null,"limit":0},"#,
                r#"{"controller":{"Player":{"entity":11,"account":21}},"name":"Red XIII","character":1,"level":7,"weapon":3,"armor":4,"limit":137}],"#,
                r#""preemptive":false,"back_attack":true,"battle_speed":128,"ticks_per_second":60,"data_fingerprint":""#,
                "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                r#""}}"#
            ),
        );
        // v9: the roster of a battle on the first mission with two players, the canon party first. A companion is
        // the bare word `Companion` (no account, no entity), and its fields are those of any member.
        golden(
            ServerMsg::EncounterStart {
                battle_id: BattleId(2),
                formation_id: 301,
                seed: 5,
                party: canon_and_players(),
                preemptive: false,
                back_attack: false,
                battle_speed: 0,
                ticks_per_second: 240,
                data_fingerprint: Fingerprint([0xab; 32]),
            },
            concat!(
                r#"{"EncounterStart":{"battle_id":2,"formation_id":301,"seed":5,"party":["#,
                r#"{"controller":"Companion","name":"Cloud","character":0,"level":7,"weapon":null,"armor":null,"limit":0},"#,
                r#"{"controller":"Companion","name":"Barret","character":1,"level":7,"weapon":null,"armor":null,"limit":0},"#,
                r#"{"controller":{"Player":{"entity":10,"account":20}},"name":"Blizz","character":0,"level":6,"weapon":null,"armor":null,"limit":0},"#,
                r#"{"controller":{"Player":{"entity":11,"account":21}},"name":"Red XIII","character":1,"level":7,"weapon":3,"armor":4,"limit":137}],"#,
                r#""preemptive":false,"back_attack":false,"battle_speed":0,"ticks_per_second":240,"data_fingerprint":""#,
                "abababababababababababababababababababababababababababababababab",
                r#""}}"#
            ),
        );
        golden(Controller::Companion, r#""Companion""#);
        golden(Controller::Player { entity: EntityId(3), account: AccountId(4) }, r#"{"Player":{"entity":3,"account":4}}"#);
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
                    BattleReward::new(AccountId(20), 6, 10, 2),
                    BattleReward::new(AccountId(21), 0, 0, 0),
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
        // v7: a limit (the gauge is spent before the blow), an escape that got away and the end it leads to.
        golden(
            ServerMsg::BattleEvent {
                battle_id: BattleId(1),
                tick: 200,
                events: vec![
                    battle::BattleEvent::Action { actor: ActorId(0), action: battle::ActionRef::Limit(128) },
                    battle::BattleEvent::LimitGauge { actor: ActorId(0), value: 0 },
                    battle::BattleEvent::Damage { actor: ActorId(0), target: ActorId(3), amount: 321, critical: false },
                ],
                command: Some(AppliedCommand { actor: ActorId(0), command: battle::Command::Limit { target: Target::One(ActorId(3)) } }),
            },
            concat!(
                r#"{"BattleEvent":{"battle_id":1,"tick":200,"events":[{"Action":{"actor":0,"action":{"Limit":128}}},"#,
                r#"{"LimitGauge":{"actor":0,"value":0}},{"Damage":{"actor":0,"target":3,"amount":321,"critical":false}}],"#,
                r#""command":{"actor":0,"command":{"Limit":{"target":{"One":3}}}}}}"#
            ),
        );
        golden(
            ServerMsg::BattleEvent {
                battle_id: BattleId(1),
                tick: 210,
                events: vec![battle::BattleEvent::Action { actor: ActorId(1), action: battle::ActionRef::Escape }],
                command: Some(AppliedCommand { actor: ActorId(1), command: battle::Command::Escape }),
            },
            r#"{"BattleEvent":{"battle_id":1,"tick":210,"events":[{"Action":{"actor":1,"action":"Escape"}}],"command":{"actor":1,"command":"Escape"}}}"#,
        );
        golden(
            ServerMsg::BattleEnd { battle_id: BattleId(3), outcome: battle::Outcome::Escaped, rewards: vec![] },
            r#"{"BattleEnd":{"battle_id":3,"outcome":"Escaped","rewards":[]}}"#,
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

    /// Character progress (v5): the message is pinned field by field, and it is small whatever its
    /// numbers are (it goes to every player after every battle).
    #[test]
    fn golden_character_progress() {
        golden(
            ServerMsg::CharacterProgress { character: 0, level: 7, exp: 626, exp_to_next: 323, gil: 10, ap: 2, leveled_up: true },
            r#"{"CharacterProgress":{"character":0,"level":7,"exp":626,"exp_to_next":323,"gil":10,"ap":2,"leveled_up":true}}"#,
        );
        golden(
            ServerMsg::CharacterProgress { character: 2, level: 99, exp: 2_408_997, exp_to_next: 0, gil: 0, ap: 0, leveled_up: false },
            r#"{"CharacterProgress":{"character":2,"level":99,"exp":2408997,"exp_to_next":0,"gil":0,"ap":0,"leveled_up":false}}"#,
        );
        let biggest = ServerMsg::CharacterProgress {
            character: u8::MAX,
            level: u8::MAX,
            exp: u32::MAX,
            exp_to_next: u32::MAX,
            gil: u32::MAX,
            ap: u32::MAX,
            leveled_up: true,
        };
        let frame = encode(&biggest).unwrap();
        assert!(frame.len() < 200, "{} bytes", frame.len());
        assert_eq!(decode::<ServerMsg>(&frame[4..]).unwrap(), biggest);
        // Every field is required: a message that lost one is refused, not guessed.
        for missing in ["character", "level", "exp", "exp_to_next", "gil", "ap", "leveled_up"] {
            let mut v = serde_json::json!({"character":0,"level":1,"exp":0,"exp_to_next":6,"gil":0,"ap":0,"leveled_up":false});
            v.as_object_mut().unwrap().remove(missing);
            let text = serde_json::json!({ "CharacterProgress": v }).to_string();
            assert!(decode::<ServerMsg>(text.as_bytes()).is_err(), "decoded without {missing}");
        }
    }

    /// The reason for the v5 bump: an older peer cannot decode `CharacterProgress`, which a server
    /// sends right after every join, so the handshake has to refuse it first. (The older peer's
    /// message enum is simulated by an enum that lacks the variant.)
    #[test]
    fn an_older_peer_cannot_decode_the_new_message_so_the_version_had_to_change() {
        #[derive(Debug, Deserialize)]
        enum ServerMsgV4 {
            #[allow(dead_code)]
            Pong { nonce: u64, tick: u64 },
        }
        let msg = ServerMsg::CharacterProgress { character: 0, level: 6, exp: 372, exp_to_next: 244, gil: 0, ap: 0, leveled_up: false };
        let frame = encode(&msg).unwrap();
        assert!(decode::<ServerMsgV4>(&frame[4..]).is_err());
        assert!(decode::<ServerMsgV4>(br#"{"Pong":{"nonce":1,"tick":2}}"#).is_ok(), "the stand-in decodes what it knows");
        // And the handshake check that refuses such a peer reads the version from any Hello.
        let hello_v4 = br#"{"Hello":{"protocol_version":4,"client_build":"c","auth":{"Token":"x"},"tint":[0,0,0],"appearance_ref":null}}"#;
        assert_eq!(probe_hello_version(hello_v4), Some(4));
        assert_ne!(probe_hello_version(hello_v4), Some(PROTOCOL_VERSION));
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
            // v6: every action is announced, and what it did.
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Attack },
                r#"{"Action":{"actor":0,"action":"Attack"}}"#,
            ),
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Magic(27) },
                r#"{"Action":{"actor":0,"action":{"Magic":27}}}"#,
            ),
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Item(7) },
                r#"{"Action":{"actor":0,"action":{"Item":7}}}"#,
            ),
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Defend },
                r#"{"Action":{"actor":0,"action":"Defend"}}"#,
            ),
            (
                battle::BattleEvent::Action { actor: t, action: battle::ActionRef::EnemyAttack(770) },
                r#"{"Action":{"actor":3,"action":{"EnemyAttack":770}}}"#,
            ),
            (battle::BattleEvent::Heal { actor: a, target: t, amount: 271 }, r#"{"Heal":{"actor":0,"target":3,"amount":271}}"#),
            (battle::BattleEvent::MpSpent { actor: a, amount: 5 }, r#"{"MpSpent":{"actor":0,"amount":5}}"#),
            (
                battle::BattleEvent::MpRestored { actor: a, target: t, amount: 100 },
                r#"{"MpRestored":{"actor":0,"target":3,"amount":100}}"#,
            ),
            (battle::BattleEvent::Revive { actor: a, target: t, hp: 100 }, r#"{"Revive":{"actor":0,"target":3,"hp":100}}"#),
            // v7: the limit gauge, the limit break and the escape.
            (battle::BattleEvent::LimitGauge { actor: a, value: 137 }, r#"{"LimitGauge":{"actor":0,"value":137}}"#),
            (battle::BattleEvent::LimitGauge { actor: t, value: 255 }, r#"{"LimitGauge":{"actor":3,"value":255}}"#),
            (battle::BattleEvent::EscapeFailed { actor: a }, r#"{"EscapeFailed":{"actor":0}}"#),
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Limit(128) },
                r#"{"Action":{"actor":0,"action":{"Limit":128}}}"#,
            ),
            (
                battle::BattleEvent::Action { actor: a, action: battle::ActionRef::Escape },
                r#"{"Action":{"actor":0,"action":"Escape"}}"#,
            ),
        ];
        for (event, expected) in events {
            golden(event, expected);
        }
        golden(battle::Command::Attack { target: t }, r#"{"Attack":{"target":3}}"#);
        golden(battle::Command::Magic { spell: 0, target: Target::One(t) }, r#"{"Magic":{"spell":0,"target":{"One":3}}}"#);
        golden(battle::Command::Item { item: 3, target: Target::One(a) }, r#"{"Item":{"item":3,"target":{"One":0}}}"#);
        golden(battle::Command::Defend, r#""Defend""#);
        golden(battle::Command::Limit { target: Target::One(t) }, r#"{"Limit":{"target":{"One":3}}}"#);
        // v8: a target is one actor or a whole side (the sides are the commanding actor's: its enemies, its allies).
        golden(Target::One(t), r#"{"One":3}"#);
        golden(Target::AllEnemies, r#""AllEnemies""#);
        golden(Target::AllAllies, r#""AllAllies""#);
        golden(battle::Command::Magic { spell: 27, target: Target::AllEnemies }, r#"{"Magic":{"spell":27,"target":"AllEnemies"}}"#);
        golden(battle::Command::Magic { spell: 0, target: Target::AllAllies }, r#"{"Magic":{"spell":0,"target":"AllAllies"}}"#);
        golden(battle::Command::Item { item: 6, target: Target::AllAllies }, r#"{"Item":{"item":6,"target":"AllAllies"}}"#);
        golden(battle::Command::Item { item: 21, target: Target::AllEnemies }, r#"{"Item":{"item":21,"target":"AllEnemies"}}"#);
        golden(battle::Command::Limit { target: Target::AllAllies }, r#"{"Limit":{"target":"AllAllies"}}"#);
        golden(battle::Command::Escape, r#""Escape""#);
        golden(battle::Outcome::Victory, r#""Victory""#);
        golden(battle::Outcome::Defeat, r#""Defeat""#);
        golden(battle::Outcome::Escaped, r#""Escaped""#);
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
            // v6
            (BattleRejection::TargetNotDead, r#""TargetNotDead""#),
            (BattleRejection::UnknownSpell, r#""UnknownSpell""#),
            (BattleRejection::NotEnoughMp, r#""NotEnoughMp""#),
            (BattleRejection::UnknownItem, r#""UnknownItem""#),
            (BattleRejection::OutOfItems, r#""OutOfItems""#),
            // v7
            (BattleRejection::NoLimit, r#""NoLimit""#),
            (BattleRejection::LimitNotReady, r#""LimitNotReady""#),
            (BattleRejection::CannotEscape, r#""CannotEscape""#),
        ];
        for (reason, expected) in cases {
            golden(reason, expected);
        }
    }

    /// v6: the inventory message, and the items a reward carries (absent from the JSON when empty, so a
    /// reward without items is exactly what it was).
    #[test]
    fn golden_inventory_and_drops() {
        golden(
            ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 1)] },
            r#"{"Inventory":{"items":[{"item":0,"count":3},{"item":7,"count":1}]}}"#,
        );
        golden(ServerMsg::Inventory { items: vec![] }, r#"{"Inventory":{"items":[]}}"#);
        let mut with_drops = BattleReward::new(AccountId(20), 6, 10, 2);
        with_drops.items = vec![InventoryEntry::new(0, 1), InventoryEntry::new(3, 2)];
        golden(
            ServerMsg::BattleEnd {
                battle_id: BattleId(1),
                outcome: battle::Outcome::Victory,
                rewards: vec![with_drops.clone(), BattleReward::new(AccountId(21), 0, 0, 0)],
            },
            concat!(
                r#"{"BattleEnd":{"battle_id":1,"outcome":"Victory","rewards":["#,
                r#"{"account":20,"exp":6,"gil":10,"ap":2,"items":[{"item":0,"count":1},{"item":3,"count":2}]},"#,
                r#"{"account":21,"exp":0,"gil":0,"ap":0}]}}"#
            ),
        );
        // A reward from a v5 server (no `items` key) reads as one without items.
        let old: BattleReward = serde_json::from_str(r#"{"account":20,"exp":6,"gil":10,"ap":2}"#).unwrap();
        assert_eq!(old, BattleReward::new(AccountId(20), 6, 10, 2));
        assert!(!old.is_nothing() && BattleReward::new(AccountId(1), 0, 0, 0).is_nothing());
        assert!(!with_drops.is_nothing() && with_drops.without_items() == BattleReward::new(AccountId(20), 6, 10, 2));
        let only_items = BattleReward { items: vec![InventoryEntry::new(1, 1)], ..BattleReward::new(AccountId(1), 0, 0, 0) };
        assert!(!only_items.is_nothing(), "items alone are a payment");
    }

    /// The fullest inventory a server may send (every stack at its widest) is far from a frame, and a
    /// message with more entries than the limit is the sender's bug, not something the codec refuses.
    #[test]
    fn an_inventory_is_small_whatever_it_holds() {
        let full = ServerMsg::Inventory {
            items: (0..MAX_INVENTORY_STACKS as u16).map(|i| InventoryEntry::new(60_000 + i, u16::MAX)).collect(),
        };
        let frame = encode(&full).unwrap();
        assert!(frame.len() < 12_000, "{} bytes", frame.len());
        assert_eq!(decode::<ServerMsg>(&frame[4..]).unwrap(), full);
        // Every field is required and 16 bits wide.
        for bad in [
            r#"{"Inventory":{}}"#,
            r#"{"Inventory":{"items":[{"item":1}]}}"#,
            r#"{"Inventory":{"items":[{"item":70000,"count":1}]}}"#,
            r#"{"Inventory":{"items":[{"item":1,"count":-1}]}}"#,
        ] {
            assert!(decode::<ServerMsg>(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    /// The reason for the v6 bump: a v5 peer cannot decode the inventory that follows every join, nor
    /// the new events and commands. (The older peer's enums are simulated by enums that lack the
    /// variants.) The handshake that refuses it reads the version from its `Hello`.
    #[test]
    fn a_v5_peer_cannot_decode_the_v6_messages_so_the_version_had_to_change() {
        #[derive(Debug, Deserialize)]
        enum ServerMsgV5 {
            #[allow(dead_code)]
            Pong { nonce: u64, tick: u64 },
        }
        #[derive(Debug, Deserialize)]
        enum EventV5 {
            #[allow(dead_code)]
            GaugeReady { actor: ActorId },
        }
        #[derive(Debug, Deserialize)]
        enum CommandV5 {
            #[allow(dead_code)]
            Attack { target: ActorId },
        }
        let inventory = encode(&ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3)] }).unwrap();
        assert!(decode::<ServerMsgV5>(&inventory[4..]).is_err());
        assert!(decode::<ServerMsgV5>(br#"{"Pong":{"nonce":1,"tick":2}}"#).is_ok(), "the stand-in decodes what it knows");
        let heal = encode(&battle::BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 3 }).unwrap();
        assert!(decode::<EventV5>(&heal[4..]).is_err());
        assert!(decode::<EventV5>(br#"{"GaugeReady":{"actor":1}}"#).is_ok());
        for command in [battle::Command::Magic { spell: 0, target: Target::One(ActorId(1)) }, battle::Command::Defend] {
            assert!(decode::<CommandV5>(&encode(&command).unwrap()[4..]).is_err(), "{command:?}");
        }
        assert!(decode::<CommandV5>(br#"{"Attack":{"target":1}}"#).is_ok());
        let hello_v5 = br#"{"Hello":{"protocol_version":5,"client_build":"c","auth":{"Token":"x"},"tint":[0,0,0],"appearance_ref":null}}"#;
        assert_eq!(probe_hello_version(hello_v5), Some(5));
        assert_ne!(probe_hello_version(hello_v5), Some(PROTOCOL_VERSION));
    }

    /// The reason for the v7 bump: a v6 peer cannot decode a limit, an escape, the new events, the new
    /// outcome or the new refusals, and a v6 `BattleMember` has no limit gauge to start from. (The older
    /// peer's enums are simulated by enums that lack the variants.) The handshake refuses it first.
    #[test]
    fn a_v7_peer_cannot_read_the_v8_target_and_a_v8_peer_cannot_read_the_v7_one_so_the_version_had_to_change() {
        // What a v7 peer's `Command` is: the target of a spell, an item and a limit is a bare actor.
        #[derive(Debug, Deserialize)]
        enum CommandV7 {
            #[allow(dead_code)]
            Attack { target: ActorId },
            #[allow(dead_code)]
            Magic { spell: u16, target: ActorId },
            #[allow(dead_code)]
            Item { item: u16, target: ActorId },
            #[allow(dead_code)]
            Defend,
            #[allow(dead_code)]
            Limit { target: ActorId },
            #[allow(dead_code)]
            Escape,
        }
        let a = ActorId(1);
        for command in [
            battle::Command::Magic { spell: 0, target: Target::One(a) },
            battle::Command::Item { item: 3, target: Target::One(a) },
            battle::Command::Limit { target: Target::One(a) },
            battle::Command::Magic { spell: 27, target: Target::AllEnemies },
            battle::Command::Item { item: 6, target: Target::AllAllies },
            battle::Command::Limit { target: Target::AllAllies },
        ] {
            assert!(decode::<CommandV7>(&encode(&command).unwrap()[4..]).is_err(), "{command:?}");
        }
        // What does not change: Attack, Defend and Escape read the same in both.
        for command in [battle::Command::Attack { target: a }, battle::Command::Defend, battle::Command::Escape] {
            assert!(decode::<CommandV7>(&encode(&command).unwrap()[4..]).is_ok(), "{command:?}");
        }
        // And the other way round: a v7 spell, item or limit (a bare number) is no v8 command.
        for old in [
            r#"{"Magic":{"spell":0,"target":3}}"#,
            r#"{"Item":{"item":3,"target":0}}"#,
            r#"{"Limit":{"target":3}}"#,
        ] {
            assert!(decode::<battle::Command>(old.as_bytes()).is_err(), "{old}");
        }
        assert!(decode::<battle::Command>(br#"{"Attack":{"target":3}}"#).is_ok(), "an attack is the same in v7");
        let hello_v7 = br#"{"Hello":{"protocol_version":7,"client_build":"c","auth":{"Token":"x"},"tint":[0,0,0],"appearance_ref":null}}"#;
        assert_eq!(probe_hello_version(hello_v7), Some(7));
        assert_ne!(probe_hello_version(hello_v7), Some(PROTOCOL_VERSION));
    }

    #[test]
    fn a_v6_peer_cannot_decode_the_v7_messages_so_the_version_had_to_change() {
        #[derive(Debug, Deserialize)]
        enum CommandV6 {
            #[allow(dead_code)]
            Attack { target: ActorId },
            #[allow(dead_code)]
            Defend,
        }
        #[derive(Debug, Deserialize)]
        enum ActionV6 {
            #[allow(dead_code)]
            Attack,
            #[allow(dead_code)]
            EnemyAttack(u16),
        }
        #[derive(Debug, Deserialize)]
        enum EventV6 {
            #[allow(dead_code)]
            GaugeReady { actor: ActorId },
            #[allow(dead_code)]
            Action { actor: ActorId, action: ActionV6 },
        }
        #[derive(Debug, Deserialize)]
        enum OutcomeV6 {
            #[allow(dead_code)]
            Victory,
            #[allow(dead_code)]
            Defeat,
        }
        #[derive(Debug, Deserialize)]
        enum RejectionV6 {
            #[allow(dead_code)]
            OutOfItems,
        }
        #[derive(Debug, Deserialize)]
        struct MemberV6 {
            #[allow(dead_code)]
            level: u8,
            #[allow(dead_code)]
            armor: Option<u8>,
        }
        for command in [battle::Command::Limit { target: Target::One(ActorId(1)) }, battle::Command::Escape] {
            assert!(decode::<CommandV6>(&encode(&command).unwrap()[4..]).is_err(), "{command:?}");
        }
        assert!(decode::<CommandV6>(br#"{"Attack":{"target":1}}"#).is_ok(), "the stand-in decodes what it knows");
        for event in [
            battle::BattleEvent::LimitGauge { actor: ActorId(0), value: 5 },
            battle::BattleEvent::EscapeFailed { actor: ActorId(0) },
            battle::BattleEvent::Action { actor: ActorId(0), action: battle::ActionRef::Limit(128) },
            battle::BattleEvent::Action { actor: ActorId(0), action: battle::ActionRef::Escape },
        ] {
            assert!(decode::<EventV6>(&encode(&event).unwrap()[4..]).is_err(), "{event:?}");
        }
        assert!(decode::<EventV6>(br#"{"Action":{"actor":0,"action":{"EnemyAttack":9}}}"#).is_ok());
        assert!(decode::<OutcomeV6>(&encode(&battle::Outcome::Escaped).unwrap()[4..]).is_err());
        assert!(decode::<OutcomeV6>(br#""Defeat""#).is_ok());
        for reason in [BattleRejection::NoLimit, BattleRejection::LimitNotReady, BattleRejection::CannotEscape] {
            assert!(decode::<RejectionV6>(&encode(&reason).unwrap()[4..]).is_err(), "{reason:?}");
        }
        // A v6 member is a v7 member without its gauge: a v6 server's `EncounterStart` cannot be a v7
        // client's (the field is required), while a v7 member still reads as a v6 one.
        let v6_member = br#"{"entity":10,"account":20,"character":0,"level":6,"weapon":null,"armor":null}"#;
        assert!(decode::<BattleMember>(v6_member).is_err(), "a start from a v6 server has no limit gauge");
        assert!(decode::<MemberV6>(&encode(&members()[1]).unwrap()[4..]).is_ok());
        let hello_v6 = br#"{"Hello":{"protocol_version":6,"client_build":"c","auth":{"Token":"x"},"tint":[0,0,0],"appearance_ref":null}}"#;
        assert_eq!(probe_hello_version(hello_v6), Some(6));
        assert_ne!(probe_hello_version(hello_v6), Some(PROTOCOL_VERSION));
    }

    /// The player's own character (v8): what a registration asks for, what the welcome says, how another player
    /// reaches the others (the binding is the entity's `appearance_ref`, and has nothing to do with the slot).
    #[test]
    fn golden_character_messages() {
        // A registration that chose a body and a slot, one that chose nothing, and a token login (which ignores it).
        golden(
            ClientMsg::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_build: "c/1".into(),
                auth: Auth::Register { display_name: "Wedge".into() },
                character: NewCharacter { tint: [200, 10, 10], body: Some("slight".into()), slot: Some(8) },
            },
            r#"{"Hello":{"protocol_version":9,"client_build":"c/1","auth":{"Register":{"display_name":"Wedge"}},"character":{"tint":[200,10,10],"body":"slight","slot":8}}}"#,
        );
        golden(
            ClientMsg::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_build: "c/1".into(),
                auth: Auth::Register { display_name: "Plain".into() },
                character: NewCharacter::with_tint([1, 2, 3]),
            },
            r#"{"Hello":{"protocol_version":9,"client_build":"c/1","auth":{"Register":{"display_name":"Plain"}},"character":{"tint":[1,2,3],"body":null,"slot":null}}}"#,
        );
        // What the server says about an account that predates v8: it never chose a body, it keeps the look it had
        // (the binding is a canon model) and the character it was fighting as (Cloud, slot 0, which only a migrated
        // account may hold).
        golden(
            CharacterCard {
                name: "Blizz".into(),
                tint: [90, 125, 245],
                body: None,
                binding: Some("ff7/cloud".into()),
                slot: Some(0),
                migrated: true,
            },
            r#"{"name":"Blizz","tint":[90,125,245],"body":null,"binding":"ff7/cloud","slot":0,"migrated":true}"#,
        );
        // A game without combat slots (the FIXTURE server) tells an identity only.
        golden(
            CharacterCard::plain("Fixy", [4, 5, 6]),
            r#"{"name":"Fixy","tint":[4,5,6],"body":null,"binding":null,"slot":null,"migrated":false}"#,
        );
        // Another player's entity: its look is the binding, a generic body; nothing in it names a slot.
        golden(
            ServerMsg::PlayerJoined {
                snapshot: fmv_core::EntitySnapshot {
                    id: EntityId(12),
                    pos: Pos::new(1.0, 2.0, 3.0),
                    facing: 0.5,
                    anim: AnimState::Idle,
                    avatar: fmv_core::AvatarAppearance::new("Wedge", [200, 10, 10], Some("ff7/body/slight".into())),
                },
            },
            concat!(
                r#"{"PlayerJoined":{"snapshot":{"id":12,"pos":{"x":1.0,"y":2.0,"z":3.0},"facing":0.5,"anim":"Idle","#,
                r#""avatar":{"display_name":"Wedge","tint":[200,10,10],"appearance_ref":"ff7/body/slight"}}}}"#
            ),
        );
        // What a message cannot do without is refused, not guessed: the colour of a request; the name, the colour
        // and the migration flag of a card. A choice that is `None` may be left out (it reads as `None`, which is
        // what it would have said).
        let mut request = serde_json::json!({"tint":[0,0,0],"body":null,"slot":null});
        request.as_object_mut().unwrap().remove("tint");
        assert!(serde_json::from_value::<NewCharacter>(request).is_err(), "decoded without a colour");
        for missing in ["name", "tint", "migrated"] {
            let mut v = serde_json::json!({"name":"A","tint":[0,0,0],"body":null,"binding":null,"slot":null,"migrated":false});
            v.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<CharacterCard>(v).is_err(), "decoded without {missing}");
        }
        let bare: NewCharacter = serde_json::from_value(serde_json::json!({"tint":[1,2,3]})).unwrap();
        assert_eq!(bare, NewCharacter::with_tint([1, 2, 3]));
    }

    /// The reason for the v8 bump: neither side can read the other's `Hello`, and a roster member without a name
    /// is not one. (The older peer's messages are simulated by types of their shape.) Both directions end in the
    /// clean refusal, because the version of any `Hello` is readable.
    #[test]
    fn a_v7_peer_and_a_v8_peer_cannot_read_each_others_hello_so_the_version_had_to_change() {
        #[derive(Debug, Deserialize)]
        enum ClientMsgV7 {
            #[allow(dead_code)]
            Hello { protocol_version: u16, client_build: String, auth: serde_json::Value, tint: [u8; 3], appearance_ref: Option<String> },
        }
        // A v8 Hello is no v7 Hello (a v7 server cannot decode it and reads its version instead)...
        let v8 = br#"{"Hello":{"protocol_version":8,"client_build":"c","auth":{"Token":"x"},"character":{"tint":[0,0,0],"body":null,"slot":null}}}"#;
        assert!(decode::<ClientMsgV7>(v8).is_err());
        assert_eq!(probe_hello_version(v8), Some(8));
        // ...and a v7 Hello is no v8 Hello (a v8 server reads its version and answers `ProtocolMismatch`).
        let v7 = br#"{"Hello":{"protocol_version":7,"client_build":"c","auth":{"Token":"x"},"tint":[0,0,0],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsgV7>(v7).is_ok(), "the stand-in decodes what it knows");
        assert!(decode::<ClientMsg>(v7).is_err());
        assert_eq!(probe_hello_version(v7), Some(7));
        assert_ne!(probe_hello_version(v7), Some(PROTOCOL_VERSION));
        // A v7 server's roster member has no name (and, since v9, no controller): refused instead of invented.
        let v7_member = br#"{"entity":10,"account":20,"character":0,"level":6,"weapon":null,"armor":null,"limit":0}"#;
        assert!(decode::<BattleMember>(v7_member).is_err());
        // A v7 server's Welcome has no card.
        let v7_welcome = br#"{"Welcome":{"protocol_version":7,"server_build":"s","session":1,"entity":2,"account":3,"issued_token":null}}"#;
        assert!(decode::<ServerMsg>(v7_welcome).is_err());
    }

    /// The reason for the v9 bump: a v8 `Hello` is still a v9 `Hello` (nothing in it changed), so the version number is
    /// what refuses it; and the roster of a battle is not the same message in the two versions in either direction (the
    /// player's `entity` and `account` moved into the member's `controller`), so a v8 peer let in would stop at the
    /// first battle. (The v8 peer's roster is simulated by a type of its shape.) The version of any `Hello` is readable,
    /// so both refusals are clean.
    #[test]
    fn a_v8_peer_cannot_read_a_v9_roster_and_the_other_way_round_so_the_version_had_to_change() {
        #[derive(Debug, Deserialize)]
        struct MemberV8 {
            #[allow(dead_code)]
            entity: EntityId,
            #[allow(dead_code)]
            account: AccountId,
            #[allow(dead_code)]
            name: String,
            #[allow(dead_code)]
            character: u8,
            #[allow(dead_code)]
            level: u8,
            #[allow(dead_code)]
            weapon: Option<u8>,
            #[allow(dead_code)]
            armor: Option<u8>,
            #[allow(dead_code)]
            limit: u16,
        }
        // A v8 `Hello` reads as a v9 `Hello`; only its version says it is not for this server.
        let v8_hello = br#"{"Hello":{"protocol_version":8,"client_build":"c","auth":{"Token":"x"},"character":{"tint":[0,0,0],"body":null,"slot":null}}}"#;
        assert!(decode::<ClientMsg>(v8_hello).is_ok());
        assert_eq!(probe_hello_version(v8_hello), Some(8));
        assert_ne!(probe_hello_version(v8_hello), Some(PROTOCOL_VERSION));
        // A v9 member of either kind is no v8 member: the v8 peer cannot decode a roster of this version...
        for member in canon_and_players() {
            assert!(decode::<MemberV8>(&encode(&member).unwrap()[4..]).is_err(), "{member:?}");
        }
        // ...and a v8 member (with the player's own `entity` and `account` at the top) is no v9 member: no
        // controller, so it is refused instead of guessed to be a player.
        let v8_member = br#"{"entity":10,"account":20,"name":"Blizz","character":0,"level":6,"weapon":null,"armor":null,"limit":0}"#;
        assert!(decode::<MemberV8>(v8_member).is_ok(), "the stand-in decodes what it knows");
        assert!(decode::<BattleMember>(v8_member).is_err());
        // A whole v8 `EncounterStart` is no v9 message either.
        let v8_start = concat!(
            r#"{"EncounterStart":{"battle_id":1,"formation_id":517,"seed":12345,"party":["#,
            r#"{"entity":10,"account":20,"name":"Blizz","character":0,"level":6,"weapon":null,"armor":null,"limit":0}],"#,
            r#""preemptive":false,"back_attack":true,"battle_speed":128,"ticks_per_second":60,"data_fingerprint":""#,
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            r#""}}"#
        );
        assert!(decode::<ServerMsg>(v8_start.as_bytes()).is_err());
        // A controller names exactly who commands: a player has both an entity and an account, a companion neither.
        for bad in [r#"{"Player":{"entity":1}}"#, r#"{"Player":{"account":1}}"#, r#"{"Companion":{"account":1}}"#, r#""Player""#, r#""Spectator""#] {
            assert!(decode::<Controller>(bad.as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(decode::<Controller>(br#""Companion""#).unwrap(), Controller::Companion);
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
                    // The widest controller there is (a player); a companion's is the bare word.
                    controller: Controller::Player { entity: EntityId(u64::MAX - i), account: AccountId(u64::MAX - i) },
                    // The widest name a sender may put there: 24 four-byte characters.
                    name: "\u{10FFFF}".repeat(MAX_CHARACTER_NAME_CHARS),
                    character: u8::MAX,
                    level: u8::MAX,
                    weapon: Some(u8::MAX),
                    armor: Some(u8::MAX),
                    limit: u16::MAX,
                })
                .collect(),
            preemptive: false,
            back_attack: false,
            battle_speed: u8::MAX,
            ticks_per_second: u32::MAX,
            data_fingerprint: Fingerprint([0xff; 32]),
        };
        let frame = encode(&msg).unwrap();
        // Eight members with the widest names, ids and numbers: the names are most of it, and it is still a
        // few KiB of a 256 KiB frame.
        assert!(frame.len() < 4_096, "{} bytes", frame.len());
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
    fn a_hello_of_an_older_version_is_no_longer_a_message_but_its_version_is_still_readable() {
        // Every Hello of v3 to v7 spelled the colour and the look at the top level: v8 and v9 ask for a character.
        let v3 = br#"{"Hello":{"protocol_version":3,"client_build":"x","auth":{"Register":{"display_name":"n"}},"tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v3).is_err());
        assert_eq!(probe_hello_version(v3), Some(3));
    }

    #[test]
    fn a_hellos_version_can_be_read_whatever_else_it_contains() {
        // A v1 Hello (display_name at the top level) is not a v2 `ClientMsg`, but its version is readable.
        let v1 = br#"{"Hello":{"protocol_version":1,"client_build":"x","display_name":"Old","tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v1).is_err());
        assert_eq!(probe_hello_version(v1), Some(1));
        let v2 = br#"{"Hello":{"protocol_version":2,"client_build":"x","auth":{"Register":{"display_name":"n"}},"tint":[1,2,3],"appearance_ref":null}}"#;
        assert!(decode::<ClientMsg>(v2).is_err(), "no Hello before v8 is a v8 ClientMsg");
        assert_eq!(probe_hello_version(v2), Some(2));
        let future = br#"{"Hello":{"protocol_version":9,"anything":{"at":"all"}}}"#;
        assert_eq!(probe_hello_version(future), Some(9));
        for not_hello in [&br#""Leave""#[..], br#"{"Ping":{"nonce":1}}"#, b"not json", b"{}", br#"{"Hello":{}}"#, br#"{"Hello":{"protocol_version":70000}}"#] {
            assert_eq!(probe_hello_version(not_hello), None, "{}", String::from_utf8_lossy(not_hello));
        }
    }
}
