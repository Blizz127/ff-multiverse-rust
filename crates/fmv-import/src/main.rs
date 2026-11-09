//! `fmv-import`: import owned FF7 data into the FF Multiverse library.

#![deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fmv_import::commands::{
    import_disc, import_pc, import_status, list_sets, remove_set, ImportDiscArgs, ImportPcArgs,
    RemoveSetArgs,
};
use fmv_import::scan::scan_pc;
use serde::Serialize;

/// Import owned FF7 data (a PC install or a PS1 disc set) into the FF Multiverse library.
///
/// Originals (installs, disc images, cue sheets) are opened read-only and never modified; all
/// writes go under the state directory. Imported content is stored content-addressed
/// (blake3) with a provenance manifest per edition; different editions, regions and revisions
/// never merge.
///
/// Physical drive raw reading is out of scope in this increment: only image files
/// (.cue, .bin, .iso) can be imported. Every command prints JSON on stdout. FF7 identification
/// (edition guesses, the disc serial table) is UNVERIFIED [verify]: it is labelled as such in the
/// output and recorded as unverified in each set's manifest. A library set holds ONE language
/// (PC installs with several languages need --language, except that an install which says
/// nothing about it, like the Steam Edition, defaults to en with a warning) and one disc family;
/// discs the serial table does not know get a set private to their import session, and images
/// whose volume label starts with SYNTHETIC are imported as FIXTURE content, never as FF7.
///
/// PC installs: pass the install root of the Steam Edition (the folder named FINAL FANTASY VII
/// Steam Edition) or the folder that holds data/ (ff7/workingdir for the Steam Edition); the data
/// is found automatically. The Steam Edition is recognised from its files and, when the install
/// sits in a Steam library, from Steam's appmanifest_3837340.acf (the only file whose contents
/// are read): its build becomes the revision label.
///
/// Symbolic links: inside a PC install a link that stays within the directory given is followed;
/// one that leads out of it is refused (scan-pc lists it under refused_links). A disc image, cue
/// sheet or .bin may be a link: the file it leads to is what is read and guarded.
///
/// Audio: a PC install's music and sound files (data/music_ogg/*.ogg, data/music/*.wav and
/// music.idx, data/sound/audio.dat and audio.fmt) are imported too when it has them (scan-pc
/// summarises them under audio); plain file names only, at most 512 files. To add audio to a
/// library that already holds the install, run import-pc again on the same install with the same
/// --edition/--region/--language/--revision labels: that adds the audio to the set that exists,
/// while other labels make a second set, which the client then refuses as ambiguous.
///
/// Limits: a disc image may not list files that share sectors, more than 20000 files, or more
/// than 1 GiB of files (a CD holds less); a library manifest is capped at 64 MiB, in both
/// directions (an import that would exceed it is refused; nothing is written).
///
/// Imported discs are recorded by content: importing a different image under a disc number the
/// library already holds is refused ("different disc content") unless --replace-disc is given.
/// remove-set drops a set, even a broken one (blobs are kept, originals are never touched).
/// An empty --state-dir (or FMV_STATE_DIR) is an error, not the default directory.
#[derive(Parser, Debug)]
#[command(name = "fmv-import", version)]
struct Cli {
    /// State directory [default: $FMV_STATE_DIR, else <user data dir>/ff-multiverse]
    #[arg(long, global = true, value_name = "DIR")]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Read-only scan of an FF7 PC install; prints a JSON report. Writes nothing and never touches the state directory
    ///
    /// The report lists what an import would take, including the install's music and sound files
    /// (data/music_ogg, data/music, data/sound) when it has them; the audio key at the end
    /// summarises them.
    ScanPc {
        /// FF7 PC install: the folder that holds data/ (for the Steam Edition: ff7/workingdir), or
        /// the Steam Edition's install root, which is searched for ff7/workingdir/data
        dir: PathBuf,
    },
    /// Import the needed files of an FF7 PC install into a library set (refuses without flevel.lgp)
    ///
    /// The install's music and sound files (data/music_ogg, data/music, data/sound) are imported
    /// too when it has them. To add audio to a library that already holds the install, run
    /// import-pc again on the same install with the same --edition/--region/--language/--revision
    /// labels: that adds the audio to the existing set and changes nothing else in it. Other
    /// labels make a second set, which the client then refuses as ambiguous.
    ImportPc {
        /// FF7 PC install (opened read-only): the folder that holds data/ (for the Steam Edition:
        /// ff7/workingdir), or the Steam Edition's install root, which is searched for
        /// ff7/workingdir/data
        dir: PathBuf,
        /// Edition label, overriding the scan's guess (e.g. "PC 1998", "Steam 2013"). Default:
        /// the guess, which is "Steam Edition (appid 3837340)" for that edition
        #[arg(long)]
        edition: Option<String>,
        /// Region label (default "unknown")
        #[arg(long)]
        region: Option<String>,
        /// Language to import (e.g. en). A set holds exactly one language: only that language's
        /// files plus the language-neutral ones are imported; run again for another language.
        /// Required when the install holds several and nothing picks one (several ff7_<xx>.exe,
        /// or several data/lang-<xx> without en). Default: the language the install implies (one
        /// ff7_<xx>.exe, or one data/lang-<xx>); en, with a warning, when it holds several
        /// languages, no ff7_<xx>.exe names one and en is among them (the Steam Edition); else
        /// "unknown". Must match what the install holds; on an install without language markers
        /// it is just a label
        #[arg(long)]
        language: Option<String>,
        /// Revision label. Default: "build <id>" read from Steam's appmanifest of the Steam
        /// Edition when it is found, else "unknown"
        #[arg(long)]
        revision: Option<String>,
    },
    /// Import one disc of a PS1 disc set from an image file (.cue, .bin or .iso). Physical drives are out of scope in this increment
    ///
    /// Known deviation from "copy all files": XA/STR stream files (CD-XA Mode 2 Form 2: FMV,
    /// audio) are skipped and reported, because they cannot be read as 2048-byte sectors; the
    /// field and battle data the milestones need is not among them. import-status reports
    /// complete = true once every disc is in, and all_files_copied = false while streams were
    /// skipped.
    ImportDisc {
        /// Disc image: a .cue sheet, a raw 2352-byte .bin (MODE1/MODE2) or a plain 2048-byte .iso
        image: PathBuf,
        /// Name of the import session; the first disc fixes its edition/region/language, later discs must match
        #[arg(long, value_name = "NAME")]
        set_name: String,
        /// Disc number, for serials the (unverified) table does not know
        #[arg(long, value_name = "N")]
        disc: Option<u8>,
        /// Revision label (fixed by the first disc of the session)
        #[arg(long)]
        revision: Option<String>,
        /// Let this image take the place of whatever the library (and the session) records under
        /// its disc number: use it when the earlier import of that number was the wrong disc, a
        /// damaged rip or a crafted image (without it such an image is refused as "different
        /// disc content"). The old files leave the set only once this image has been read in
        /// full; their blobs stay in the store
        #[arg(long)]
        replace_disc: bool,
    },
    /// Show which discs of an import session are imported and which are still missing (FF7 has 3)
    ImportStatus {
        /// Import session name (the --set-name given to import-disc)
        name: String,
    },
    /// List library sets with a provenance summary (path-free unless --show-paths is given)
    List {
        /// Also print the install paths recorded in each set's sources
        #[arg(long)]
        show_paths: bool,
    },
    /// Remove a library set, even a broken one (damaged or oversized manifest), and the import sessions that point at it. Blobs are kept and originals are never touched
    ///
    /// Without --yes nothing is changed and the command only says what it would remove. It
    /// refuses while an import is writing the set. Re-import afterwards to rebuild the set: the
    /// blobs that were kept are reused.
    RemoveSet {
        /// Id of the set, as `list` shows it
        set_id: String,
        /// Confirm the removal
        #[arg(long)]
        yes: bool,
    },
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).context("serialising the result")?;
    let mut out = io::stdout().lock();
    match writeln!(out, "{text}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn run(cli: Cli) -> Result<()> {
    // An empty value is a mistake (an unset shell variable), never "use the default library":
    // clap refuses an empty --state-dir, and the library refuses an empty $FMV_STATE_DIR.
    let state_dir = cli.state_dir;
    match cli.command {
        Command::ScanPc { dir } => {
            // deliberately does not look at the state directory at all
            let report = scan_pc(&dir).with_context(|| format!("scanning {}", dir.display()))?;
            print_json(&report)
        }
        Command::ImportPc {
            dir,
            edition,
            region,
            language,
            revision,
        } => {
            let args = ImportPcArgs {
                dir,
                state_dir,
                edition,
                region,
                language,
                revision,
            };
            print_json(&import_pc(&args, now_unix())?)
        }
        Command::ImportDisc {
            image,
            set_name,
            disc,
            revision,
            replace_disc,
        } => {
            let args = ImportDiscArgs {
                image,
                set_name,
                state_dir,
                disc,
                revision,
                replace_disc,
            };
            print_json(&import_disc(&args, now_unix())?)
        }
        Command::ImportStatus { name } => print_json(&import_status(&name, state_dir.as_deref())?),
        Command::List { show_paths } => print_json(&list_sets(state_dir.as_deref(), show_paths)?),
        Command::RemoveSet { set_id, yes } => {
            let args = RemoveSetArgs {
                set_id,
                state_dir,
                confirmed: yes,
            };
            print_json(&remove_set(&args)?)
        }
    }
}

/// The error and its causes on one line. Library errors already put their I/O cause into their own
/// message, so a cause whose text is already contained in what was printed is not repeated.
fn render_error(e: &anyhow::Error) -> String {
    let mut out = e.to_string();
    for cause in e.chain().skip(1) {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
    }
    out
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {}", render_error(&e));
            ExitCode::from(1)
        }
    }
}
