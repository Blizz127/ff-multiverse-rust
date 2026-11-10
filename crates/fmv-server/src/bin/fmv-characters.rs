//! Operator tool for a player's character: show it, change an existing account's look to a generic placeholder
//! body (an account that still looks like a canon character, for instance), or delete a character and bring it back.
//!
//! ```text
//! fmv-characters <state-dir> show <account-id|name> [--game ff7]
//! fmv-characters <state-dir> restyle <account-id|name> --body <lean|sturdy|slight> [--tint rrggbb] [--game ff7] [--dry-run]
//! fmv-characters <state-dir> delete <account-id|name> [--dry-run]
//! fmv-characters <state-dir> trash
//! fmv-characters <state-dir> restore <trash-entry|account-id|name> [--dry-run]
//! ```
//!
//! # delete, trash and restore
//!
//! `delete` does what a player's own delete does (the client's Del key asks the server, with the character's token):
//! it moves the account's records (account, avatar, player character, progress) into a timestamped entry of the
//! state directory's `trash/` folder, frees the name, voids the token and takes the account off the index, and
//! **nothing else changes**: the tool hashes every file of the state directory before and after and fails the run
//! if any file other than the account's own records and `index.log` differs. It is the way to free a name whose
//! player has no token (or to remove a character by hand). `trash` lists the entries. `restore` brings one back: the
//! name must still be free, the character comes back as a **new** account (the old id is retired for good, a new
//! one is given, every record is re-keyed) with a **new token** that is printed once, so it is how a character a
//! player deleted by mistake is given back (the client forgot the old token when it deleted). A restored entry is
//! kept in the trash under a `.restored-<id>` name and cannot be restored a second time. An entry is named as
//! `trash` lists it, by the id the deleted account had, or by the character's name. Like every command here, these
//! need the server stopped (the tool takes the state directory's lock).
//!
//! `<state-dir>` is the SERVER's state directory, the one that holds `accounts/`, `avatars/`, `characters/` and
//! `progression/` (the server's default is the `server` directory inside its base state directory, which is not
//! the base itself). There is no default and no environment variable: the tool only touches the directory it is
//! given. The account is a number (its id) or its display name (compared by the store's own rule: case, width and
//! look-alike letters make no difference). `--game` defaults to `ff7`. The bodies are the game's generic
//! placeholders ([`fmv_server::admin::offered_bodies`]); a canon character's name or look, or anything misspelt, is
//! **refused** (exit 1) with the list of what is offered, never replaced by a default. `--tint` is a colour as six
//! hex digits (`5a7df5`).
//!
//! **Stop the server first.** The tool opens the store like the server does, so it takes the state directory's lock
//! and is refused while a server (or anything else) holds it ("locked by pid ..."): nothing is touched then.
//!
//! # What `restyle` changes, and what it never does
//!
//! It changes exactly three values of the account's `characters/<id>.json`: the body (`identity.body`), the game's
//! binding (the model the game draws it with, `ff7/body/<archetype>`) and, when `--tint` is given, the colour
//! (`identity.tint`). Nothing else is written. In particular **no `progression/<id>/*.json`** (level, experience,
//! gold, AP, items, limit gauge, story), **no combat slot** (the canon character the account fights as stays
//! the same, so its numbers do not move), no avatar profile (which is what a server rolled back to v7 reads, so a
//! rollback still draws and fights the account exactly as before), no account record and no index.
//!
//! An account that predates player-character records (every account of a v6 or v7 server, until its first login
//! with a v8 server) has no `characters/<id>.json` yet. `restyle` then first builds the record **exactly as that
//! first login would** (the server's own migration rule, [`fmv_server::admin::legacy_entry`], from the account's
//! avatar profile; origin `migrated`, its slot being the canon character it was fighting as), applies the look
//! change to it, and writes the result in one step. The server finds the record at its next login like any other.
//! An account that has neither a record nor an avatar profile is refused (there is nothing to build the character
//! from): log in with it once, then restyle.
//!
//! A restyle that would change nothing (the look is already this one) writes nothing. `--dry-run` does everything but
//! the write, prints the same plan and proves that nothing was written. Opening a store (which `show` does too, to
//! read through the store's own rules) makes the empty `characters/` directory on a state directory that has none (a
//! v7 one): that is what the store does, not the restyle, and `restyle` reports it as such.
//!
//! # What it prints
//!
//! For `restyle`: the account, whether the record was created from the legacy data or existed, `slot (unchanged)`,
//! `body: A -> B`, `binding: A -> B`, `tint: A -> B` (or `unchanged`), and a verification block: every file of the
//! state directory is hashed (SHA-256) before the tool opens the store, right after it opens it and after it
//! finished, and the block lists each file that was NOT written with `sha256 <12 hex> -> same`, plus the one file
//! written. A file that changed other than the record is reported as `CHANGED` and fails the run (exit 1), as does a
//! record read back from the disk that is not the planned one. The hashes are the ones `sha256sum` prints (first 12
//! hex digits shown), so they can be checked by hand. The tool reads no game file and prints no game data.
//!
//! # Exit codes
//!
//! 0 done (also a dry run and a change that was not needed); 1 refused or kept (locked, not a state directory,
//! unknown account, nothing to build the character from, a record that cannot be read, a body that is not offered,
//! a failed verification); 2 usage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use fmv_core::{name_key, AccountId, GameId};
use fmv_persist::{PersistError, PlayerCharacter, ProgressRef, Restyle, RestyleAdded, Store, TrashEntry, TrashState};
use fmv_server::admin::{self, BindingKind};

/// The default for `--game`.
const DEFAULT_GAME: GameId = GameId::Ff7;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- the command line ----------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Show { account: String, game: GameId },
    Restyle { account: String, game: GameId, body: String, tint: Option<[u8; 3]>, dry_run: bool },
    Delete { account: String, dry_run: bool },
    Trash,
    Restore { entry: String, dry_run: bool },
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    dir: PathBuf,
    command: Command,
}

/// The positional arguments and flags of a command line: each flag at most once, as `--flag value` or `--flag=value`.
#[derive(Default)]
struct Scanned {
    positional: Vec<String>,
    values: BTreeMap<&'static str, String>,
    switches: BTreeSet<&'static str>,
}

fn scan_args(rest: &[String], value_flags: &[&'static str], switches: &[&'static str]) -> Result<Scanned, String> {
    let mut out = Scanned::default();
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            out.positional.push(arg.clone());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if let Some(&known) = value_flags.iter().find(|f| **f == name) {
            let value = match inline {
                Some(value) => value,
                None => match args.next() {
                    Some(next) if !next.starts_with("--") => next.clone(),
                    _ => return Err(format!("--{name} needs a value")),
                },
            };
            if out.values.insert(known, value).is_some() {
                return Err(format!("--{name} is given twice"));
            }
        } else if let Some(&known) = switches.iter().find(|f| **f == name) {
            if inline.is_some() {
                return Err(format!("--{name} takes no value"));
            }
            if !out.switches.insert(known) {
                return Err(format!("--{name} is given twice"));
            }
        } else {
            return Err(format!("unknown option --{name}"));
        }
    }
    Ok(out)
}

fn the_account(scanned: &Scanned) -> Result<String, String> {
    match scanned.positional.as_slice() {
        [account] => Ok(account.clone()),
        [] => Err("an account (its id or its display name) is required".into()),
        _ => Err("exactly one account (its id or its display name) is expected".into()),
    }
}

fn the_game(scanned: &Scanned) -> Result<GameId, String> {
    match scanned.values.get("game") {
        None => Ok(DEFAULT_GAME),
        Some(wanted) => GameId::ALL
            .into_iter()
            .find(|game| game.key().eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("{wanted:?} is not a game (expected one of Ff7, Ff8, Ff9, Ff10, Ff12, Fixture)")),
    }
}

