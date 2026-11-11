//! Windows of a live field (protocol 23, see `crate::live`): the server's scripts open them, this machine's own copy of
//! the field says what is written in them.
//!
//! `LiveWindow` names the window and the dialog (an index in the area's ORIGINAL field script table: the server runs
//! the original's scripts, so Echo-S's numbering never applies here) and, for a question, the lines that can be
//! picked. [`message_for`] turns that and the dialog's pieces into one [`Message`]; [`LiveSource`] is the
//! [`MessageSource`] a [`super::DialogueWindow`] pulls it from: that one message, then the end. How the window ended
//! (dismissed, or the line picked) is read back from the shared [`Outcome`] to answer the server.

use super::layout::is_blank;
use crate::mods::{ModSink, VoiceCue};
use fmv_ff7::script::dialogue::{Beat, Choice, End, Message, MessageOrigin, MessageSource, Piece};
use fmv_protocol::LiveAsk;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Where a live window's voice goes: the area it is in (voice files are named by it) and the sink the cues are sent to.
#[derive(Clone)]
pub struct LiveVoice {
    pub field: String,
    pub sink: Arc<dyn ModSink>,
}

/// A live window's message, and the voice that goes with it when the text is Echo-S's.
pub struct LiveText {
    pub message: Message,
    pub voice: Option<LiveVoice>,
}

/// How a live window was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    /// Still on show.
    #[default]
    Open,
    /// Dismissed (a plain window's last page confirmed).
    Dismissed,
    /// A question answered with this line, counted as the script counts them (from the first line of the page).
    Answered(usize),
}

/// One message, then the end; it writes how the window was left into `outcome`.
pub struct LiveSource {
    message: Option<Message>,
    outcome: Rc<Cell<Outcome>>,
    voice: Option<LiveVoice>,
}

impl LiveSource {
    pub fn new(message: Message, outcome: Rc<Cell<Outcome>>) -> Self {
        Self { message: Some(message), outcome, voice: None }
    }

    /// The same source, cueing the voice of the message's first page when it is given out (a window that shows nothing has none)
    /// and a stop when it is dropped, as a voiced conversation does.
    pub fn with_voice(mut self, voice: Option<LiveVoice>) -> Self {
        self.voice = voice;
        self
    }
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        if let Some(voice) = &self.voice {
            voice.sink.cue(VoiceCue::Stop);
        }
    }
}

impl MessageSource for LiveSource {
    fn next(&mut self, answer: Option<usize>) -> Beat {
        if let Some(message) = self.message.take() {
            if let (Some(voice), MessageOrigin::Canon { dialog, .. }, false) = (&self.voice, &message.origin, is_blank(&message)) {
                voice.sink.cue(VoiceCue::Say { field: voice.field.clone(), window: message.window, dialog: *dialog, page: 0 });
            }
            return Beat::Message(message);
        }
        self.outcome.set(answer.map_or(Outcome::Dismissed, Outcome::Answered));
        Beat::End(End::Finished)
    }
}

/// The lines on the last page of `pieces`: what a question's line numbers refer to.
fn last_page_lines(pieces: &[Piece]) -> usize {
    pieces.iter().fold(1usize, |lines, piece| match piece {
        Piece::NewPage => 1,
        Piece::NewLine => lines.saturating_add(1),
        _ => lines,
    })
}

/// The message of dialog `dialog` of the area, window number `window`, from its `pieces`, with its question (`ask`) if
/// it is one. The question is read as the server's script runner reads an `ASK`: a last line the text does not have is
/// the text's last line, and a first line it does not have makes a plain window.
pub fn message_for(dialog: usize, window: u8, pieces: Vec<Piece>, ask: Option<LiveAsk>) -> Message {
    let lines = last_page_lines(&pieces);
    let choice = ask.and_then(|a| {
        let first = usize::from(a.first);
        let last = usize::from(a.last).max(first).min(lines.saturating_sub(1));
        (first <= last).then_some(Choice { first, last, cursor: first })
    });
    let mut message = Message::new(MessageOrigin::Canon { entity: 0, dialog }, pieces);
    message.window = window;
    message.choice = choice;
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Piece {
        Piece::Text(s.to_string())
    }

    /// "Go?" then two choice lines.
    fn question() -> Vec<Piece> {
        vec![text("Go?"), Piece::NewLine, Piece::Choice, text("Yes"), Piece::NewLine, Piece::Choice, text("No")]
    }

    #[test]
    fn a_live_source_gives_its_one_message_then_ends_and_says_how() {
        let outcome = Rc::new(Cell::new(Outcome::Open));
        let mut source = LiveSource::new(message_for(3, 1, vec![text("Hi")], None), Rc::clone(&outcome));
        let Beat::Message(m) = source.next(None) else { panic!("the message comes first") };
        assert_eq!((m.window, m.choice, m.page_lines()), (1, None, vec![vec!["Hi".to_string()]]));
        assert!(matches!(m.origin, MessageOrigin::Canon { dialog: 3, .. }));
        assert_eq!(outcome.get(), Outcome::Open);
        assert!(matches!(source.next(None), Beat::End(_)));
        assert_eq!(outcome.get(), Outcome::Dismissed);
        assert!(matches!(source.next(None), Beat::End(_)), "and stays ended");
    }

    #[test]
    fn an_answer_is_the_line_the_window_gave_back() {
        let outcome = Rc::new(Cell::new(Outcome::Open));
        let mut source = LiveSource::new(message_for(0, 0, question(), Some(LiveAsk { first: 1, last: 2 })), Rc::clone(&outcome));
        let Beat::Message(m) = source.next(None) else { panic!() };
        assert_eq!(m.choice, Some(Choice { first: 1, last: 2, cursor: 1 }));
        assert!(matches!(source.next(Some(2)), Beat::End(_)));
        assert_eq!(outcome.get(), Outcome::Answered(2));
    }

    #[test]
    fn the_ask_range_is_read_as_the_server_reads_it() {
        // A last line the text lacks is the text's last line.
        let m = message_for(0, 0, question(), Some(LiveAsk { first: 1, last: 9 }));
        assert_eq!(m.choice, Some(Choice { first: 1, last: 2, cursor: 1 }));
        // A first line the text lacks is no question at all.
        assert_eq!(message_for(0, 0, question(), Some(LiveAsk { first: 7, last: 9 })).choice, None);
        // Choices are counted on the LAST page.
        let mut paged = vec![text("one"), Piece::NewPage];
        paged.extend(question());
        assert_eq!(message_for(0, 0, paged, Some(LiveAsk { first: 1, last: 2 })).choice, Some(Choice { first: 1, last: 2, cursor: 1 }));
        assert_eq!(message_for(0, 0, vec![text("x")], None).choice, None);
    }
}
