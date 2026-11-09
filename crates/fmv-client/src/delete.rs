//! Deleting a character on the server: this client's side of `ClientMsg::DeleteCharacter` (see the protocol's module
//! docs, "Deleting a character"), and the `--delete NAME` command that uses it.
//!
//! A delete is a connection of its own, short and without a window: [`delete_character`] sends `DeleteCharacter` (the
//! protocol version, this client's build and the character's login token) as the FIRST message instead of `Hello`, reads
//! the one answer and is done. It is generic over the stream, so it is tested with in-memory pipes like
//! [`crate::net::handshake`]; [`delete_on`] runs it over TCP with the connect timeout and one overall deadline, and
//! [`delete_blocking`] runs that on a small runtime of its own for the threads that are not async (the window's
//! [`crate::ui::deleter::Deleter`] and the command).
//!
//! What the server's answer means, and what the client does with its saved login:
//!
//! | The server says | The result | The saved login |
//! |---|---|---|
//! | `CharacterDeleted { name }` | [`DeleteOutcome::Deleted`] | removed |
//! | `Reject { InvalidToken }`: it does not know this login | [`DeleteOutcome::AlreadyGone`] | removed |
//! | any other `Reject` (`AlreadyOnline`, `ProtocolMismatch`, `RateLimited`, `Internal`, ...) | [`NetError::Rejected`] | kept |
//! | no answer in time, a connection that fails or closes, anything else | [`NetError::Timeout`], [`NetError::Io`], [`NetError::Connect`], [`NetError::Protocol`] | kept |
//!
//! `InvalidToken` is "already gone" on purpose: it settles a delete whose answer was lost (the server committed it and the
//! connection died; asking again finds nothing) and a saved login of a server whose accounts were reset. **A saved login
//! is only ever removed after one of the first two answers**: whatever else happens (the server cannot be reached, it is
//! busy, the character is in the game right now) means nothing was deleted, and the login stays, so the player can try
//! again. The token is a [`Secret`]: it is sent on the wire and appears nowhere else, no log line, no message, no `{:?}`.
//!
//! Text from the server (the name in `CharacterDeleted`, the sentence of a `Reject`) is cleaned ([`clean`]) before it is
//! kept, like every text from the network.
//!
//! # `--delete NAME`
//!
//! [`run_command`] is the whole command with its surroundings handed in (the saved logins, the look notes, the network
//! call, the two output streams), so it is tested without a server, a terminal or the real state directory. It finds the
//! login saved for NAME on the server, asks the server to delete the character with it and, on `Deleted` or
//! `AlreadyGone`, removes the saved login and the note about the character's look, then prints ONE line on stdout:
//! `deleted character name=<enc>` or `character already gone on the server name=<enc>` (`<enc>` is the saved
//! login's name percent-encoded like the `name=` of the smoke line, see [`encode_name`]). Exit codes: 0 done, 1 any
//! failure (the saved login is kept and the error is on stderr), 2 when no login is saved for NAME.

use crate::accounts::{loose_mode_warning, SavedAccount, TokenStore};
use crate::cli::exit;
use crate::headless::encode_name;
use crate::net::{clean, connect, describe_proto_error, send, NetError};
use crate::ui::looks::LookCache;
use fmv_protocol::{read_msg, ClientMsg, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION};
use std::io::Write;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{timeout_at, Instant};

/// The whole exchange (the request and its answer) is over after this long, after the connection was made (which has
/// the connect timeout of its own, [`crate::net::CONNECT_TIMEOUT`]).
pub const DELETE_TIMEOUT: Duration = Duration::from_secs(15);

/// What the server answered to a delete request that it understood and that was not refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The server deleted the character. The name is the one the server held, cleaned ([`clean`]): it is free again.
    Deleted(String),
    /// The server does not know this login (`InvalidToken`): there is nothing left to delete on its side. The character
    /// was deleted earlier (possibly by this very request, whose answer was lost) or the server's accounts were reset.
    AlreadyGone,
}

/// Ask the server to delete the character of `token`, on a connection whose first message is the request. `budget` bounds
/// the whole exchange. See the module docs for what each answer is.
pub async fn delete_character<R, W>(rd: &mut R, wr: &mut W, token: &Secret, client_build: &str, budget: Duration) -> Result<DeleteOutcome, NetError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let deadline = Instant::now() + budget;
    let request = ClientMsg::DeleteCharacter { protocol_version: PROTOCOL_VERSION, client_build: client_build.to_string(), token: token.clone() };
    send(wr, &request, "sending DeleteCharacter", deadline).await?;
    let answer: ServerMsg = match timeout_at(deadline, read_msg(rd)).await {
        Err(_) => return Err(NetError::Timeout("CharacterDeleted")),
        Ok(Err(source)) => return Err(NetError::Io { during: "waiting for CharacterDeleted", detail: describe_proto_error(&source) }),
        Ok(Ok(msg)) => msg,
    };
    match answer {
        ServerMsg::CharacterDeleted { name } => Ok(DeleteOutcome::Deleted(clean(&name))),
        ServerMsg::Reject { code: RejectCode::InvalidToken, .. } => Ok(DeleteOutcome::AlreadyGone),
        ServerMsg::Reject { code, reason } => Err(NetError::Rejected { code, reason: clean(&reason) }),
        // Nothing else answers a delete request (and no server text is echoed in the error).
        _ => Err(NetError::Protocol("the server answered the delete request with something else".into())),
    }
}

