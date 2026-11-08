//! Connecting without freezing the menu.
//!
//! [`NetHandle::wait_joined`] blocks until the handshake is over (up to the connect and handshake
//! timeouts), which is fine for the command line but would freeze the menu window. A
//! [`Connector`] runs that wait on a small helper thread that owns the handle, and the window polls
//! for the outcome each frame.
//!
//! An attempt the player abandons is simply dropped: if it still finishes with a join, the helper
//! thread sends `Leave` itself, so no ghost player is left standing in the area. A registration
//! that is abandoned still saves its token (that happens inside the network thread the moment the
//! server's `Welcome` arrives), so the new character shows up in the list.

use crate::net::{Joined, NetConfig, NetError, NetHandle};
use fmv_adapter_api::GameAdapter;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

/// How long an abandoned join waits for its `Leave` to be flushed.
const LEAVE_WAIT: Duration = Duration::from_secs(1);

/// A finished handshake: the handle (now serving the session) and what the server agreed.
pub struct Connected {
    pub net: NetHandle,
    pub joined: Joined,
}

pub enum Outcome {
    Joined(Box<Connected>),
    Failed(NetError),
}

/// A connection attempt in progress.
pub struct Connector {
    rx: mpsc::Receiver<Outcome>,
    /// The outcome was already handed out (or the helper died without one).
    done: bool,
}

impl Connector {
    /// Start connecting: the network thread connects and runs the handshake, a helper thread
    /// waits for it. `Err` is a message for the screen (the threads could not be started).
    pub fn start(cfg: NetConfig, adapter: Box<dyn GameAdapter>) -> Result<Self, String> {
        let mut net = NetHandle::spawn(cfg, adapter).map_err(|e| format!("cannot start the network thread: {e}"))?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("fmv-connect".into())
            .spawn(move || {
                let outcome = match net.wait_joined() {
                    Ok(joined) => Outcome::Joined(Box::new(Connected { net, joined })),
                    Err(e) => Outcome::Failed(e),
                };
                if let Err(mpsc::SendError(Outcome::Joined(connected))) = tx.send(outcome) {
                    // Nobody is waiting any more: say goodbye instead of leaving a ghost player.
                    let Connected { mut net, .. } = *connected;
                    net.leave_and_join(LEAVE_WAIT);
                }
            })
            .map_err(|e| format!("cannot start the connection thread: {e}"))?;
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
                Some(Outcome::Failed(NetError::Runtime("the connection thread ended unexpectedly".into())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{SessionTiming, TokenSink};
    use crate::ui::testkit::{serve_join, Reply};
    use fmv_core::EntityId;
    use fmv_fixture::FixtureAdapter;
    use fmv_protocol::{Auth, Secret};
    use std::net::TcpListener;
    use std::time::Instant;

    fn cfg(server: String, auth: Auth) -> NetConfig {
        NetConfig {
            server,
            auth,
            tint: [1, 2, 3],
            appearance_ref: None,
            client_build: "test".into(),
            timing: SessionTiming::DEFAULT,
            token_sink: TokenSink::new(|_| Ok(())),
        }
    }

    fn wait(c: &mut Connector) -> Outcome {
        let start = Instant::now();
        loop {
            if let Some(o) = c.poll() {
                return o;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "no outcome");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_refused_connection_comes_back_as_a_failure() {
        // Nothing listens on this port any more.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let mut c = Connector::start(cfg(addr, Auth::Register { display_name: "x".into() }), Box::new(FixtureAdapter::new())).unwrap();
        match wait(&mut c) {
            Outcome::Failed(NetError::Connect { .. }) => {}
            Outcome::Failed(other) => panic!("{other}"),
            Outcome::Joined(_) => panic!("nothing listens there"),
        }
        assert!(c.poll().is_none(), "the outcome is handed out exactly once");
    }

    #[test]
    fn a_join_hands_over_the_running_session() {
        let (addr, server) = serve_join(Reply::Join { stored: None, token: None, gate: None });
        let mut c = Connector::start(cfg(addr, Auth::Token(Secret::new("tok"))), Box::new(FixtureAdapter::new())).unwrap();
        let connected = match wait(&mut c) {
            Outcome::Joined(c) => *c,
            Outcome::Failed(e) => panic!("{e}"),
        };
        assert_eq!(connected.joined.entity, EntityId(7));
        assert!(connected.joined.fixture);
        // The handle is live: leaving makes the server see a Leave.
        let mut net = connected.net;
        net.leave_and_join(Duration::from_secs(2));
        assert!(server.join().unwrap().left, "the server saw Leave");
    }

    #[test]
    fn an_abandoned_attempt_that_still_joins_says_goodbye() {
        let (addr, server) = serve_join(Reply::Join { stored: None, token: None, gate: None });
        let c = Connector::start(cfg(addr, Auth::Token(Secret::new("tok"))), Box::new(FixtureAdapter::new())).unwrap();
        // The player pressed Escape: nobody will ever poll this attempt.
        drop(c);
        assert!(server.join().unwrap().left, "the helper thread sent Leave for the abandoned join");
    }
}
