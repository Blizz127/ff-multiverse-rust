//! `--talk NAME[,NAME...]` for a headless bot: talk to the named characters of the area through the SAME
//! conversation code the window uses ([`crate::ui::dialogue::ff7::Ff7Conversations`]: the character's own
//! talk script in this machine's own copy of the field, run by `fmv_ff7::script::dialogue::TalkSession`) and
//! print what each says, one `talk:` line on stderr per message window:
//!
//! ```text
//! talk: npc=<enc name> story=<moment|-> window=<n> origin=<canon|added> [ask=<first>-<last>] text=<enc text>
//! talk: npc=<enc name> story=<moment|-> ended=<Finished|Budget|Broken|MapJump|Battle|MiniGame|GameOver|Limit> windows=<n>
//! talk: npc=<enc name> story=<moment|-> nothing to say
//! talk: npc=<enc name> was never in the area
//! ```
//!
//! `story=` is the player's own story counter (FF7's `Var[2][0]`) that the conversation was read with: the
//! account's progression as the server last told this client, and nothing about who the player looks like or
//! which combat slot it fills, so a conversation is the game's own whoever plays. The text is
//! percent-encoded like every `key=value` field of the headless output (`headless::encode_name`); its pages
//! are joined with ` // ` and its lines with one space. A question (`ask=`) is answered with the cursor's
//! starting line, as a player who presses the confirm key at once.
//!
//! The bot does not walk up to the character: it asks the conversation code for the talk script directly, as
//! soon as the area announces the character (the window's proximity rules, talk radius and facing, are the
//! window's and are tested there). A character the area never announced (or that has no talk script: the
//! stand-in of a playable character, which is not a script entity) is reported as it is.

use crate::headless::encode_name;
use crate::state::NpcView;
use crate::ui::dialogue::ff7::StoryFactory;
use crate::ui::dialogue::Conversations;
use fmv_ff7::script::dialogue::{Beat, End, Message, MessageOrigin};
use fmv_ff7::story::StoryState;

/// The most windows one conversation prints (a script that asks for more is cut off: `ended=Limit`).
pub const MAX_WINDOWS: usize = 32;

/// The characters to talk to, and what they have said.
pub struct TalkBot {
    wanted: Vec<String>,
    done: Vec<bool>,
    conversations: Box<dyn Conversations>,
}

impl TalkBot {
    /// A bot that talks to `wanted` (by the name the area announces them under) through `conversations`.
    pub fn new(wanted: &[String], conversations: Box<dyn Conversations>) -> Self {
        Self { wanted: wanted.to_vec(), done: vec![false; wanted.len()], conversations }
    }

    /// Whether `--talk` was given.
    pub fn is_active(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// Whether every wanted character has been talked to.
    pub fn is_done(&self) -> bool {
        self.done.iter().all(|d| *d)
    }

    /// Talk to every wanted character that `npcs` (the area `area` announces) now contains and that has not
    /// been talked to yet; the lines to print. `moment` is the player's story counter (`None`: the
    /// progression is not FF7's: the conversation is read as at the start of a new game).
    pub fn step(&mut self, area: &str, npcs: &[NpcView], moment: Option<u16>) -> Vec<String> {
        self.step_with(area, npcs, moment.map(StoryState::at))
    }

    /// [`TalkBot::step`] with the player's whole story state: the moment and the variables beyond it that the story
    /// model carries (what the elevator's ride set), which a conversation's branches read through the same hook.
    pub fn step_with(&mut self, area: &str, npcs: &[NpcView], story: Option<StoryState>) -> Vec<String> {
        let moment = story.as_ref().map(|s| s.moment);
        let mut lines = Vec::new();
        for (index, name) in self.wanted.iter().enumerate() {
            if self.done.get(index).copied().unwrap_or(true) {
                continue;
            }
            let Some(npc) = npcs.iter().find(|n| &n.name == name) else { continue };
            if let Some(done) = self.done.get_mut(index) {
                *done = true;
            }
            self.conversations.set_story(story_of(story.clone()));
            lines.extend(converse(self.conversations.as_mut(), area, npc, moment));
        }
        lines
    }

    /// The lines for the characters that were never talked to (the area never announced them).
    pub fn finish(&self) -> Vec<String> {
        self.wanted
            .iter()
            .zip(&self.done)
            .filter(|(_, done)| !**done)
            .map(|(name, _)| format!("talk: npc={} was never in the area", encode_name(name)))
            .collect()
    }
}

/// The story hook for a conversation: the player's own story state, or a new game's.
fn story_of(state: Option<StoryState>) -> StoryFactory {
    let state = state.unwrap_or_else(StoryState::new_game);
    Box::new(move || Box::new(state.clone()))
}

/// What the story column of a line says.
fn story_label(moment: Option<u16>) -> String {
    moment.map_or_else(|| "-".to_string(), |m| m.to_string())
}

/// One whole conversation with `npc`, as lines.
fn converse(conversations: &mut dyn Conversations, area: &str, npc: &NpcView, moment: Option<u16>) -> Vec<String> {
    let (name, story) = (encode_name(&npc.name), story_label(moment));
    let nothing = || vec![format!("talk: npc={name} story={story} nothing to say")];
    let Some(mut source) = conversations.start(area, npc) else { return nothing() };
    let mut lines = Vec::new();
    let mut windows = 0_usize;
    let ended = loop {
        if windows >= MAX_WINDOWS {
            break "Limit".to_string();
        }
        match source.next(None) {
            Beat::Message(message) => {
                lines.push(window_line(&name, &story, windows, &message));
                windows += 1;
            }
            Beat::End(end) => break end_label(end).to_string(),
        }
    };
    if windows == 0 {
        return nothing();
    }
    lines.push(format!("talk: npc={name} story={story} ended={ended} windows={windows}"));
    lines
}

/// The line of one message window.
fn window_line(name: &str, story: &str, window: usize, message: &Message) -> String {
    let origin = match message.origin {
        MessageOrigin::Canon { .. } => "canon",
        MessageOrigin::Added { .. } => "added",
    };
    let ask = message.choice.map(|c| format!(" ask={}-{}", c.first, c.last)).unwrap_or_default();
    format!("talk: npc={name} story={story} window={window} origin={origin}{ask} text={}", encode_name(&text_of(message)))
}

/// The words of a message: its pages joined with ` // `, the lines of a page with one space, no colours or
/// choice markers (`Message::page_lines`).
fn text_of(message: &Message) -> String {
    message
        .page_lines()
        .iter()
        .map(|page| page.iter().map(|line| line.trim()).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" "))
        .filter(|page| !page.is_empty())
        .collect::<Vec<_>>()
        .join(" // ")
}

