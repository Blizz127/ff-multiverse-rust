//! Typed reasons for [`ServerMsg::Reject`](crate::ServerMsg::Reject).
//!
//! A `Reject` carries a human sentence (`reason`, for the screen and the log) AND a [`RejectCode`]
//! (`code`, for the program). A client decides what to do and what to tell the player from the
//! code; it must never match on the English text, which is free to change.
//!
//! # Compatibility (why this is additive within protocol v3)
//!
//! * A server that has no code (older v3 build) sends `{"Reject":{"reason":"..."}}`: the field has
//!   `#[serde(default)]`, so a newer client reads it as [`RejectCode::Unspecified`].
//! * A server that sends a code to a client that does not know the field (older v3 build): serde
//!   ignores unknown fields of a struct variant, so the old client still reads `reason`.
//! * A code this build does not know (a newer server added one) reads as
//!   [`RejectCode::Unspecified`] (`#[serde(other)]`), never as a decode error that would turn a
//!   refusal into "malformed message".
//! * [`RejectCode::Unspecified`] is left out of the JSON, so a code-less `Reject` has exactly the
//!   shape it always had.
//!
//! Nothing a peer relied on is removed or renamed, so there is no version bump; adding a variant
//! later is compatible for the same reasons. Only a *change of meaning* of an existing code would
//! need one.

use serde::{Deserialize, Serialize};

/// Why the server refused or ended a connection. See the module docs for the wire rules.
///
/// The names are part of the wire (golden-tested in `lib.rs`): never rename or reuse one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RejectCode {
    /// The `Hello` speaks another protocol version.
    ProtocolMismatch,
    /// Every connection slot of the server is taken. Retrying later may work.
    ServerFull,
    /// Too many connections from this address at once (the per-address cap).
    TooManyConnections,
    /// The server takes no new accounts (closed to registration, or its account store is full).
    /// A saved token still works.
    RegistrationClosed,
    /// Too many requests too quickly: registrations (server-wide or from this address) or messages
    /// on this connection. Waiting helps.
    RateLimited,
    /// The display name is taken (compared the way the server compares names).
    NameTaken,
    /// The display name is unusable (nothing visible is left of it after cleaning).
    InvalidName,
    /// The token is unknown to this server (never issued, malformed, or the server's accounts were
    /// reset). Retrying the same token cannot work.
    InvalidToken,
    /// This account already has a session on the server.
    AlreadyOnline,
    /// The area, fingerprint or edition in `JoinArea` is not the server's.
    ContentMismatch,
    /// The client broke the protocol: a malformed or oversized frame, or a message that is not
    /// allowed at this point of the connection.
    BadRequest,
    /// The client did not send what the handshake needs (`Hello`, `JoinArea`) in time.
    Timeout,
    /// A joined session was closed for inactivity: it sent nothing at all, or only keepalive
    /// `Ping`s and no game input, for too long. Logging in again works.
    IdleTimeout,
    /// The server is stopping.
    ShuttingDown,
    /// The server failed (its account store, a worker). Not the client's fault; try again later.
    Internal,
    /// No code: the server predates the codes, or none of them describes the reason, or it is a
    /// code this build does not know. Use the text. (Last, because serde's `other` must be.)
    #[default]
    #[serde(other)]
    Unspecified,
}

impl RejectCode {
    /// Every code, for exhaustive tests and tables.
    pub const ALL: [RejectCode; 16] = [
        RejectCode::Unspecified,
        RejectCode::ProtocolMismatch,
        RejectCode::ServerFull,
        RejectCode::TooManyConnections,
        RejectCode::RegistrationClosed,
        RejectCode::RateLimited,
        RejectCode::NameTaken,
        RejectCode::InvalidName,
        RejectCode::InvalidToken,
        RejectCode::AlreadyOnline,
        RejectCode::ContentMismatch,
        RejectCode::BadRequest,
        RejectCode::Timeout,
        RejectCode::IdleTimeout,
        RejectCode::ShuttingDown,
        RejectCode::Internal,
    ];

    /// `true` for [`RejectCode::Unspecified`] (the code that is left out of the JSON).
    pub fn is_unspecified(&self) -> bool {
        matches!(self, RejectCode::Unspecified)
    }

    /// Whether the same request can succeed later without the player changing anything: the
    /// refusal is about the server's state or load, not about what was asked for.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            RejectCode::ServerFull
                | RejectCode::TooManyConnections
                | RejectCode::RateLimited
                | RejectCode::AlreadyOnline
                | RejectCode::Timeout
                | RejectCode::IdleTimeout
                | RejectCode::ShuttingDown
                | RejectCode::Internal
        )
    }
}