/// `rrggbb` (an optional leading `#` is accepted, but quote it: a shell reads an unquoted `#` as a comment).
fn parse_tint(text: &str) -> Result<[u8; 3], String> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    let bad = || format!("--tint is a colour as six hex digits (rrggbb), not {text:?}");
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let channel = |at: usize| digits.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).ok_or_else(bad);
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

fn parse(args: &[String]) -> Result<Invocation, String> {
    let (Some(dir), Some(command)) = (args.first(), args.get(1)) else {
        return Err("a state directory and a command are required".into());
    };
    let rest = args.get(2..).unwrap_or_default();
    let command = match command.as_str() {
        "show" => {
            let scanned = scan_args(rest, &["game"], &[])?;
            Command::Show { account: the_account(&scanned)?, game: the_game(&scanned)? }
        }
        "restyle" => {
            let scanned = scan_args(rest, &["body", "tint", "game"], &["dry-run"])?;
            let body = scanned.values.get("body").cloned().ok_or("restyle needs --body")?;
            let tint = scanned.values.get("tint").map(|text| parse_tint(text)).transpose()?;
            Command::Restyle {
                account: the_account(&scanned)?,
                game: the_game(&scanned)?,
                body,
                tint,
                dry_run: scanned.switches.contains("dry-run"),
            }
        }
        "delete" => {
            let scanned = scan_args(rest, &[], &["dry-run"])?;
            Command::Delete { account: the_account(&scanned)?, dry_run: scanned.switches.contains("dry-run") }
        }
        "trash" => {
            let scanned = scan_args(rest, &[], &[])?;
            if !scanned.positional.is_empty() {
                return Err("trash takes no arguments".into());
            }
            Command::Trash
        }
        "restore" => {
            let scanned = scan_args(rest, &[], &["dry-run"])?;
            let entry = match scanned.positional.as_slice() {
                [entry] => entry.clone(),
                [] => return Err("a trash entry (as `trash` lists it, or the deleted account's id or the character's name) is required".into()),
                _ => return Err("exactly one trash entry is expected".into()),
            };
            Command::Restore { entry, dry_run: scanned.switches.contains("dry-run") }
        }
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(Invocation { dir: PathBuf::from(dir), command })
}

fn usage() -> ExitCode {
    let bodies: Vec<&str> = admin::offered_bodies(DEFAULT_GAME).iter().map(|body| body.archetype).collect();
    eprintln!(
        "usage: fmv-characters <state-dir> show <account-id|name> [--game ff7]\n       \
         fmv-characters <state-dir> restyle <account-id|name> --body <{}> [--tint rrggbb] [--game ff7] [--dry-run]\n       \
         fmv-characters <state-dir> delete <account-id|name> [--dry-run]\n       \
         fmv-characters <state-dir> trash\n       \
         fmv-characters <state-dir> restore <trash-entry|account-id|name> [--dry-run]\n\
         <state-dir> is the server's state directory (accounts/, avatars/, characters/, progression/); stop the server first",
        bodies.join("|")
    );
    ExitCode::from(2)
}

// ---- SHA-256 -------------------------------------------------------------------------------------------------

/// SHA-256 (FIPS 180-4), so that the hashes printed are the ones `sha256sum` prints and can be checked by hand. (The
/// server has no hash crate of this kind to use; the unit tests below hold the digests `sha256sum` printed for inputs
/// around every padding boundary.)
mod sha256 {
    use std::io;

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
        0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
        0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
        0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
        0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    const INITIAL: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

    pub struct Sha256 {
        state: [u32; 8],
        block: [u8; 64],
        filled: usize,
        bytes: u64,
    }

    impl Sha256 {
        pub fn new() -> Self {
            Self { state: INITIAL, block: [0; 64], filled: 0, bytes: 0 }
        }

        pub fn update(&mut self, mut data: &[u8]) {
            self.bytes = self.bytes.wrapping_add(data.len() as u64);
            if self.filled > 0 {
                let take = (64 - self.filled).min(data.len());
                let (head, tail) = data.split_at(take);
                self.block[self.filled..self.filled + take].copy_from_slice(head);
                self.filled += take;
                data = tail;
                if self.filled < 64 {
                    return;
                }
                compress(&mut self.state, &self.block);
                self.filled = 0;
            }
            let (blocks, rest) = data.as_chunks::<64>();
            for block in blocks {
                compress(&mut self.state, block);
            }
            self.block[..rest.len()].copy_from_slice(rest);
            self.filled = rest.len();
        }

        pub fn finish(mut self) -> [u8; 32] {
            let bits = self.bytes.wrapping_mul(8);
            self.update(&[0x80]);
            while self.filled != 56 {
                self.update(&[0]);
            }
            self.update(&bits.to_be_bytes());
            let mut digest = [0u8; 32];
            for (out, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
                *out = word.to_be_bytes();
            }
            digest
        }
    }

    impl io::Write for Sha256 {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.update(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let (a, b) = (w[i - 15], w[i - 2]);
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for (&k, &word) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(choose).wrapping_add(k).wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    /// The digest of `data` in one call (the tool itself hashes files through `io::copy`).
    #[cfg(test)]
    pub fn of(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finish()
    }
}

type Digest = [u8; 32];

/// The first 12 hex digits of a digest: what is shown (the comparison always uses all 32 bytes).
fn short(digest: &Digest) -> String {
    digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_file(path: &Path) -> io::Result<Digest> {
    let mut file = fs::File::open(path)?;
    let mut hasher = sha256::Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finish())
}

// ---- the whole state directory, before and after -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir,
    File(Digest),
    Other,
}

/// Every file (by SHA-256) and directory under the state directory, by relative path with `/` separators. `LOCK`
/// (the pid of whoever holds the store) is the only thing left out: it is not data.
type Tree = BTreeMap<String, Entry>;

fn scan_tree(root: &Path) -> io::Result<Tree> {
    fn walk(base: &Path, dir: &Path, out: &mut Tree) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(base).map_err(io::Error::other)?.to_string_lossy().into_owned();
            if rel == "LOCK" {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(base, &path, out)?;
            } else if kind.is_file() {
                out.insert(rel, Entry::File(sha256_file(&path)?));
            } else {
                out.insert(rel, Entry::Other);
            }
        }
        Ok(())
    }
    let mut out = Tree::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn describe(entry: Option<&Entry>) -> String {
    match entry {
        None => "absent".to_owned(),
        Some(Entry::Dir) => "a directory".to_owned(),
        Some(Entry::File(digest)) => format!("sha256 {}", short(digest)),
        Some(Entry::Other) => "not a regular file".to_owned(),
    }
}

/// What the three scans prove.
#[derive(Debug, Default, PartialEq, Eq)]
struct Verification {
    /// The lines to print, one per file.
    lines: Vec<String>,
    /// What opening the store did (not the restyle's doing).
    notes: Vec<String>,
    /// What went wrong: a file other than the record changed while the tool held the store, the record was or was not
    /// written against what the run meant to do.
    failures: Vec<String>,
}

/// Compare the state directory as it was handed to the tool (`before_open`), right after the store was opened
/// (`opened`, the lock is held, so nothing else writes) and after the tool finished (`after`). `record` is the one
/// file this run may write (relative path) and `expect_write` whether it meant to.
///
/// What changed between `before_open` and `opened` is what opening the store does (a state directory of a v7 server
/// has no `characters/` directory, which the store makes); it is noted, not blamed on the restyle. Anything that
/// changed between `opened` and `after` other than the record is a failure, and so is the record changing when no
/// write was meant, or not changing when one was.
fn verify(before_open: &Tree, opened: &Tree, after: &Tree, record: &str, expect_write: bool) -> Verification {
    let mut v = Verification::default();
    // The record's own line comes last, after the files that were not written.
    let mut record_lines = Vec::new();
    // The record is always judged, even when no scan has it (a restyle that was meant to write it and did not).
    let paths: BTreeSet<&str> = before_open.keys().chain(opened.keys()).chain(after.keys()).map(String::as_str).chain([record]).collect();
    for rel in paths {
        let (a, b, c) = (before_open.get(rel), opened.get(rel), after.get(rel));
        if rel == record {
            match (expect_write, b == c) {
                (true, true) => v.failures.push(format!("{rel} was not written although the restyle meant to")),
                (true, false) => {
                    let how = if b.is_none() { "new file, ".to_owned() } else { format!("{} -> ", describe(b)) };
                    record_lines.push(format!("written: {rel} ({how}{})", describe(c)));
                }
                (false, false) => {
                    record_lines.push(format!("{rel} {} -> {} CHANGED", describe(b), describe(c)));
                    v.failures.push(format!("{rel} was written although this run meant to write nothing"));
                }
                (false, true) => record_lines.push(match b {
                    None => format!("not written: {rel} (absent)"),
                    Some(_) => format!("not written: {rel} ({} -> same)", describe(b)),
                }),
            }
            if a != b {
                v.notes.push(format!("{rel} was {} when the store was opened", if a.is_none() { "created" } else { "changed" }));
            }
            continue;
        }
        if a == Some(&Entry::Dir) && b == a && c == a {
            continue;
        }
        if matches!(c, Some(Entry::Dir)) && a.is_none() && b == c {
            v.notes.push(format!("directory {rel} was created when the store was opened (opening a store makes the directory of the character records)"));
            continue;
        }
        if b != c {
            v.lines.push(format!("{rel} {} -> {} CHANGED", describe(b), describe(c)));
            v.failures.push(format!("{rel} changed while the tool held the store, and it is not the record: {} -> {}", describe(b), describe(c)));
        } else if a == c {
            v.lines.push(format!("{rel} {} -> same", describe(a)));
        } else {
            v.lines.push(format!("{rel} {} -> {} (changed when the store was opened, not by the restyle)", describe(a), describe(c)));
            v.notes.push(format!("{rel} was changed when the store was opened"));
        }
    }
    v.lines.extend(record_lines);
    v
}

// ---- accounts ----------------------------------------------------------------------------------------------------

/// The server's state directory, or a sentence that says what is wrong with the path. The store creates a state
/// directory out of any path it is given, so a mistyped one must be stopped here: it would leave a LOCK, an index and
/// four empty directories behind.
fn check_state_dir(dir: &Path) -> Result<(), String> {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", dir.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    }
    if !dir.join("index.log").is_file() {
        return Err(format!(
            "{} is not a server state directory (it has no index.log): name the directory that holds accounts/, avatars/ and progression/ \
             (the server's default is the `server` directory inside its base state directory, not the base itself)",
            dir.display()
        ));
    }
    Ok(())
}

/// Open the store (taking the directory's lock), or print why not.
fn open_store(dir: &Path) -> Result<Store, ExitCode> {
    match Store::open(dir) {
        Ok(store) => {
            for warning in store.open_warnings() {
                eprintln!("note: {warning}");
            }
            Ok(store)
        }
        Err(e) => {
            eprintln!("cannot open {}: {e}", dir.display());
            if matches!(e, PersistError::Locked { .. }) {
                eprintln!("stop the server that holds the directory first; nothing was changed");
            }
            Err(ExitCode::FAILURE)
        }
    }
}

/// The account named by an id or a display name, with its name.
fn resolve_account(store: &Store, arg: &str) -> Result<(AccountId, String), String> {
    let by_id = match arg.parse::<u64>() {
        Ok(id) => match store.account_info(AccountId(id)) {
            Ok(info) => info,
            Err(e) => return Err(format!("account {id}: {e}")),
        },
        Err(_) => None,
    };
    let by_name = match store.find_account_by_name(arg) {
        Some(id) => match store.account_info(id) {
            Ok(info) => info,
            Err(e) => return Err(format!("account {}: {e}", id.0)),
        },
        None => None,
    };
    match (by_id, by_name) {
        (Some(a), Some(b)) if a.account_id != b.account_id => Err(format!(
            "{arg:?} is both the id of account {} ({:?}) and the name of account {} ({:?}): name the account by the one that is not a number",
            a.account_id.0, a.display_name, b.account_id.0, b.display_name
        )),
        (Some(info), _) | (None, Some(info)) => Ok((info.account_id, info.display_name)),
        (None, None) => Err(format!("unknown account {arg:?} (give an account id or a display name)")),
    }
}

// ---- printing ----------------------------------------------------------------------------------------------------

fn tint_text(tint: [u8; 3]) -> String {
    format!("{:02x}{:02x}{:02x}", tint[0], tint[1], tint[2])
}

fn body_text(body: &str) -> String {
    if body.is_empty() {
        "(none)".to_owned()
    } else {
        body.to_owned()
    }
}

fn binding_text(binding: Option<&str>) -> String {
    binding.map_or_else(|| "(none)".to_owned(), str::to_owned)
}

fn slot_text(slot: Option<u8>) -> String {
    slot.map_or_else(|| "(none)".to_owned(), |slot| slot.to_string())
}

fn origin_text(record: &PlayerCharacter) -> String {
    format!("{:?}", record.origin).to_lowercase()
}

fn header(id: AccountId, name: &str, game: GameId) {
    println!("account {} {name:?} (game {})", id.0, game.key());
}

// ---- show ---------------------------------------------------------------------------------------------------------

fn show(dir: &Path, account_arg: &str, game: GameId) -> ExitCode {
    if let Err(why) = check_state_dir(dir) {
        eprintln!("{why}");
        return ExitCode::FAILURE;
    }
    let store = match open_store(dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let (account, name) = match resolve_account(&store, account_arg) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
    };
    let (record, added) = match store.preview_player_character(account, game, now(), |view| admin::legacy_entry(game, view)) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("account {} {name:?}: {e}", account.0);
            return ExitCode::FAILURE;
        }
    };
    header(account, &name, game);
    match added {
        RestyleAdded::Nothing => println!("record: stored (created {}, last updated {})", record.created_at, record.updated_at),
        RestyleAdded::Record => println!("record: none yet; this is what the first login would create from the legacy data (nothing is written by this command)"),
        RestyleAdded::Entry => println!("record: stored, but without an entry for {}; this is what the first login would add from the legacy data (nothing is written by this command)", game.key()),
    }
    println!("  name: {}", record.identity.name);
    println!("  tint: {}", tint_text(record.identity.tint));
    println!("  body: {}", body_text(&record.identity.body));
    let binding = record.binding(game);
    let kind = binding.map(|binding| match admin::binding_kind(game, binding) {
        BindingKind::CanonCharacter => " (a canon character's look)",
        BindingKind::PlaceholderBody => " (a placeholder body)",
        BindingKind::Unknown => " (not a model the game offers: shown as no look)",
    });
    println!("  binding: {}{}", binding_text(binding), kind.unwrap_or_default());
    println!("  slot: {}", slot_text(record.slot(game)));
    println!("  origin: {}", origin_text(&record));
    let pointers: Vec<String> = record
        .game(game)
        .map(|entry| entry.progress.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|pointer| match pointer {
            ProgressRef::Character { character, .. } => format!("character progress (character {character})"),
            ProgressRef::Story { .. } => "story".to_owned(),
        })
        .collect();
    println!("  progress pointers: {}", if pointers.is_empty() { "(none)".to_owned() } else { pointers.join(", ") });
    match store.avatar_for_account(account) {
        Ok(Some(avatar)) => println!(
            "avatar profile look (what a server rolled back to v7 reads; this tool never changes it): {}",
            binding_text(avatar.override_for(game))
        ),
        Ok(None) => println!("avatar profile: none"),
        Err(e) => println!("avatar profile: cannot be read ({e})"),
    }
    ExitCode::SUCCESS
}