fn end_label(end: End) -> &'static str {
    match end {
        End::Finished => "Finished",
        End::Budget => "Budget",
        End::Broken => "Broken",
        End::MapJump { .. } => "MapJump",
        End::Battle => "Battle",
        End::MiniGame => "MiniGame",
        End::GameOver => "GameOver",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::Pos;
    use fmv_ff7::script::dialogue::{Choice, MessageSource, Piece};
    use std::collections::VecDeque;

    /// A canned conversation: the beats it gives, in order (then `End::Finished`).
    struct Canned(VecDeque<Beat>);

    impl MessageSource for Canned {
        fn next(&mut self, _answer: Option<usize>) -> Beat {
            self.0.pop_front().unwrap_or(Beat::End(End::Finished))
        }
    }

    /// Conversations by NPC name.
    struct Fake {
        beats: Vec<(&'static str, Vec<Beat>)>,
    }

    impl Conversations for Fake {
        fn talk_radius(&mut self, _area: &str, _npc: &NpcView) -> f32 {
            50.0
        }

        fn start(&mut self, _area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>> {
            let (_, beats) = self.beats.iter().find(|(n, _)| *n == npc.name)?;
            Some(Box::new(Canned(beats.clone().into())))
        }
    }

    fn npc(name: &str) -> NpcView {
        NpcView { id: 0, name: name.to_string(), model: String::new(), idle_animation: None, pos: Pos::new(0.0, 0.0, 0.0), facing: 0.0 }
    }

    fn say(text: &str) -> Beat {
        let mut pieces = Vec::new();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                pieces.push(Piece::NewLine);
            }
            pieces.push(Piece::Text(line.to_string()));
        }
        Beat::Message(Message::new(MessageOrigin::Canon { entity: 3, dialog: 1 }, pieces))
    }

    fn bot(beats: Vec<(&'static str, Vec<Beat>)>, wanted: &[&str]) -> TalkBot {
        let wanted: Vec<String> = wanted.iter().map(ToString::to_string).collect();
        TalkBot::new(&wanted, Box::new(Fake { beats }))
    }

    #[test]
    fn a_character_with_words_gets_one_line_per_window_and_an_end_line() {
        let mut talk = bot(vec![("gu0", vec![say("Hello, mister.\nTake this."), say("Bye.")])], &["gu0"]);
        let lines = talk.step("md1stin", &[npc("gu0"), npc("gu1")], Some(1));
        assert_eq!(
            lines,
            vec![
                "talk: npc=gu0 story=1 window=0 origin=canon text=Hello%2C%20mister.%20Take%20this.".to_string(),
                "talk: npc=gu0 story=1 window=1 origin=canon text=Bye.".to_string(),
                "talk: npc=gu0 story=1 ended=Finished windows=2".to_string(),
            ]
        );
        assert!(talk.is_done());
        // It talks once: the same area announced again says nothing more.
        assert!(talk.step("md1stin", &[npc("gu0")], Some(1)).is_empty());
    }

    #[test]
    fn a_character_without_a_talk_script_has_nothing_to_say() {
        let mut talk = bot(vec![], &["Barret"]);
        assert_eq!(talk.step("md1stin", &[npc("Barret")], None), vec!["talk: npc=Barret story=- nothing to say".to_string()]);
        // A script that ends at once has nothing to say either.
        let mut talk = bot(vec![("gu1", vec![])], &["gu1"]);
        assert_eq!(talk.step("md1stin", &[npc("gu1")], Some(6)), vec!["talk: npc=gu1 story=6 nothing to say".to_string()]);
    }

    #[test]
    fn a_character_the_area_never_announces_is_reported_at_the_end_and_not_before() {
        let mut talk = bot(vec![("gu0", vec![say("Hi")])], &["gu0", "Ghost"]);
        assert!(talk.is_active());
        let lines = talk.step("md1stin", &[npc("gu0")], Some(1));
        assert_eq!(lines.len(), 2);
        assert!(!talk.is_done());
        assert_eq!(talk.finish(), vec!["talk: npc=Ghost was never in the area".to_string()]);
    }

    #[test]
    fn a_question_is_marked_and_answered_with_the_cursor() {
        let mut ask = Message::new(MessageOrigin::Canon { entity: 1, dialog: 4 }, vec![Piece::Text("Well?".into()), Piece::NewLine, Piece::Choice, Piece::Text("Yes".into())]);
        ask.choice = Some(Choice { first: 1, last: 2, cursor: 1 });
        let mut talk = bot(vec![("gu0", vec![Beat::Message(ask)])], &["gu0"]);
        let lines = talk.step("md1stin", &[npc("gu0")], Some(1));
        assert_eq!(lines.first().map(String::as_str), Some("talk: npc=gu0 story=1 window=0 origin=canon ask=1-2 text=Well%3F%20Yes"));
    }

    #[test]
    fn an_added_line_never_passes_for_canon() {
        let added = Beat::Message(Message::new(MessageOrigin::Added { layer: "showcase".into() }, vec![Piece::Text("Hi".into())]));
        let mut talk = bot(vec![("gu0", vec![added])], &["gu0"]);
        let lines = talk.step("md1stin", &[npc("gu0")], Some(1));
        assert!(lines[0].contains(" origin=added "), "{lines:?}");
    }

    #[test]
    fn a_conversation_that_never_ends_is_cut_off_at_the_limit() {
        struct Endless;
        impl MessageSource for Endless {
            fn next(&mut self, _answer: Option<usize>) -> Beat {
                say("again")
            }
        }
        struct Chatty;
        impl Conversations for Chatty {
            fn talk_radius(&mut self, _area: &str, _npc: &NpcView) -> f32 {
                1.0
            }
            fn start(&mut self, _area: &str, _npc: &NpcView) -> Option<Box<dyn MessageSource>> {
                Some(Box::new(Endless))
            }
        }
        let mut talk = TalkBot::new(&["gu0".to_string()], Box::new(Chatty));
        let lines = talk.step("md1stin", &[npc("gu0")], Some(1));
        assert_eq!(lines.len(), MAX_WINDOWS + 1);
        assert_eq!(lines.last().map(String::as_str), Some(format!("talk: npc=gu0 story=1 ended=Limit windows={MAX_WINDOWS}").as_str()));
    }

    #[test]
    fn the_story_a_conversation_is_read_with_is_the_players_own() {
        let mut talk = bot(vec![("gu0", vec![say("x")])], &["gu0"]);
        talk.step("md1stin", &[npc("gu0")], Some(6));
        // The factory is the player's moment, not the start of a new game.
        let vars = (story_of(Some(StoryState::at(6))))();
        assert_eq!(vars.byte(2, 0), 6);
        let vars = (story_of(None))();
        assert_eq!(vars.byte(2, 0), 1);
    }

    #[test]
    fn what_the_story_set_beyond_the_moment_reaches_a_conversation_too() {
        // After the elevator's ride the lift bit is on in the account's story, and a conversation's branches read it.
        let mut ridden = StoryState::at(12);
        assert!(ridden.set_bit(1, 225, 0, true));
        let vars = (story_of(Some(ridden.clone())))();
        assert_eq!((vars.byte(2, 0), vars.byte(1, 225), vars.byte(1, 224)), (12, 1, 0));
        // the walker asks the factory for a fresh state every time, so one conversation cannot change the next
        assert_eq!((story_of(Some(ridden))().byte(1, 225), story_of(None)().byte(1, 225)), (1, 0));
        // a bot given the whole state talks as at the moment in it, and prints that moment
        let mut talk = bot(vec![("gu0", vec![say("x")])], &["gu0"]);
        let mut state = StoryState::at(12);
        state.set_bit(1, 225, 0, true);
        let lines = talk.step_with("md1stin", &[npc("gu0")], Some(state));
        assert!(lines.iter().all(|l| l.contains("story=12")), "{lines:?}");
    }
}
