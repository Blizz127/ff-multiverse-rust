//! What Echo-S's archive holds and the names FFNx looks its files up by.
//!
//! **Inventory of `Echo-s.iro`** (version 1.24, 23 178 entries, 4 335 170 582 bytes; counts only; the archive's own
//! `mod.xml` says which folders are active under which option):
//!
//! | Folder | Entries | What | Used here |
//! |---|---|---|---|
//! | `Echo-S\flevel.lgp\<field>` | 706 | replacement field files, the same LZSS-compressed entries `flevel.lgp` holds: all 702 fields plus `las4_0.bak`, `mds7pb.tut`, `nothing.akao`, `startmap.png` | **script** |
//! | `VO\Voice\<field>\...` | 15 461 | field voices, `.ogg` (plus 100 stray `.sfk` peak files and one `.wav`) | **voice** |
//! | `EchoeS Of The Planet\vgmstream\<NAME>.ogg` | 112 | the music ("Echoes of the Planet", timed to the voices), named like `music.idx`'s tracks, and a 3-section `config.toml` | **music** |
//! | `Arranged\vgmstream\<name>.ogg` | 10 | arranged themes (also under the `Music` option) | **music** |
//! | `Sound\sfx\<n>.ogg` + `config.toml` | 881 | Echo-S's own sound effect set (ids 751 to 6724) and its 749-section map | sound, after Cosmo |
//! | `V_Attacks\sfx`, `NV_Attacks\sfx` | 155 each | party members' attack voices and the same sounds without voices | not reachable (see below) |
//! | `Battle VO\Voice`, `Battle VO_S\Voice`, `Prob\<n>\voice\config.toml` | 1 221, 1 158, 5 | battle lines and their play probabilities | not used |
//! | `NoCloud\cid`, `NoCloud\tifa` | 115 each | answer voices for when the lead is not Cloud | not used |
//! | `UI\Aavock`, `UI\ESUI`, `UI\FT`, `UI\Retouch` | 706 each | `flevel.lgp` variants for four window-UI mods, switched by a game-memory byte (`RuntimeVar`) | not used (the base `Echo-S\flevel.lgp` is) |
//! | `60fps\*`, `Echo-S\char.lgp`, `Echo-S\cr`, `Echo-S\menu`, `Echo-S\kernel`, `Echo-S\world`, `Echo-S\battle`, `Echo-S\hext`, `DayNight`, `Common`, `direct`, `Materia Tutorial`, `Preview` | 20, 134, 16, 34, 12, 2, 1, 4, 3, 21, 1, 1, 3 | models, textures, kernel and world-map text, hex patches, a day-night cycle, movie audio, previews | not used |
//!
//! The 60fps folders and the `UI\*` variants are not mentioned as folders in `mod.xml`'s unconditional list:
//! `60fps\*` is never referenced at all, and the four `UI\*` folders are `Conditional`s on a byte of the game's
//! memory that a window-UI mod sets.
//!
//! # Where the text lives
//!
//! In the replacement field files: `Echo-S\flevel.lgp\<field>` is a whole field entry (LZSS, then the nine-section
//! field file), and its script section carries Echo-S's dialog table **and its own script**. It is not the original
//! script with new strings: see [`crate::mods::Mods::script_section`] for the measurements and for why the section,
//! not a line table, is the unit.
//!
//! # Voice names
//!
//! FFNx plays a field voice from the `MESSAGE` or `ASK` that opens a window, by the script's own window and dialog
//! numbers. Pinned to FFNx commit `4fc392ba5e1bfdd53d39aa6542464379a446083a`,
//! [`src/voice.cpp`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/voice.cpp):
//! `play_voice` (lines 163 to 183) and `play_option` (320 to 327):
//!
//! ```text
//! page letter = 'a' + page (the page of the message the player has reached, 0 for the first), at most 'z'
//! 1. <field>/w<window>_<dialog><letter>
//! 2. <field>/<dialog><letter>
//! 3. (first page only) <field>/w<window>_<dialog>
//! 4. (first page only) <field>/<dialog>
//! option voice, played as the cursor of an ASK reaches a line: <field>/<dialog>_<line>   (the line is absolute)
//! ```
//!
//! In the archive 8 827 voice files are `<dialog>.ogg`, 1 425 are `<dialog>_<line>.ogg` (option voices), 21 are window-specific
//! and 5 061 carry a page letter. Of the 10 909 dialog numbers (over 547 fields) that have a clip, 8 799 have a plain one
//! only, **2 077 have lettered clips only** (`<dialog>a.ogg`, `<dialog>b.ogg`, ...: a lookup of `<dialog>.ogg` alone would
//! silently lose them) and 33 have both.

