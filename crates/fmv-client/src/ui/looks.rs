//! A local, display-only memory of how each saved character looks.
//!
//! The server keeps a character's look (tint, appearance) in its stored profile, and a saved login
//! is only a token and a name: so the character list could not show a colour swatch or a look
//! until the character joined. This remembers what this machine last saw (when it registered the
//! character, and again whenever it joins), one small text file per character next to the token
//! files:
//!
//! ```text
//! fmv-look 1
//! name=Cloud
//! tint=5a7df5
//! appearance=ff7/cloud
//! ```
//!
//! It is NOT authoritative and nothing depends on it: a missing, unreadable or odd file just means
//! "look unknown" on the list, and the server's stored profile is what everybody else sees. Files
//! end in `.look`, so no token lookup ever mistakes one for a login. They hold no secret.

use super::menu::Look;
use crate::accounts::{name_key, server_key, write_private_atomic, ACCOUNTS_DIR};
use crate::net::clean;
use fmv_core::{EntityId, EntitySnapshot};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const HEADER: &str = "fmv-look 1";
const SUFFIX: &str = ".look";
/// A look file is a few dozen bytes; anything bigger is not ours.
const MAX_BYTES: u64 = 1024;
/// The server's own limit for an appearance id.
const MAX_APPEARANCE_CHARS: usize = 64;

/// The text of the look file of `name`.
pub fn format_look(name: &str, look: &Look) -> String {
    let mut text = format!("{HEADER}\nname={name}\ntint={}\n", super::palette::hex(look.tint));
    if let Some(appearance) = &look.appearance {
        text.push_str(&format!("appearance={appearance}\n"));
    }
    text
}

/// Parse a look file: the character's name and look, or `None` if it is not one (wrong header,
/// no name, no usable tint). Unknown lines are ignored so a newer client's file still reads.
pub fn parse_look(text: &str) -> Option<(String, Look)> {
    let mut lines = text.lines();
    if lines.next()?.trim_end() != HEADER {
        return None;
    }
    let (mut name, mut tint, mut appearance) = (None, None, None);
    for line in lines {
        match line.split_once('=') {
            Some(("name", v)) => name = Some(v.to_string()),
            Some(("tint", v)) => tint = crate::cli::parse_tint(v).ok(),
            Some(("appearance", v)) => {
                let v: String = clean(v).chars().take(MAX_APPEARANCE_CHARS).collect();
                appearance = (!v.trim().is_empty()).then_some(v);
            }
            _ => {}
        }
    }
    let name = name.filter(|n| !n.is_empty())?;
    Some((name, Look { tint: tint?, appearance }))
}

/// The look files of one client state directory.
#[derive(Debug, Clone)]
pub struct LookCache {
    dir: PathBuf,
}

impl LookCache {
    /// Next to the token files: `<state dir>/accounts/`.
    pub fn new(state_dir: &Path) -> Self {
        Self { dir: state_dir.join(ACCOUNTS_DIR) }
    }

    fn path(&self, server: &str, name: &str) -> PathBuf {
        self.dir.join(format!("{}.{}{SUFFIX}", server_key(server), name_key(name)))
    }

    /// How `name` looked the last time this machine saw it on `server`. `None` if never seen,
    /// unreadable, or the file belongs to another name that shares its file name key.
    pub fn load(&self, server: &str, name: &str) -> Option<Look> {
        let path = self.path(server, name);
        let meta = fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        fs::File::open(&path).ok()?.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        let (stored, look) = parse_look(&String::from_utf8(bytes).ok()?)?;
        (stored.to_lowercase() == name.to_lowercase()).then_some(look)
    }

    /// Remember how `name` looks. The accounts directory must exist (it does as soon as a token
    /// was saved); an error is for the log, never for the player.
    pub fn store(&self, server: &str, name: &str, look: &Look) -> std::io::Result<()> {
        write_private_atomic(&self.path(server, name), &format_look(name, look))
    }