// ---- restyle ------------------------------------------------------------------------------------------------------

fn restyle(dir: &Path, account_arg: &str, game: GameId, body_arg: &str, tint: Option<[u8; 3]>, dry_run: bool) -> ExitCode {
    // What is asked for is checked before anything is opened: an unknown body is refused, never defaulted.
    let Some(body) = admin::body_named(game, body_arg) else {
        let offered: Vec<&str> = admin::offered_bodies(game).iter().map(|body| body.archetype).collect();
        if offered.is_empty() {
            eprintln!("refused: {} offers no bodies, so there is nothing to restyle to; nothing was changed", game.key());
        } else {
            eprintln!(
                "refused: {body_arg:?} is not a body {} offers (offered: {}); bodies are generic placeholders, a canon character's look \
                 is not one; nothing was changed",
                game.key(),
                offered.join(", ")
            );
        }
        return ExitCode::FAILURE;
    };
    if let Err(why) = check_state_dir(dir) {
        eprintln!("{why}");
        return ExitCode::FAILURE;
    }
    let scan_failed = |when: &str, e: io::Error| {
        eprintln!("cannot read the state directory {when}: {e}");
        ExitCode::FAILURE
    };
    let before_open = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("before opening it", e),
    };
    let store = match open_store(dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let opened = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after opening it", e),
    };
    let (account, name) = match resolve_account(&store, account_arg) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("{why}; no file was written");
            return ExitCode::FAILURE;
        }
    };

    let change = Restyle { body: body.archetype.to_owned(), binding: body.binding.to_owned(), tint };
    let report = match store.restyle_player_character(account, game, &change, now(), dry_run, |view| admin::legacy_entry(game, view)) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("account {} {name:?}: {e}; no file was written", account.0);
            return ExitCode::FAILURE;
        }
    };
    let read_back = store.get_player_character(account);
    drop(store);

    // The plan.
    if dry_run {
        println!("dry run: nothing was written");
    }
    header(account, &name, game);
    let (before, after) = (&report.before, &report.after);
    let legacy = format!("slot {}, binding {}", slot_text(before.slot(game)), binding_text(before.binding(game)));
    match report.added {
        RestyleAdded::Record => println!("record: created from the legacy data: {legacy}"),
        RestyleAdded::Entry => println!("record: existed; the entry for {} was created from the legacy data: {legacy}", game.key()),
        RestyleAdded::Nothing => println!("record: existed (origin {})", origin_text(before)),
    }
    println!("slot (unchanged): {}", slot_text(after.slot(game)));
    println!("body: {} -> {}", body_text(&before.identity.body), body_text(&after.identity.body));
    println!("binding: {} -> {}", binding_text(before.binding(game)), binding_text(after.binding(game)));
    if before.identity.tint == after.identity.tint {
        println!("tint: unchanged ({})", tint_text(after.identity.tint));
    } else {
        println!("tint: {} -> {}", tint_text(before.identity.tint), tint_text(after.identity.tint));
    }
    if report.added == RestyleAdded::Nothing && !report.look_changed() {
        println!("nothing to change: the character already looks like this");
    }

    // The proof.
    let record_rel = format!("characters/{}.json", account.0);
    let after_scan = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after the restyle", e),
    };
    let mut verification = verify(&before_open, &opened, &after_scan, &record_rel, report.written);
    if report.written {
        match &read_back {
            Ok(Some(stored)) if *stored == report.after => {}
            other => verification.failures.push(format!("the record read back from the disk is not the planned one ({other:?})")),
        }
    }
    println!("verification (every file of the state directory, hashed before the tool opened it, after it opened it and after it finished):");
    for line in &verification.lines {
        println!("  {line}");
    }
    for note in &verification.notes {
        println!("  note: {note}");
    }
    if report.written && verification.failures.is_empty() {
        println!("  read back: the record on disk is the planned one");
    }
    if !verification.failures.is_empty() {
        for failure in &verification.failures {
            println!("VERIFICATION FAILED: {failure}");
            eprintln!("VERIFICATION FAILED: {failure}");
        }
        return ExitCode::FAILURE;
    }
    if report.written {
        println!("result: done; {record_rel} was written and every other file is unchanged");
    } else if dry_run {
        println!("result: dry run; no file was written");
    } else {
        println!("result: nothing to change; no file was written");
    }
    ExitCode::SUCCESS
}