/// [`delete_character`] over a new TCP connection to `server`: the connect timeout, then [`DELETE_TIMEOUT`] for the exchange.
pub async fn delete_on(server: &str, token: &Secret, client_build: &str) -> Result<DeleteOutcome, NetError> {
    let stream = connect(server).await?;
    let (mut rd, mut wr) = stream.into_split();
    delete_character(&mut rd, &mut wr, token, client_build, DELETE_TIMEOUT).await
}

/// [`delete_on`] for a thread that is not async: it runs on a small runtime made for the one call. Blocks until the
/// answer, a failure or the timeouts (at most the connect timeout plus [`DELETE_TIMEOUT`]).
pub fn delete_blocking(server: &str, token: &Secret, client_build: &str) -> Result<DeleteOutcome, NetError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| NetError::Runtime(format!("cannot start the async runtime: {e}")))?;
    runtime.block_on(delete_on(server, token, client_build))
}

/// What a failed delete says, in one sentence for the window and the terminal: which character was NOT deleted and why.
/// `name` is the character's display name, already cleaned. The server's own sentence of a refusal is quoted (it was
/// cleaned when it came in) and never decides anything; the advice is the caller's, from the refusal's code alone.
/// A character that is in the game right now has a sentence of its own (the refusal that matters to a player).
pub fn failure_text(name: &str, error: &NetError) -> String {
    match error {
        NetError::Rejected { code: RejectCode::AlreadyOnline, .. } => format!("{name} is in the game right now (from this or another computer)."),
        NetError::Rejected { reason, .. } => format!("{name} was NOT deleted and its saved login was kept. The server refused: {reason}"),
        other => format!("{name} was NOT deleted and its saved login was kept: {other}"),
    }
}

/// What the `--delete NAME` command works with, handed in so that the command is tested without a server or a terminal.
pub struct Command<'a> {
    /// The server, as `host:port` (the saved logins are per server).
    pub server: &'a str,
    /// The character to delete: a display name, matched against the saved logins of `server` like `--name` is.
    pub name: &'a str,
    pub store: &'a TokenStore,
    pub looks: &'a LookCache,
}

/// Run `--delete NAME`: see the module docs. `ask` is the network call (given the login's token; it is
/// [`delete_blocking`] in the real command), `out` is stdout (one line, on success only) and `err` is stderr. Returns
/// the exit code. Never prints the token.
pub fn run_command(cmd: &Command<'_>, ask: impl FnOnce(&Secret) -> Result<DeleteOutcome, NetError>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let account = match cmd.store.find(cmd.server, Some(cmd.name)) {
        Ok(Some(account)) => account,
        Ok(None) => {
            let _ = writeln!(
                err,
                "error: no login is saved for \"{}\" on {}, so there is nothing to delete with (a character can only be deleted with its own saved login)",
                clean(cmd.name),
                cmd.server
            );
            return exit::USAGE;
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return exit::FAILURE;
        }
    };
    if let Some(mode) = account.loose_mode {
        let _ = writeln!(err, "{}", loose_mode_warning(&account.path, mode));
    }
    let name = clean(&account.name);
    let _ = writeln!(err, "deleting the character \"{name}\" on {}...", cmd.server);
    match ask(&account.token) {
        Ok(outcome) => finish(cmd, &account, &name, &outcome, out, err),
        Err(error) => {
            let advice = match &error {
                NetError::Rejected { code: RejectCode::AlreadyOnline, .. } => " Leave the game first, then run --delete again.",
                _ => "",
            };
            let _ = writeln!(err, "error: {}{advice}", failure_text(&name, &error));
            exit::FAILURE
        }
    }
}

/// The server said the character is gone: remove the saved login and the note about its look, and say so on stdout.
fn finish(cmd: &Command<'_>, account: &SavedAccount, name: &str, outcome: &DeleteOutcome, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let line = match outcome {
        DeleteOutcome::Deleted(_) => format!("deleted character name={}", encode_name(&account.name)),
        DeleteOutcome::AlreadyGone => format!("character already gone on the server name={}", encode_name(&account.name)),
    };
    match cmd.store.forget(cmd.server, account) {
        Ok(()) => {
            cmd.looks.forget(cmd.server, &account.name);
            let _ = writeln!(out, "{line}");
            exit::OK
        }
        Err(e) => {
            // The server's side is done and the line says so; this machine still holds the login, which is the failure.
            let _ = writeln!(out, "{line}");
            let _ = writeln!(
                err,
                "error: the server no longer has {name}, but its saved login could not be removed from this computer ({e}); fix that file or delete it by hand, or run --delete again"
            );
            exit::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
