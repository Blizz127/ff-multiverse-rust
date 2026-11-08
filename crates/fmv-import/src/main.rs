//! `fmv-import`: import owned FF7 data into the FF Multiverse library.

#![deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fmv_import::commands::{
    import_disc, import_pc, import_status, list_sets, ImportDiscArgs, ImportPcArgs,
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
/// (PC installs with several languages need --language) and one disc family; discs the serial
/// table does not know get a set private to their import session, and images whose volume label
/// starts with SYNTHETIC are imported as FIXTURE content, never as FF7.
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
    ScanPc {
        /// FF7 PC install directory
        dir: PathBuf,
    },
    /// Import the needed files of an FF7 PC install into a library set (refuses without flevel.lgp)
    ImportPc {
        /// FF7 PC install directory (opened read-only)
        dir: PathBuf,
        /// Edition label, overriding the scan's guess (e.g. "PC 1998", "Steam 2013")
        #[arg(long)]
        edition: Option<String>,
        /// Region label (default "unknown")
        #[arg(long)]
        region: Option<String>,
        /// Language to import (e.g. en). A set holds exactly one language: required when the
        /// install holds several (ff7_<xx>.exe / data/lang-<xx>), and then only that language's
        /// files plus the language-neutral ones are imported; run again for another language.
        /// Default: the one the install implies, else "unknown". Must match what the install
        /// holds; on an install without language markers it is just a label
        #[arg(long)]
        language: Option<String>,
        /// Revision label (default "unknown")
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
        } => {
            let args = ImportDiscArgs {
                image,
                set_name,
                state_dir,
                disc,
                revision,
            };
            print_json(&import_disc(&args, now_unix())?)
        }
        Command::ImportStatus { name } => print_json(&import_status(&name, state_dir.as_deref())?),
        Command::List { show_paths } => print_json(&list_sets(state_dir.as_deref(), show_paths)?),
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