    /// Remember how the server shows our own entity in the first snapshot of a join, for the
    /// character list next time. Display only: a problem is a line on stderr, never a failure.
    pub fn remember_joined(&self, server: &str, name: &str, entities: &[EntitySnapshot], entity: EntityId) {
        let Some(own) = entities.iter().find(|e| e.id == entity) else { return };
        let look = Look { tint: own.avatar.tint, appearance: own.avatar.appearance_ref.clone() };
        if let Err(e) = self.store(server, name, &look) {
            eprintln!("note: could not remember how {} looks: {e}", clean(name));
        }
    }

    /// Drop the note about `name` (a forgotten login). Nothing to do if there is none.
    pub fn forget(&self, server: &str, name: &str) {
        let path = self.path(server, name);
        // Only a note about this very name: another name may share the file name key.
        if self.load(server, name).is_some() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "127.0.0.1:47770";

    fn look(tint: [u8; 3], appearance: Option<&str>) -> Look {
        Look { tint, appearance: appearance.map(str::to_string) }
    }

    fn cache() -> (tempfile::TempDir, LookCache) {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join(ACCOUNTS_DIR)).unwrap();
        let cache = LookCache::new(dir.path());
        (dir, cache)
    }

    #[test]
    fn the_file_format_round_trips() {
        let l = look([0x5a, 0x7d, 0xf5], Some("ff7/cloud"));
        let text = format_look("Cloud", &l);
        assert_eq!(text, "fmv-look 1\nname=Cloud\ntint=5a7df5\nappearance=ff7/cloud\n");
        assert_eq!(parse_look(&text), Some(("Cloud".to_string(), l)));
        // No appearance: no line.
        let plain = format_look("Tifa", &look([1, 2, 3], None));
        assert!(!plain.contains("appearance"));
        assert_eq!(parse_look(&plain), Some(("Tifa".to_string(), look([1, 2, 3], None))));
        // A name with spaces and an `=` in it survives.
        let odd = format_look("Red = XIII", &look([9, 9, 9], None));
        assert_eq!(parse_look(&odd).unwrap().0, "Red = XIII");
    }

    #[test]
    fn files_that_are_not_look_files_are_ignored() {
        for bad in [
            "",
            "fmv-look 2\nname=A\ntint=000000\n",
            "name=A\ntint=000000\n",
            "fmv-look 1\ntint=000000\n",
            "fmv-look 1\nname=A\n",
            "fmv-look 1\nname=A\ntint=nothex\n",
            "fmv-look 1\nname=\ntint=000000\n",
            "garbage",
        ] {
            assert_eq!(parse_look(bad), None, "{bad:?}");
        }
        // Unknown lines are skipped, so a newer file still reads.
        let newer = "fmv-look 1\nname=A\nfuture=thing\ntint=010203\nmore\n";
        assert_eq!(parse_look(newer), Some(("A".to_string(), look([1, 2, 3], None))));
    }

    #[test]
    fn a_hostile_appearance_is_cleaned_and_bounded() {
        let text = format!("fmv-look 1\nname=A\ntint=000000\nappearance=\u{1b}[31m{}\n", "x".repeat(500));
        let (_, l) = parse_look(&text).unwrap();
        let a = l.appearance.unwrap();
        assert!(!a.contains('\u{1b}') && a.chars().count() <= 64, "{a:?}");
        let blank = parse_look("fmv-look 1\nname=A\ntint=000000\nappearance=\u{1b}\n").unwrap().1;
        assert_eq!(blank.appearance, None);
    }

    #[test]
    fn a_look_is_stored_and_found_again_per_server_and_name() {
        let (_d, c) = cache();
        assert_eq!(c.load(SERVER, "Cloud"), None, "never seen");
        c.store(SERVER, "Cloud", &look([1, 2, 3], Some("ff7/tifa"))).unwrap();
        c.store(SERVER, "Tifa", &look([4, 5, 6], None)).unwrap();
        assert_eq!(c.load(SERVER, "Cloud"), Some(look([1, 2, 3], Some("ff7/tifa"))));
        assert_eq!(c.load(SERVER, "cloud"), Some(look([1, 2, 3], Some("ff7/tifa"))), "names are case-insensitive, as on the server");
        assert_eq!(c.load(SERVER, "Tifa"), Some(look([4, 5, 6], None)));
        assert_eq!(c.load("10.0.0.9:1", "Cloud"), None, "another server");
        // Storing again replaces.
        c.store(SERVER, "Cloud", &look([7, 8, 9], None)).unwrap();
        assert_eq!(c.load(SERVER, "Cloud"), Some(look([7, 8, 9], None)));
    }