// ---- delete, trash and restore ----------------------------------------------------------------------------------

/// What a delete or a restore may change in the state directory: these paths, and everything under `prefixes`, may
/// appear, change or disappear. Every other path must be exactly as it was.
#[derive(Debug, Default)]
struct Allowed {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
}

impl Allowed {
    fn permits(&self, rel: &str) -> bool {
        self.exact.contains(rel) || self.prefixes.iter().any(|prefix| rel.starts_with(prefix.as_str()))
    }
}

/// What comparing the tree before and after found outside the allowed paths.
#[derive(Debug, Default, PartialEq, Eq)]
struct Untouched {
    /// How many files and directories were compared and are identical (every other account's records among them).
    files: usize,
    dirs: usize,
    failures: Vec<String>,
}

/// Every path that is not allowed to change must be there before and after with the same digest.
fn verify_untouched(opened: &Tree, after: &Tree, allowed: &Allowed) -> Untouched {
    let mut out = Untouched::default();
    let paths: BTreeSet<&str> = opened.keys().chain(after.keys()).map(String::as_str).collect();
    for rel in paths {
        if allowed.permits(rel) {
            continue;
        }
        match (opened.get(rel), after.get(rel)) {
            (Some(a), Some(b)) if a == b => match a {
                Entry::File(_) => out.files += 1,
                Entry::Dir => out.dirs += 1,
                Entry::Other => {}
            },
            (a, b) => out.failures.push(format!("{rel} {} -> {}, and it is not one of the files this run may change", describe(a), describe(b))),
        }
    }
    out
}

fn print_untouched(untouched: &Untouched) {
    println!("  {} other file(s) and {} director(ies): byte for byte unchanged (sha256 before and after)", untouched.files, untouched.dirs);
}

/// The proof block's failures, printed like the restyle's; `true` if there were none.
fn report_failures(failures: &[String]) -> bool {
    for failure in failures {
        println!("VERIFICATION FAILED: {failure}");
        eprintln!("VERIFICATION FAILED: {failure}");
    }
    failures.is_empty()
}

/// The deleted trash entry the operator means: by the name `trash` lists, by the id the deleted account had, or by the
/// character's name (compared by the store's own rule). More than one candidate is refused with the list, never guessed.
fn pick_entry<'a>(entries: &'a [TrashEntry], arg: &str) -> Result<&'a TrashEntry, String> {
    let restorable: Vec<&TrashEntry> = entries.iter().filter(|e| e.state == TrashState::Deleted && e.problem.is_none()).collect();
    if let Some(exact) = restorable.iter().find(|e| e.name == arg) {
        return Ok(exact);
    }
    let by_id = arg.parse::<u64>().ok().map(|id| restorable.iter().copied().filter(|e| e.account_id.0 == id).collect::<Vec<_>>());
    let by_name = {
        let wanted = name_key(arg);
        restorable.iter().copied().filter(|e| !wanted.is_empty() && name_key(&e.display_name) == wanted).collect::<Vec<_>>()
    };
    let candidates = match by_id {
        Some(found) if !found.is_empty() => found,
        _ => by_name,
    };
    match candidates.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no restorable trash entry matches {arg:?} (`trash` lists them; a restored or interrupted one cannot be restored)")),
        many => {
            let names: Vec<&str> = many.iter().map(|e| e.name.as_str()).collect();
            Err(format!("{arg:?} matches {} trash entries ({}): name one of them exactly", many.len(), names.join(", ")))
        }
    }
}

fn state_text(state: TrashState) -> String {
    match state {
        TrashState::Deleted => "deleted".to_owned(),
        TrashState::Pending => "delete interrupted (the next open of the store settles it)".to_owned(),
        TrashState::Restoring { as_account } => format!("restore interrupted, as account {} (the next open of the store settles it)", as_account.0),
        TrashState::Restored { as_account } => format!("restored as account {}", as_account.0),
    }
}

