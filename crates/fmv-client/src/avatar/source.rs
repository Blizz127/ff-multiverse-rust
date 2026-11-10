//! Where this machine's character archive (`char.lgp`) is, and how a character is read from it.
//!
//! The models are never sent over the network: every client reads its own copy of the game's
//! field character archive, found by `fmv_library::ff7::resolve_char_lgp` from the same library set
//! (or the same directory) as the field archive. [`CharacterLoader`] is the seam between the
//! avatar cache and that file, so the cache can be tested with a stand-in that counts its calls.
//!
//! # The process-wide slot
//!
//! [`ff7::open_ff7`](crate::ff7::open_ff7) runs once at start-up, long before any window exists, and
//! the window code that wants models sits several layers away (the character menu, the session
//! hand-over). Instead of threading a path through all of those, `open_ff7` leaves the location in
//! this write-once-per-open slot ([`remember`]) and the window asks for it ([`remembered`]). Only
//! that glue reads the slot: the cache, the loader and every test take the location (or a stand-in
//! loader) explicitly and never touch it.

use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::model::{load_field_animation, load_field_model, ModelError};
use fmv_ff7::playable::{load_playable, Character, PlayableModel};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::Mutex;

/// The `appearance` of a [`Character`] that is not one of the playable ones (a field's non-player
/// character): it names no entry of the table of playable characters.
pub const NON_PLAYABLE: PlayableModel = PlayableModel {
    appearance_ref: "npc",
    display_name: "Non-player character",
    hrc: "",
    idle_anim: "",
    walk_anim: "",
    run_anim: "",
};

/// Reads characters: the playable ones by table entry, the non-player ones by the references an
/// area's data holds.
pub trait CharacterLoader {
    /// The character for `appearance`, or why it cannot be had (shown on stderr, once).
    fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String>;

    /// The non-player character whose skeleton is `model` (`<model>.hrc`), shown standing with the
    /// clip `idle` (`<idle>.a`): a [`Character`] whose three clips are all that standing clip (it
    /// is never walked or run). Both are untrusted text, only ever looked up as archive entries.
    /// The default has no such models.
    fn load_npc(&mut self, model: &str, idle: &str) -> Result<Character, String> {
        let _ = (model, idle);
        Err("this loader has no non-player models".to_string())
    }
}

/// A non-player character standing: the skeleton `model` with the clip `idle`, which must fit it.
pub fn load_standing<R: Read + Seek>(lgp: &mut LgpArchive<R>, model: &str, idle: &str) -> Result<Character, ModelError> {
    let skeleton = load_field_model(lgp, model)?;
    let clip = load_field_animation(lgp, idle)?;
    if !skeleton.fits(&clip) {
        return Err(ModelError::Invalid {
            what: "non-player model",
            why: format!("the clip drives {} bones but the skeleton has {}", clip.bone_count(), skeleton.bones.len()),
        });
    }
    Ok(Character { appearance: NON_PLAYABLE, model: skeleton, idle: clip.clone(), walk: clip.clone(), run: clip })
}

/// Reads characters from a `char.lgp` on disk, read-only. The archive is opened (its table of
/// contents read) by the first character asked for and kept open after that.
pub struct LgpLoader {
    path: PathBuf,
    archive: Option<LgpArchive<File>>,
}

impl LgpLoader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), archive: None }
    }

    /// The archive's location (for log lines on this machine only).
    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl LgpLoader {
    /// The open archive (opened by the first call).
    fn archive(&mut self) -> Result<&mut LgpArchive<File>, String> {
        if self.archive.is_none() {
            let mut archive = LgpArchive::open_path(&self.path).map_err(|e| format!("cannot open char.lgp: {e}"))?;
            if let Some(overlay) = crate::model_layer::field() { archive.set_overlay(overlay); }
            self.archive = Some(archive);
        }
        self.archive.as_mut().ok_or_else(|| "cannot open char.lgp".to_string())
    }
}

impl CharacterLoader for LgpLoader {
    fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
        let archive = self.archive()?;
        match load_playable(archive, appearance) {
            Ok(character) => Ok(character),
            Err(_) => {
                let mut original = LgpArchive::open_path(&self.path).map_err(|e| e.to_string())?;
                load_playable(&mut original, appearance).map_err(|e| e.to_string())
            }
        }
    }

    fn load_npc(&mut self, model: &str, idle: &str) -> Result<Character, String> {
        let archive = self.archive()?;
        match load_standing(archive, model, idle) {
            Ok(character) => Ok(character),
            Err(_) => {
                let mut original = LgpArchive::open_path(&self.path).map_err(|e| e.to_string())?;
                load_standing(&mut original, model, idle).map_err(|e| e.to_string())
            }
        }
    }
}

static REMEMBERED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remember where this machine's `char.lgp` is (`None`: it has none, so the window draws markers).
/// Called by `open_ff7`; a later call replaces the earlier one.
pub fn remember(path: Option<PathBuf>) {
    if let Ok(mut slot) = REMEMBERED.lock() {
        *slot = path;
    }
}

/// The location [`remember`] was given, if any.
pub fn remembered() -> Option<PathBuf> {
    REMEMBERED.lock().ok().and_then(|slot| slot.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::playable::resolve_appearance;

    #[test]
    fn a_missing_archive_is_a_message_not_a_panic_and_names_no_path() {
        let mut loader = LgpLoader::new("/definitely/not/here/char.lgp");
        let cloud = resolve_appearance("ff7/cloud").unwrap();
        let err = loader.load(&cloud).map(|_| ()).unwrap_err();
        assert!(err.contains("cannot open char.lgp"), "{err}");
        assert!(!err.contains("definitely"), "{err}");
        // Asking again retries the open (the caller, not the loader, decides not to ask twice).
        assert!(loader.load(&cloud).is_err());
        assert_eq!(loader.path(), std::path::Path::new("/definitely/not/here/char.lgp"));
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_refused_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("char.lgp");
        std::fs::write(&path, b"this is not an LGP archive").unwrap();
        let cloud = resolve_appearance("ff7/cloud").unwrap();
        assert!(LgpLoader::new(&path).load(&cloud).is_err());
    }
}