    #[test]
    fn two_names_sharing_a_file_name_never_show_each_others_look() {
        let (_d, c) = cache();
        c.store(SERVER, "Red XIII", &look([200, 20, 20], None)).unwrap();
        // `red_xiii` maps to the same file name but is another account.
        assert_eq!(c.load(SERVER, "red_xiii"), None);
        assert_eq!(c.load(SERVER, "Red XIII"), Some(look([200, 20, 20], None)));
        // ...and forgetting the other one leaves the note alone.
        c.forget(SERVER, "red_xiii");
        assert!(c.load(SERVER, "Red XIII").is_some());
    }

    #[test]
    fn forgetting_removes_the_note_and_tolerates_there_being_none() {
        let (_d, c) = cache();
        c.forget(SERVER, "Nobody");
        c.store(SERVER, "Cloud", &look([1, 2, 3], None)).unwrap();
        c.forget(SERVER, "Cloud");
        assert_eq!(c.load(SERVER, "Cloud"), None);
        assert!(fs::read_dir(&c.dir).unwrap().next().is_none(), "the file is gone");
    }

    #[test]
    fn a_join_remembers_the_look_the_server_showed_for_our_own_entity() {
        use fmv_core::{AnimState, AvatarAppearance, Pos};
        let entity = |id: u64, name: &str, tint: [u8; 3]| EntitySnapshot {
            id: EntityId(id),
            pos: Pos::default(),
            facing: 0.0,
            anim: AnimState::Idle,
            avatar: AvatarAppearance::new(name, tint, Some(format!("ff7/{name}"))),
        };
        let (_d, c) = cache();
        let all = [entity(8, "peer", [9, 9, 9]), entity(7, "me", [1, 2, 3])];
        c.remember_joined(SERVER, "Me", &all, EntityId(7));
        assert_eq!(c.load(SERVER, "Me"), Some(look([1, 2, 3], Some("ff7/me"))), "our own entity, not the peer's");
        // Our entity missing from the snapshot, or nowhere to write: nothing happens, nothing breaks.
        c.remember_joined(SERVER, "Ghost", &all, EntityId(99));
        assert_eq!(c.load(SERVER, "Ghost"), None);
        c.remember_joined(SERVER, "Me", &[], EntityId(7));
        let nowhere = LookCache::new(std::path::Path::new("/nonexistent/fmv-test-state"));
        nowhere.remember_joined(SERVER, "Me", &all, EntityId(7));
    }

    #[test]
    fn storing_without_an_accounts_directory_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let c = LookCache::new(dir.path());
        assert!(c.store(SERVER, "Cloud", &look([1, 2, 3], None)).is_err());
        assert_eq!(c.load(SERVER, "Cloud"), None);
    }

    #[test]
    fn oversized_or_binary_files_read_as_unknown() {
        let (_d, c) = cache();
        let path = c.path(SERVER, "Cloud");
        fs::write(&path, vec![b'x'; 5000]).unwrap();
        assert_eq!(c.load(SERVER, "Cloud"), None);
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(c.load(SERVER, "Cloud"), None);
        fs::create_dir(c.path(SERVER, "Tifa")).unwrap();
        assert_eq!(c.load(SERVER, "Tifa"), None, "a directory in the way");
    }

    #[test]
    fn look_files_are_never_taken_for_logins() {
        use crate::accounts::TokenStore;
        use fmv_protocol::Secret;
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path());
        store.save_new(SERVER, "Cloud", &Secret::new("fmvt_a")).unwrap();
        let c = LookCache::new(dir.path());
        c.store(SERVER, "Cloud", &look([1, 2, 3], None)).unwrap();
        c.store(SERVER, "Tifa", &look([4, 5, 6], None)).unwrap();
        let list = store.list(SERVER);
        assert_eq!(list.accounts.len(), 1, "Tifa has a look note but no login");
        assert!(list.problems.is_empty(), "{:?}", list.problems);
        assert_eq!(store.find(SERVER, None).unwrap().unwrap().name, "Cloud");
    }
}