fn delete(dir: &Path, account_arg: &str, dry_run: bool) -> ExitCode {
    if let Err(why) = check_state_dir(dir) {
        eprintln!("{why}");
        return ExitCode::FAILURE;
    }
    let scan_failed = |when: &str, e: io::Error| {
        eprintln!("cannot read the state directory {when}: {e}");
        ExitCode::FAILURE
    };
    let store = match open_store(dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let opened = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after opening it", e),
    };
    let (account, name) = match resolve_account(&store, account_arg) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("{why}; no file was written");
            return ExitCode::FAILURE;
        }
    };
    let plan = match store.deletion_plan(account) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("account {} {name:?}: {e}; no file was written", account.0);
            return ExitCode::FAILURE;
        }
    };
    if dry_run {
        println!("dry run: nothing was written");
    }
    println!("account {} {name:?}", account.0);
    println!("delete: {} file(s) move into the trash:", plan.len());
    for rel in &plan {
        println!("  {rel}");
    }
    println!("afterwards the name {name:?} is free, the token stops working and the account id {} is retired for good", account.0);

    if dry_run {
        drop(store);
        let after = match scan_tree(dir) {
            Ok(tree) => tree,
            Err(e) => return scan_failed("after the dry run", e),
        };
        println!("verification (every file of the state directory, hashed before and after):");
        let untouched = verify_untouched(&opened, &after, &Allowed::default());
        print_untouched(&untouched);
        if !report_failures(&untouched.failures) {
            return ExitCode::FAILURE;
        }
        println!("result: dry run; no file was written");
        return ExitCode::SUCCESS;
    }

    let deleted = match store.delete_account(account, now()) {
        Ok(deleted) => deleted,
        Err(e) => {
            eprintln!("account {} {name:?}: {e}; nothing was deleted", account.0);
            return ExitCode::FAILURE;
        }
    };
    // Its saved spots are no records the trash takes (MMO-style saving): forget them, as the server's delete does.
    let forgotten = match store.forget_locations(account) {
        Ok(forgotten) => forgotten,
        Err(e) => {
            eprintln!("account {} {name:?}: deleted, but its saved spot could not be removed: {e}", account.0);
            Vec::new()
        }
    };
    drop(store);
    let after = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after the delete", e),
    };

    // The proof: the account's files are in the trash entry byte for byte and gone from where they were; the index grew;
    // everything else is exactly as it was.
    let entry_dir = format!("trash/{}", deleted.entry);
    let mut failures = Vec::new();
    println!("verification (every file of the state directory, hashed after the store was opened and after the delete):");
    for rel in &deleted.files {
        let moved_to = format!("{entry_dir}/{rel}");
        match (opened.get(rel), after.get(&moved_to), after.contains_key(rel)) {
            (Some(Entry::File(was)), Some(Entry::File(now_is)), false) if was == now_is => {
                println!("  moved: {rel} (sha256 {}) -> {moved_to}, the same bytes", short(was));
            }
            (was, now_is, still_there) => failures.push(format!(
                "{rel} was {} and is now {} in the trash{}",
                describe(was),
                describe(now_is),
                if still_there { " and is still where it was" } else { "" }
            )),
        }
    }
    if opened.get("index.log") == after.get("index.log") {
        failures.push("index.log did not change, so the account was not taken off the index".to_owned());
    } else {
        println!("  index.log: changed (the release of account {} was appended to it)", account.0);
    }
    if !after.contains_key(&format!("{entry_dir}/manifest.json")) {
        failures.push(format!("{entry_dir}/manifest.json was not written"));
    }
    let mut allowed = Allowed::default();
    allowed.exact.extend(["index.log".to_owned(), "trash".to_owned(), entry_dir.clone(), format!("progression/{}", account.0)]);
    allowed.exact.extend(deleted.files.iter().cloned());
    for rel in &forgotten {
        if after.contains_key(rel) {
            failures.push(format!("{rel} (the saved spot) is still there"));
        } else {
            println!("  forgotten: {rel} (where the character last stood; not kept in the trash)");
        }
    }
    allowed.exact.extend(forgotten.iter().cloned());
    allowed.prefixes.push(format!("{entry_dir}/"));
    let untouched = verify_untouched(&opened, &after, &allowed);
    print_untouched(&untouched);
    failures.extend(untouched.failures);
    if !report_failures(&failures) {
        return ExitCode::FAILURE;
    }
    if !deleted.unfinished.is_empty() {
        println!("note: the move of {} file(s) could not be finished ({}); the next open of the store finishes it", deleted.unfinished.len(), deleted.unfinished.join("; "));
    }
    println!(
        "result: done; account {} {name:?} is in the trash entry {} (its name is free); restore it with: fmv-characters {} restore {}",
        account.0,
        deleted.entry,
        dir.display(),
        deleted.entry
    );
    ExitCode::SUCCESS
}

fn trash(dir: &Path) -> ExitCode {
    if let Err(why) = check_state_dir(dir) {
        eprintln!("{why}");
        return ExitCode::FAILURE;
    }
    let store = match open_store(dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let entries = match store.trash_entries() {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("cannot read the trash: {e}");
            return ExitCode::FAILURE;
        }
    };
    if entries.is_empty() {
        println!("the trash is empty");
        return ExitCode::SUCCESS;
    }
    println!("the trash: {} entr(ies), oldest first", entries.len());
    for entry in &entries {
        match &entry.problem {
            None => println!(
                "  {}  {:?}  account {}, deleted {}, {} file(s); {}",
                entry.name,
                entry.display_name,
                entry.account_id.0,
                entry.deleted_at_utc,
                entry.files,
                state_text(entry.state)
            ),
            Some(problem) => println!("  {}  account {}; {}; its manifest cannot be read: {problem}", entry.name, entry.account_id.0, state_text(entry.state)),
        }
    }
    ExitCode::SUCCESS
}

