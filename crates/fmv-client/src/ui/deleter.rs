//! Deleting a character without freezing the menu.
//!
//! [`delete_blocking`] waits for the server (up to the connect and exchange timeouts), which is fine for the command line
//! but would freeze the window. A [`Deleter`] runs it on a helper thread with a small runtime of its own, and the window
//! polls for the outcome each frame, the way it polls the [`Connector`](super::connect::Connector) it is modelled on.
//!
//! A delete the player gives up waiting for is simply dropped: the helper thread still finishes its exchange (a request
//! that was asked for is the server's to carry out), but nobody hears the answer, so the saved login is kept, because it
//! is only ever removed when the answer arrives. Asking again settles it: the server answers `AlreadyGone` when the first
//! request had worked.

use crate::delete::{delete_blocking, DeleteOutcome};
use crate::net::NetError;
use fmv_protocol::Secret;
use std::sync::mpsc::{self, TryRecvError};

/// How a delete ended: what the server said, or why nothing was deleted.
pub type Outcome = Result<DeleteOutcome, NetError>;

/// A delete in progress.
pub struct Deleter {
    rx: mpsc::Receiver<Outcome>,
    /// The outcome was already handed out (or the helper died without one).
    done: bool,
}

impl Deleter {
    /// Start deleting the character of `token` on `server`. `Err` is a message for the screen (the thread could not be
    /// started).
    pub fn start(server: String, token: Secret, client_build: String) -> Result<Self, String> {
        Self::start_with(move || delete_blocking(&server, &token, &client_build))
    }

    /// Run `job` on a helper thread (the seam of [`Deleter::start`], so that anything can stand in for the network).
    pub fn start_with(job: impl FnOnce() -> Outcome + Send + 'static) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("fmv-delete".into())
            .spawn(move || {
                // Nobody may be listening any more (the player gave up): the answer then goes nowhere.
                let _ = tx.send(job());
            })
            .map_err(|e| format!("cannot start the delete thread: {e}"))?;
        Ok(Self { rx, done: false })
    }

    /// The outcome, once there is one (never blocks). Handed out exactly once.
    pub fn poll(&mut self) -> Option<Outcome> {
        if self.done {
            return None;
        }
        match self.rx.try_recv() {
            Ok(outcome) => {
                self.done = true;
                Some(outcome)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err(NetError::Runtime("the delete thread ended unexpectedly".into())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testkit::{serve_delete, serve_delete_gated, DeleteReply};
    use fmv_protocol::{ClientMsg, RejectCode, PROTOCOL_VERSION};
    use std::time::{Duration, Instant};

    fn wait(deleter: &mut Deleter) -> Outcome {
        let start = Instant::now();
        loop {
            if let Some(outcome) = deleter.poll() {
                return outcome;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "no outcome");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn request(token: &str) -> ClientMsg {
        ClientMsg::DeleteCharacter { protocol_version: PROTOCOL_VERSION, client_build: "test-build".into(), token: Secret::new(token) }
    }

    #[test]
    fn the_outcome_is_handed_out_exactly_once_and_polling_never_blocks() {
        let (release, gate) = mpsc::channel::<()>();
        let mut deleter = Deleter::start_with(move || {
            let _ = gate.recv_timeout(Duration::from_secs(20));
            Ok(DeleteOutcome::Deleted("Aria".into()))
        })
        .expect("a thread");
        // The job is held: polling answers at once, with nothing, again and again.
        for _ in 0..50 {
            assert!(deleter.poll().is_none());
        }
        release.send(()).expect("release the job");
        assert_eq!(wait(&mut deleter).expect("deleted"), DeleteOutcome::Deleted("Aria".into()));
        assert!(deleter.poll().is_none(), "the outcome is handed out exactly once");
        assert!(deleter.poll().is_none());
    }

    #[test]
    fn a_failure_is_an_outcome_too() {
        let mut deleter = Deleter::start_with(|| Err(NetError::Timeout("CharacterDeleted"))).expect("a thread");
        assert!(matches!(wait(&mut deleter), Err(NetError::Timeout("CharacterDeleted"))));
        assert!(deleter.poll().is_none());
    }

    #[test]
    fn a_helper_that_dies_without_an_answer_is_a_failure_and_not_a_hang() {
        let mut deleter = Deleter::start_with(|| panic!("the helper fell over (expected in this test)")).expect("a thread");
        match wait(&mut deleter) {
            Err(NetError::Runtime(why)) => assert!(why.contains("ended unexpectedly"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(deleter.poll().is_none(), "and that is said once too");
    }

    #[test]
    fn it_asks_the_server_on_its_own_connection_and_hands_out_the_answer() {
        let (addr, server) = serve_delete(DeleteReply::Deleted("Aria"));
        let mut deleter = Deleter::start(addr, Secret::new("fmvt_tok"), "test-build".into()).expect("a thread");
        assert_eq!(wait(&mut deleter).expect("deleted"), DeleteOutcome::Deleted("Aria".into()));
        assert!(deleter.poll().is_none());
        let seen = server.join().expect("the fake server");
        assert_eq!(seen.first, Some(request("fmvt_tok")), "the request is the first frame");
        assert_eq!(seen.after, 0);

        let (addr, server) = serve_delete(DeleteReply::Reject(RejectCode::InvalidToken, "invalid or unknown token"));
        let mut deleter = Deleter::start(addr, Secret::new("fmvt_tok"), "test-build".into()).expect("a thread");
        assert_eq!(wait(&mut deleter).expect("already gone"), DeleteOutcome::AlreadyGone);
        drop(server.join());
    }

    #[test]
    fn a_refused_connection_comes_back_as_a_failure() {
        // Nothing listens on this port any more.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        drop(listener);
        let mut deleter = Deleter::start(addr, Secret::new("fmvt_tok"), "test-build".into()).expect("a thread");
        assert!(matches!(wait(&mut deleter), Err(NetError::Connect { .. })));
        assert!(deleter.poll().is_none(), "the outcome is handed out exactly once");
    }

    #[test]
    fn a_delete_that_was_given_up_on_still_reaches_the_server() {
        // The server holds its answer until the test lets it go; the player gives up in the meantime.
        let (release, gate) = mpsc::channel::<()>();
        let (addr, server) = serve_delete_gated(DeleteReply::Deleted("Aria"), Some(gate));
        let deleter = Deleter::start(addr, Secret::new("fmvt_tok"), "test-build".into()).expect("a thread");
        drop(deleter);
        release.send(()).expect("release the server");
        let seen = server.join().expect("the fake server");
        assert_eq!(seen.first, Some(request("fmvt_tok")), "the request was sent and is the server's to carry out");
    }
}
