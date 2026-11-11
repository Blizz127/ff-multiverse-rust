//! Live fields (protocol 23): what the server's field runtime says to this client, and what is owed back.
//!
//! On a live field the server runs the field's own scripts. Its cast replaces the area's NPCs (`ServerMsg::LiveCast`,
//! applied by the state like a snapshot's NPCs); the scripts' windows arrive as `LiveWindow` with the dialog's index
//! in this machine's own copy of the field; `LiveControl` holds the player still while a script runs a scene;
//! `LivePlace` puts the player where a script moved it; `LiveMovie` asks for a movie. Only the party leader's answers
//! count (`answerable`); everyone else watches.

use std::collections::BTreeMap;

use fmv_protocol::{ClientMsg, LiveAsk};

/// A window a field script opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveWindowView {
    pub window: u8,
    /// The dialog's index in the area's field script.
    pub dialog: u16,
    pub ask: Option<LiveAsk>,
    /// Whether this player's answer is the one that counts.
    pub answerable: bool,
    /// Which opening of a window this is (counts up in [`LiveClient::on_window`]): a script that opens the same dialog
    /// twice in a row still gets a fresh window each time.
    pub serial: u64,
    /// This window's answer (or dismissal) has been queued for the server; it stays shown until the server closes it.
    pub answered: bool,
}

impl LiveWindowView {
    pub fn new(window: u8, dialog: u16, ask: Option<LiveAsk>, answerable: bool) -> Self {
        Self { window, dialog, ask, answerable, serial: 0, answered: false }
    }
}

/// The live field's state on this side.
#[derive(Debug, Default)]
pub struct LiveClient {
    windows: BTreeMap<u8, LiveWindowView>,
    /// A script holds the player still.
    pub locked: bool,
    /// The movie the server asked for, while it plays.
    pub movie: Option<u16>,
    /// The server's field scripts run this area (a `LiveCast` has come since the area was entered): the scripts own talking.
    pub live_area: bool,
    opened: u64,
    outbox: Vec<ClientMsg>,
    notes: Vec<String>,
}

impl LiveClient {
    /// A `LiveCast` arrived: this area is a live field.
    pub fn on_cast(&mut self) {
        self.live_area = true;
    }

    pub fn on_window(&mut self, mut view: LiveWindowView) {
        self.opened += 1;
        view.serial = self.opened;
        view.answered = false;
        self.notes.push(format!(
            "live: window {} dialog {} ask {} answerable {}",
            view.window,
            view.dialog,
            view.ask.map_or_else(|| "-".to_string(), |a| format!("{}-{}", a.first, a.last)),
            view.answerable
        ));
        self.windows.insert(view.window, view);
    }

    pub fn on_window_closed(&mut self, window: u8) {
        self.windows.remove(&window);
    }

    pub fn on_control(&mut self, locked: bool) {
        self.locked = locked;
        self.notes.push(format!("live: control locked={locked}"));
    }

    pub fn on_movie(&mut self, movie: u16) {
        self.movie = Some(movie);
        self.notes.push(format!("live: movie {movie}"));
    }

    /// The windows open now, lowest number first.
    pub fn windows(&self) -> impl Iterator<Item = &LiveWindowView> {
        self.windows.values()
    }

    pub fn has_windows(&self) -> bool {
        !self.windows.is_empty()
    }

    /// The player dismissed window `window` (`None`) or answered its question with line `choice`, counted from its first
    /// choice. Only an answerable window's answer is sent; the window stays shown until the server closes it.
    pub fn answer(&mut self, window: u8, choice: Option<u8>) {
        let Some(view) = self.windows.get_mut(&window) else { return };
        if !view.answerable || view.answered {
            return;
        }
        view.answered = true;
        let view = *view;
        let choice = match (view.ask, choice) {
            (Some(ask), Some(c)) => Some(c.min(ask.last.saturating_sub(ask.first))),
            (Some(_), None) => Some(0),
            (None, _) => None,
        };
        self.outbox.push(ClientMsg::LiveAnswer { window, choice });
    }

    /// The movie is over (or cannot be played here).
    pub fn movie_done(&mut self) {
        if let Some(movie) = self.movie.take() {
            self.outbox.push(ClientMsg::LiveMovieDone { movie });
        }
    }

    /// What is owed the server, in order.
    pub fn take_outbox(&mut self) -> Vec<ClientMsg> {
        std::mem::take(&mut self.outbox)
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// A new area: nothing of the last field's windows or holds carries over.
    pub fn reset(&mut self) {
        self.windows.clear();
        self.locked = false;
        self.movie = None;
        self.live_area = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_leaders_answers_go_out_and_a_question_always_gets_one() {
        let mut live = LiveClient::default();
        live.on_window(LiveWindowView::new(1, 4, Some(LiveAsk { first: 1, last: 3 }), true));
        live.on_window(LiveWindowView::new(2, 5, None, false));
        live.answer(1, Some(9));
        live.answer(2, None);
        assert_eq!(live.take_outbox(), vec![ClientMsg::LiveAnswer { window: 1, choice: Some(2) }]);
        live.on_movie(53);
        live.movie_done();
        live.movie_done();
        assert_eq!(live.take_outbox(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
    }

    #[test]
    fn a_window_is_answered_once_and_a_reopened_one_can_be_answered_again() {
        let mut live = LiveClient::default();
        live.on_window(LiveWindowView::new(0, 7, None, true));
        let first = live.windows().next().unwrap().serial;
        live.answer(0, None);
        live.answer(0, None);
        assert_eq!(live.take_outbox(), vec![ClientMsg::LiveAnswer { window: 0, choice: None }]);
        assert!(live.windows().next().unwrap().answered, "it stays shown until the server closes it");
        // The same dialog opened again is a new window.
        live.on_window(LiveWindowView::new(0, 7, None, true));
        let view = *live.windows().next().unwrap();
        assert_ne!(view.serial, first);
        assert!(!view.answered);
        live.answer(0, None);
        assert_eq!(live.take_outbox().len(), 1);
    }

    #[test]
    fn a_cast_makes_a_live_area_until_the_area_changes() {
        let mut live = LiveClient::default();
        assert!(!live.live_area);
        live.on_cast();
        assert!(live.live_area);
        live.reset();
        assert!(!live.live_area);
    }
}