fn restore(dir: &Path, entry_arg: &str, dry_run: bool) -> ExitCode {
    if let Err(why) = check_state_dir(dir) {
        eprintln!("{why}");
        return ExitCode::FAILURE;
    }
    let scan_failed = |when: &str, e: io::Error| {
        eprintln!("cannot read the state directory {when}: {e}");
        ExitCode::FAILURE
    };
    let store = match open_store(dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let opened = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after opening it", e),
    };
    let entries = match store.trash_entries() {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("cannot read the trash: {e}; no file was written");
            return ExitCode::FAILURE;
        }
    };
    let entry = match pick_entry(&entries, entry_arg) {
        Ok(entry) => entry.clone(),
        Err(why) => {
            eprintln!("{why}; no file was written");
            return ExitCode::FAILURE;
        }
    };
    // The same checks the restore runs, before anything is written: the entry reads cleanly, the name is free, the
    // store has room.
    if let Err(e) = store.restore_check(&entry.name) {
        eprintln!("cannot restore {}: {e}; no file was written", entry.name);
        return ExitCode::FAILURE;
    }
    if dry_run {
        println!("dry run: nothing was written");
    }
    println!(
        "trash entry {}: {:?}, account {} deleted {}, {} file(s)",
        entry.name, entry.display_name, entry.account_id.0, entry.deleted_at_utc, entry.files
    );
    println!("restore: as a NEW account (the old id {} is retired and stays so) with a NEW token; the name {:?} is free", entry.account_id.0, entry.display_name);

    if dry_run {
        drop(store);
        let after = match scan_tree(dir) {
            Ok(tree) => tree,
            Err(e) => return scan_failed("after the dry run", e),
        };
        println!("verification (every file of the state directory, hashed before and after):");
        let untouched = verify_untouched(&opened, &after, &Allowed::default());
        print_untouched(&untouched);
        if !report_failures(&untouched.failures) {
            return ExitCode::FAILURE;
        }
        println!("result: dry run; no file was written");
        return ExitCode::SUCCESS;
    }

    let restored = match store.restore_account(&entry.name, now()) {
        Ok(restored) => restored,
        Err(e) => {
            eprintln!("cannot restore {}: {e}; the entry is as it was", entry.name);
            return ExitCode::FAILURE;
        }
    };
    drop(store);
    let after = match scan_tree(dir) {
        Ok(tree) => tree,
        Err(e) => return scan_failed("after the restore", e),
    };

    // The proof: the new account's records exist, the entry's records are byte for byte what they were (only its
    // manifest gained the plan), and every other file is exactly as it was.
    let new = restored.account_id.0;
    let (old_dir, new_dir) = (format!("trash/{}", entry.name), format!("trash/{}", restored.entry));
    let mut failures = Vec::new();
    println!("verification (every file of the state directory, hashed after the store was opened and after the restore):");
    let mut written = vec![format!("accounts/{new}.json")];
    written.extend(restored.avatar_id.map(|avatar| format!("avatars/{}.json", avatar.0)));
    for rel in after.keys().filter(|rel| rel.starts_with(&format!("characters/{new}.json")) || rel.starts_with(&format!("progression/{new}/"))) {
        written.push(rel.clone());
    }
    for rel in &written {
        match after.get(rel) {
            Some(Entry::File(digest)) => println!("  written: {rel} (sha256 {})", short(digest)),
            other => failures.push(format!("{rel} was not written ({})", describe(other))),
        }
    }
    let prefix = format!("{old_dir}/");
    let mut kept = 0usize;
    for (rel, was) in opened.iter().filter(|(rel, _)| rel.starts_with(&prefix)) {
        let inside = rel.strip_prefix(&prefix).unwrap_or(rel);
        if inside == "manifest.json" || matches!(was, Entry::Dir) {
            continue;
        }
        match after.get(&format!("{new_dir}/{inside}")) {
            Some(now_is) if now_is == was => kept += 1,
            other => failures.push(format!("{rel} in the trash was {} and is now {} in {new_dir}", describe(Some(was)), describe(other))),
        }
    }
    println!("  the trash entry kept its {kept} record file(s) byte for byte (as {new_dir})");
    if opened.get("index.log") == after.get("index.log") {
        failures.push("index.log did not change, so the account was not added to the index".to_owned());
    } else {
        println!("  index.log: changed (the new account {new} was appended to it)");
    }
    let mut allowed = Allowed::default();
    allowed.exact.extend(["index.log".to_owned(), old_dir.clone(), new_dir.clone(), format!("progression/{new}")]);
    allowed.exact.extend(written.iter().cloned());
    allowed.prefixes.extend([format!("{old_dir}/"), format!("{new_dir}/"), format!("progression/{new}/")]);
    let untouched = verify_untouched(&opened, &after, &allowed);
    print_untouched(&untouched);
    failures.extend(untouched.failures);
    if !report_failures(&failures) {
        return ExitCode::FAILURE;
    }
    if !restored.unfinished.is_empty() {
        println!("note: {} progress record(s) could not be written ({}); the next open of the store writes them", restored.unfinished.len(), restored.unfinished.join("; "));
    }
    println!("result: done; {:?} is account {new} again (the old id {} stays retired)", restored.display_name, entry.account_id.0);
    println!("token: {}", restored.token);
    println!(
        "(shown once; it is a password and the server keeps only its hash. The player's client keeps it in its state directory as accounts/<server>.<name>.token: \
         two lines, the token, then `name={}`)",
        restored.display_name
    );
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match parse(&args) {
        Ok(invocation) => invocation,
        Err(why) => {
            eprintln!("fmv-characters: {why}");
            return usage();
        }
    };
    match invocation.command {
        Command::Show { account, game } => show(&invocation.dir, &account, game),
        Command::Restyle { account, game, body, tint, dry_run } => restyle(&invocation.dir, &account, game, &body, tint, dry_run),
        Command::Delete { account, dry_run } => delete(&invocation.dir, &account, dry_run),
        Command::Trash => trash(&invocation.dir),
        Command::Restore { entry, dry_run } => restore(&invocation.dir, &entry, dry_run),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn hex(digest: &Digest) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Digests printed by `sha256sum` (uutils coreutils 0.8.0) for `a` repeated N times: lengths around every
    /// padding boundary (55 and 56 decide whether the length fits the last block, 63 to 65 and 119 to 129 are the
    /// block edges), plus the FIPS vectors and every byte value.
    #[test]
    fn sha256_matches_sha256sum_around_every_padding_boundary() {
        for (len, expected) in [
            (0usize, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (1, "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"),
            (3, "9834876dcfb05cb167a5c24953eba58c4ac89b1adf57f28f2f9d09af107ee8f0"),
            (55, "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"),
            (56, "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"),
            (57, "f13b2d724659eb3bf47f2dd6af1accc87b81f09f59f2b75e5c0bed6589dfe8c6"),
            (63, "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34"),
            (64, "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"),
            (65, "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0"),
            (119, "31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb"),
            (120, "2f3d335432c70b580af0e8e1b3674a7c020d683aa5f73aaaedfdc55af904c21c"),
            (127, "c57e9278af78fa3cab38667bef4ce29d783787a2f731d4e12200270f0c32320a"),
            (128, "6836cf13bac400e9105071cd6af47084dfacad4e5e302c94bfed24e013afb73e"),
            (129, "c12cb024a2e5551cca0e08fce8f1c5e314555cc3fef6329ee994a3db752166ae"),
            (1000, "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"),
        ] {
            let data = vec![b'a'; len];
            assert_eq!(hex(&sha256::of(&data)), expected, "{len} bytes");
        }
        assert_eq!(hex(&sha256::of(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256::of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let every_byte: Vec<u8> = (0..=255u8).collect();
        assert_eq!(hex(&sha256::of(&every_byte)), "40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880");
    }

    /// The input may arrive in any pieces (a file is read in chunks): the digest is the same.
    #[test]
    fn sha256_is_the_same_whatever_the_pieces() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let whole = sha256::of(&data);
        for piece in [1usize, 2, 3, 7, 55, 56, 63, 64, 65, 100, 999] {
            let mut hasher = sha256::Sha256::new();
            for chunk in data.chunks(piece) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish(), whole, "pieces of {piece}");
        }
        assert_eq!(short(&whole).len(), 12);
        assert!(hex(&whole).starts_with(&short(&whole)));
    }

    #[test]
    fn the_command_line_is_parsed_strictly() {
        let ok = parse(&args(&["/s", "restyle", "Blizz", "--body", "sturdy", "--tint", "5a7df5", "--dry-run", "--game", "FF7"])).unwrap();
        assert_eq!(
            ok,
            Invocation {
                dir: PathBuf::from("/s"),
                command: Command::Restyle { account: "Blizz".into(), game: GameId::Ff7, body: "sturdy".into(), tint: Some([0x5a, 0x7d, 0xf5]), dry_run: true }
            }
        );
        // Flags in any order, `--flag=value`, defaults.
        let other = parse(&args(&["/s", "restyle", "--body=lean", "7"])).unwrap();
        assert_eq!(
            other.command,
            Command::Restyle { account: "7".into(), game: GameId::Ff7, body: "lean".into(), tint: None, dry_run: false }
        );
        assert_eq!(parse(&args(&["/s", "show", "7", "--game", "fixture"])).unwrap().command, Command::Show { account: "7".into(), game: GameId::Fixture });
        assert_eq!(parse(&args(&["/s", "show", "Blizz"])).unwrap().command, Command::Show { account: "Blizz".into(), game: GameId::Ff7 });
        // Everything else is a usage error.
        for bad in [
            vec![],
            vec!["/s"],
            vec!["/s", "frobnicate"],
            vec!["/s", "show"],
            vec!["/s", "show", "a", "b"],
            vec!["/s", "show", "a", "--body", "lean"],
            vec!["/s", "restyle", "a"],
            vec!["/s", "restyle", "--body", "lean"],
            vec!["/s", "restyle", "a", "--body"],
            vec!["/s", "restyle", "a", "--body", "--dry-run"],
            vec!["/s", "restyle", "a", "--body", "lean", "--body", "sturdy"],
            vec!["/s", "restyle", "a", "--body", "lean", "--dry-run", "--dry-run"],
            vec!["/s", "restyle", "a", "--body", "lean", "--dry-run=yes"],
            vec!["/s", "restyle", "a", "--body", "lean", "--tint", "red"],
            vec!["/s", "restyle", "a", "--body", "lean", "--tint", "12345"],
            vec!["/s", "restyle", "a", "--body", "lean", "--tint", "12345g"],
            vec!["/s", "restyle", "a", "--body", "lean", "--tint", "+12345"],
            vec!["/s", "restyle", "a", "--body", "lean", "--game", "ff99"],
            vec!["/s", "restyle", "a", "--body", "lean", "--force"],
        ] {
            assert!(parse(&args(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_tint_is_six_hex_digits() {
        assert_eq!(parse_tint("000000"), Ok([0, 0, 0]));
        assert_eq!(parse_tint("FFa0Ff"), Ok([255, 160, 255]));
        assert_eq!(parse_tint("#5a7df5"), Ok([0x5a, 0x7d, 0xf5]));
        for bad in ["", "#", "fff", "5a7df5f", "5a7df", "gggggg", "5a 7d f", "５a7df5", "-5a7df5"] {
            assert!(parse_tint(bad).is_err(), "{bad:?}");
        }
        assert_eq!(tint_text([0x5a, 0x07, 0xf5]), "5a07f5");
    }

    fn file(content: &[u8]) -> Entry {
        Entry::File(sha256::of(content))
    }

    fn tree(entries: &[(&str, Entry)]) -> Tree {
        entries.iter().map(|(rel, entry)| ((*rel).to_owned(), entry.clone())).collect()
    }

    const REC: &str = "characters/1.json";

    #[test]
    fn the_verification_lists_every_unwritten_file_as_same_and_the_one_written() {
        let base = [
            ("accounts", Entry::Dir),
            ("accounts/1.json", file(b"account")),
            ("index.log", file(b"log")),
            ("progression", Entry::Dir),
            ("progression/1", Entry::Dir),
            ("progression/1/Ff7.character.json", file(b"numbers")),
        ];
        let start = tree(&base);
        let mut with_dir = start.clone();
        with_dir.insert("characters".into(), Entry::Dir);
        let mut written = with_dir.clone();
        written.insert(REC.into(), file(b"record"));

        let v = verify(&start, &with_dir, &written, REC, true);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        let same = format!("progression/1/Ff7.character.json sha256 {} -> same", short(&sha256::of(b"numbers")));
        assert!(v.lines.contains(&same), "{:?}", v.lines);
        assert!(v.lines.contains(&format!("accounts/1.json sha256 {} -> same", short(&sha256::of(b"account")))));
        assert!(v.lines.contains(&format!("written: {REC} (new file, sha256 {})", short(&sha256::of(b"record")))), "{:?}", v.lines);
        assert_eq!(v.lines.len(), 4, "three unwritten files and the written one: {:?}", v.lines);
        assert!(v.lines.last().is_some_and(|l| l.starts_with("written: ")), "the written file comes last: {:?}", v.lines);
        assert!(v.notes.iter().any(|n| n.contains("directory characters was created when the store was opened")), "{:?}", v.notes);

        // A replaced record shows both hashes.
        let mut replaced = written.clone();
        replaced.insert(REC.into(), file(b"record 2"));
        let v = verify(&written, &written, &replaced, REC, true);
        assert!(v.failures.is_empty());
        let line = format!("written: {REC} (sha256 {} -> sha256 {})", short(&sha256::of(b"record")), short(&sha256::of(b"record 2")));
        assert!(v.lines.contains(&line), "{:?}", v.lines);
    }

    #[test]
    fn a_dry_run_must_leave_every_file_alone_the_record_included() {
        let start = tree(&[("index.log", file(b"log")), ("characters", Entry::Dir)]);
        let v = verify(&start, &start, &start, REC, false);
        assert!(v.failures.is_empty());
        assert!(v.lines.contains(&format!("not written: {REC} (absent)")), "{:?}", v.lines);

        let mut wrote_anyway = start.clone();
        wrote_anyway.insert(REC.into(), file(b"record"));
        let v = verify(&start, &start, &wrote_anyway, REC, false);
        assert!(v.failures.iter().any(|f| f.contains("written although this run meant to write nothing")), "{:?}", v.failures);
    }

    /// The point of the block: a file that is not the record changing while the tool holds the store fails the run.
    #[test]
    fn a_file_other_than_the_record_changing_is_a_failure_and_opening_the_store_is_not_blamed_on_the_restyle() {
        let start = tree(&[("index.log", file(b"log")), ("progression/1/Ff7.character.json", file(b"numbers"))]);
        let mut touched = start.clone();
        touched.insert("progression/1/Ff7.character.json".into(), file(b"other numbers"));
        touched.insert(REC.into(), file(b"record"));
        let v = verify(&start, &start, &touched, REC, true);
        assert!(v.lines.iter().any(|l| l.starts_with("progression/1/Ff7.character.json") && l.ends_with("CHANGED")), "{:?}", v.lines);
        assert!(v.failures.iter().any(|f| f.contains("progression/1/Ff7.character.json changed while the tool held the store")), "{:?}", v.failures);

        // A removed file and an added file are changes too.
        let mut removed = start.clone();
        removed.remove("index.log");
        removed.insert("extra.json".into(), file(b"x"));
        removed.insert(REC.into(), file(b"record"));
        let v = verify(&start, &start, &removed, REC, true);
        assert_eq!(v.failures.len(), 2, "{:?}", v.failures);

        // What opening the store changed (a repaired log, say) is a note, not a failure.
        let mut opened = start.clone();
        opened.insert("index.log".into(), file(b"log, repaired"));
        let mut done = opened.clone();
        done.insert(REC.into(), file(b"record"));
        let v = verify(&start, &opened, &done, REC, true);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(v.notes.iter().any(|n| n.contains("index.log was changed when the store was opened")), "{:?}", v.notes);
        assert!(v.lines.iter().any(|l| l.contains("index.log") && l.contains("changed when the store was opened")), "{:?}", v.lines);

        // A record that was meant to be written and was not is a failure too.
        let v = verify(&start, &start, &start, REC, true);
        assert!(v.failures.iter().any(|f| f.contains("was not written although the restyle meant to")), "{:?}", v.failures);
    }

    #[test]
    fn the_tree_scan_hashes_files_lists_directories_and_leaves_the_lock_out() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("progression/7")).unwrap();
        fs::write(dir.path().join("LOCK"), b"123\n").unwrap();
        fs::write(dir.path().join("index.log"), b"abc").unwrap();
        fs::write(dir.path().join("progression/7/Ff7.json"), b"").unwrap();
        let scanned = scan_tree(dir.path()).unwrap();
        let names: Vec<&str> = scanned.keys().map(String::as_str).collect();
        assert_eq!(names, ["index.log", "progression", "progression/7", "progression/7/Ff7.json"]);
        assert_eq!(scanned.get("index.log"), Some(&Entry::File(sha256::of(b"abc"))));
        assert_eq!(scanned.get("progression/7/Ff7.json"), Some(&Entry::File(sha256::of(b""))));
        assert_eq!(scanned.get("progression"), Some(&Entry::Dir));
    }

    #[test]
    fn a_path_that_is_not_a_state_directory_is_stopped_before_the_store_could_make_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(check_state_dir(&missing).unwrap_err().contains("cannot read"));
        assert!(check_state_dir(dir.path()).unwrap_err().contains("no index.log"));
        let file = dir.path().join("a-file");
        fs::write(&file, b"x").unwrap();
        assert!(check_state_dir(&file).unwrap_err().contains("not a directory"));
        fs::write(dir.path().join("index.log"), b"x").unwrap();
        assert!(check_state_dir(dir.path()).is_ok());
    }

    // ---- delete, trash and restore

    #[test]
    fn the_delete_trash_and_restore_commands_are_parsed_strictly() {
        let delete = parse(&args(&["/s", "delete", "Blizz", "--dry-run"])).unwrap();
        assert_eq!(delete.command, Command::Delete { account: "Blizz".into(), dry_run: true });
        assert_eq!(parse(&args(&["/s", "delete", "7"])).unwrap().command, Command::Delete { account: "7".into(), dry_run: false });
        assert_eq!(parse(&args(&["/s", "trash"])).unwrap().command, Command::Trash);
        assert_eq!(
            parse(&args(&["/s", "restore", "20261109T165905Z-7"])).unwrap().command,
            Command::Restore { entry: "20261109T165905Z-7".into(), dry_run: false }
        );
        assert_eq!(parse(&args(&["/s", "restore", "--dry-run", "Aria Vale"])).unwrap().command, Command::Restore { entry: "Aria Vale".into(), dry_run: true });
        for bad in [
            vec!["/s", "delete"],
            vec!["/s", "delete", "a", "b"],
            vec!["/s", "delete", "a", "--dry-run=yes"],
            vec!["/s", "delete", "a", "--dry-run", "--dry-run"],
            vec!["/s", "delete", "a", "--game", "ff7"],
            vec!["/s", "delete", "a", "--force"],
            vec!["/s", "trash", "extra"],
            vec!["/s", "trash", "--dry-run"],
            vec!["/s", "restore"],
            vec!["/s", "restore", "a", "b"],
            vec!["/s", "restore", "a", "--body", "lean"],
        ] {
            assert!(parse(&args(&bad)).is_err(), "{bad:?}");
        }
    }

    fn entry(name: &str, state: TrashState, account: u64, display: &str) -> TrashEntry {
        TrashEntry {
            name: name.into(),
            state,
            account_id: AccountId(account),
            display_name: display.into(),
            deleted_at: 0,
            deleted_at_utc: "1970-01-01T00:00:00Z".into(),
            files: 5,
            problem: None,
        }
    }

    #[test]
    fn a_trash_entry_is_found_by_its_name_the_id_it_had_or_the_characters_name_and_never_guessed() {
        let aria = entry("20261109T100000Z-7", TrashState::Deleted, 7, "Aria Vale");
        let aria_again = entry("20261109T120000Z-9", TrashState::Deleted, 9, "ARIA VALE");
        let brom = entry("20261109T110000Z-8", TrashState::Deleted, 8, "Brom");
        let done = entry("20261109T090000Z-3", TrashState::Restored { as_account: AccountId(12) }, 3, "Cid");
        let stuck = entry("20261109T080000Z-4.pending", TrashState::Pending, 4, "Dyne");
        let mut broken = entry("20261109T070000Z-5", TrashState::Deleted, 5, "");
        broken.problem = Some("its manifest cannot be read".into());
        let all = [aria.clone(), brom.clone(), aria_again.clone(), done, stuck, broken];
        let name_of = |arg: &str| pick_entry(&all, arg).map(|e| e.name.clone());
        // by the exact entry name, by the id the deleted account had, by the character's name (case and width do not matter)
        assert_eq!(name_of("20261109T100000Z-7").unwrap(), aria.name);
        assert_eq!(name_of("8").unwrap(), brom.name);
        assert_eq!(name_of("brom").unwrap(), brom.name);
        assert_eq!(name_of("BROM").unwrap(), brom.name);
        // two characters of one name: the operator is asked to say which, with the candidates named
        let err = name_of("aria vale").unwrap_err();
        assert!(err.contains("matches 2 trash entries") && err.contains(&aria.name) && err.contains(&aria_again.name), "{err}");
        assert_eq!(name_of("20261109T120000Z-9").unwrap(), aria_again.name, "an exact entry name settles it");
        assert_eq!(name_of("9").unwrap(), aria_again.name, "and so does the id");
        // an entry that is restored, in flight or unreadable is not offered, and nothing else matches
        for arg in ["Cid", "3", "20261109T090000Z-3", "Dyne", "4", "20261109T080000Z-4.pending", "5", "20261109T070000Z-5", "nobody", "", "99", " "] {
            let err = name_of(arg).unwrap_err();
            assert!(err.contains("no restorable trash entry matches"), "{arg:?}: {err}");
        }
    }

    #[test]
    fn the_untouched_check_fails_on_any_change_outside_the_allowed_paths() {
        let start = tree(&[
            ("accounts", Entry::Dir),
            ("accounts/1.json", file(b"one")),
            ("accounts/2.json", file(b"two")),
            ("index.log", file(b"log")),
            ("progression", Entry::Dir),
            ("progression/2", Entry::Dir),
            ("progression/2/Ff7.json", file(b"story")),
        ]);
        let mut allowed = Allowed::default();
        allowed.exact.extend(["index.log".to_owned(), "accounts/1.json".to_owned(), "trash".to_owned()]);
        allowed.prefixes.push("trash/entry/".to_owned());
        // account 1 left, the trash and the log grew: all allowed, the rest is counted as unchanged
        let mut moved = start.clone();
        moved.remove("accounts/1.json");
        moved.insert("index.log".into(), file(b"log + release"));
        moved.insert("trash".into(), Entry::Dir);
        moved.insert("trash/entry".into(), Entry::Dir);
        moved.insert("trash/entry/accounts".into(), Entry::Dir);
        moved.insert("trash/entry/accounts/1.json".into(), file(b"one"));
        let ok = verify_untouched(&start, &moved, &allowed);
        // (`trash/entry` itself is a directory the allowed list names only as a prefix of its contents: it is a failure
        // unless it is named exactly, which is why the tool names it)
        assert_eq!(ok.failures.len(), 1, "{:?}", ok.failures);
        allowed.exact.insert("trash/entry".to_owned());
        let ok = verify_untouched(&start, &moved, &allowed);
        assert!(ok.failures.is_empty(), "{:?}", ok.failures);
        assert_eq!((ok.files, ok.dirs), (2, 3), "accounts/2.json and progression/2/Ff7.json; accounts, progression, progression/2");

        // another account's record changing, disappearing or appearing is a failure each
        for (what, mutate) in [
            ("changed", Box::new(|t: &mut Tree| { t.insert("accounts/2.json".into(), file(b"TWO")); }) as Box<dyn Fn(&mut Tree)>),
            ("removed", Box::new(|t: &mut Tree| { t.remove("progression/2/Ff7.json"); })),
            ("added", Box::new(|t: &mut Tree| { t.insert("accounts/3.json".into(), file(b"three")); })),
            ("replaced by a directory", Box::new(|t: &mut Tree| { t.insert("accounts/2.json".into(), Entry::Dir); })),
        ] {
            let mut bad = moved.clone();
            mutate(&mut bad);
            let found = verify_untouched(&start, &bad, &allowed);
            assert_eq!(found.failures.len(), 1, "{what}: {:?}", found.failures);
            assert!(found.failures[0].contains("not one of the files this run may change"), "{what}: {:?}", found.failures);
        }
        // a path that merely starts like an allowed one is not allowed (`trash/entry-2` is not under `trash/entry/`)
        let mut sneaky = moved.clone();
        sneaky.insert("trash/entry-2".into(), Entry::Dir);
        assert_eq!(verify_untouched(&start, &sneaky, &allowed).failures.len(), 1);
    }

    #[test]
    fn states_are_described_in_words() {
        assert_eq!(state_text(TrashState::Deleted), "deleted");
        assert_eq!(state_text(TrashState::Restored { as_account: AccountId(12) }), "restored as account 12");
        assert!(state_text(TrashState::Pending).contains("interrupted"));
        assert!(state_text(TrashState::Restoring { as_account: AccountId(3) }).contains("as account 3"));
    }
}