/// The folder of the replacement field entries.
pub const FLEVEL_DIR: &str = "Echo-S\\flevel.lgp\\";
/// The folder of the field voices.
pub const VOICE_DIR: &str = "VO\\Voice\\";
/// Music folders, in the order the mod lists them (the first with the file wins, as in 7th Heaven).
pub const MUSIC_DIRS: [&str; 2] = ["EchoeS Of The Planet\\vgmstream\\", "Arranged\\vgmstream\\"];
/// Echo-S's own sound effect folder.
pub const SOUND_DIR: &str = "Sound\\sfx\\";
/// The attack voices and their silent twins.
pub const VOICED_ATTACKS_DIR: &str = "V_Attacks\\sfx\\";
pub const PLAIN_ATTACKS_DIR: &str = "NV_Attacks\\sfx\\";
/// File extensions tried for a voice or a music clip, in order.
pub const CLIP_EXTENSIONS: [&str; 2] = ["ogg", "wav"];

/// The page letter of game page `page` (`a` for the first).
pub fn page_letter(page: usize) -> char {
    char::from(b'a'.saturating_add(u8::try_from(page.min(25)).unwrap_or(25)))
}

/// The names FFNx tries for the voice of `dialog` opened as window `window` and shown at game page `page`, in order, without
/// the folder or extension. `page` is the number of page breaks the player has passed in this message.
pub fn voice_names(window: u8, dialog: usize, page: usize) -> Vec<String> {
    let letter = page_letter(page);
    let mut names = vec![format!("w{window}_{dialog}{letter}"), format!("{dialog}{letter}")];
    if page == 0 {
        names.push(format!("w{window}_{dialog}"));
        names.push(dialog.to_string());
    }
    names
}

/// The name of the voice played when the cursor of the `ASK` of `dialog` reaches `line` (absolute).
pub fn option_voice_name(dialog: usize, line: usize) -> String {
    format!("{dialog}_{line}")
}

/// The archive path of voice `name` of `field` with `extension`.
pub fn voice_path(field: &str, name: &str, extension: &str) -> String {
    format!("{VOICE_DIR}{}\\{name}.{extension}", field.to_ascii_lowercase())
}

/// The archive path of the replacement entry of `field`.
pub fn flevel_path(field: &str) -> String {
    format!("{FLEVEL_DIR}{}", field.to_ascii_lowercase())
}

/// A voice clip's archive path as a log says it: `VO\Voice\md1stin\12a.ogg` is `md1stin/12a` (an id of a clip, never its words).
pub fn short_voice_name(path: &str) -> String {
    let rest = path.get(..VOICE_DIR.len()).filter(|p| p.eq_ignore_ascii_case(VOICE_DIR)).and_then(|_| path.get(VOICE_DIR.len()..)).unwrap_or(path);
    let stem = rest.rsplit_once('.').map_or(rest, |(stem, _)| stem);
    stem.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_tries_the_lettered_names_and_then_the_plain_ones() {
        assert_eq!(voice_names(0, 12, 0), ["w0_12a", "12a", "w0_12", "12"]);
        assert_eq!(voice_names(2, 5, 0), ["w2_5a", "5a", "w2_5", "5"]);
    }

    #[test]
    fn later_pages_try_only_the_lettered_names() {
        assert_eq!(voice_names(0, 12, 1), ["w0_12b", "12b"]);
        assert_eq!(voice_names(1, 3, 2), ["w1_3c", "3c"]);
    }

    #[test]
    fn the_page_letter_stops_at_z() {
        assert_eq!(page_letter(0), 'a');
        assert_eq!(page_letter(25), 'z');
        assert_eq!(page_letter(26), 'z');
        assert_eq!(page_letter(usize::MAX), 'z');
        assert_eq!(voice_names(0, 1, 100), ["w0_1z", "1z"]);
    }

    #[test]
    fn voice_names_are_short_in_a_log() {
        assert_eq!(short_voice_name("VO\\Voice\\md1stin\\12a.ogg"), "md1stin/12a");
        assert_eq!(short_voice_name("vo\\voice\\nmkin_5\\w1_3.OGG"), "nmkin_5/w1_3");
        assert_eq!(short_voice_name("elsewhere\\x.ogg"), "elsewhere/x");
        assert_eq!(short_voice_name(""), "");
    }

    #[test]
    fn paths_follow_the_archive_layout_in_lower_case() {
        assert_eq!(voice_path("MD1STIN", "12a", "ogg"), "VO\\Voice\\md1stin\\12a.ogg");
        assert_eq!(flevel_path("Md1Stin"), "Echo-S\\flevel.lgp\\md1stin");
        assert_eq!(option_voice_name(7, 2), "7_2");
    }
}
