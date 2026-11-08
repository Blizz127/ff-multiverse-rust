//! fmv-import: the owned-content import pipeline (plan section 7.3).
//!
//! - [`scan`]: read-only identification of an FF7 PC install (edition guess, mods, M1/M2 files);
//!   it accepts the folder that holds `data/` or the install root of the Steam Edition;
//! - [`steam`]: read-only reading of Steam's `appmanifest_<appid>.acf` (appid, install folder,
//!   build), the evidence that lets the scan name the Steam Edition with high confidence;
//! - [`iso9660`] + [`image`] + [`cue`]: a read-only disc reader (plain 2048-byte `.iso`, raw
//!   2352-byte MODE1/MODE2 `.bin`, `.cue` sheets);
//! - [`serial`]: SYSTEM.CNF boot-serial parsing and the (UNVERIFIED) FF7 serial table;
//! - [`session`]: sequential multi-disc import sessions;
//! - [`commands`]: the import commands behind the `fmv-import` binary.
//!
//! Physical drives are out of scope in this increment: only image files can be imported.
//! Everything read from images, archives or install trees is untrusted and must never panic the
//! process; the lint attributes below turn that rule into a compile-time check (run clippy).
//!
//! # Rules the importer keeps
//!
//! - **One language per PC set.** An install with several languages (`ff7_<xx>.exe`,
//!   `data/lang-<xx>/`) is imported one language at a time (`--language`); a set gets the
//!   language-neutral files plus that language's own, never another language's. Adapters reading
//!   a set should look in `data/lang-<set language>/` first and fall back to the classic `data/`.
//!   Treating the classic layout as language-neutral is an assumption on installs that also have
//!   language directories, and the manifest says so. An install that holds several languages
//!   and nothing that says which one is meant (the Steam Edition has no per-language
//!   executables) is imported as `en` when `en` is among them, with a warning in the report and
//!   a caveat in the manifest; the language is still never mixed.
//! - **Labels that fix compatibility never drift.** Edition, region and revision are part of the
//!   set id and of the area fingerprint that decides which players share an instance. The
//!   Steam Edition is labelled `Steam Edition (appid 3837340)` / `unknown` / `build <id>`
//!   (the build from Steam's appmanifest), exactly as `--edition`/`--revision` spell them, so
//!   the auto-detected path reaches the sets that explicit labels made.
//! - **A guess is never stored as a fact.** Edition, region, language and the FF7 serial table are
//!   unverified; each manifest carries an [`Identification`](fmv_library::Identification) saying
//!   so. A serial the table does not know is ASSUMED to be FF7, its region is `unknown` (the
//!   serial prefix is kept only as a separate hint), and its set is private to its import
//!   session. An image whose volume label starts with `SYNTHETIC` is FIXTURE content.
//! - **A refusal leaves no trace; a failure leaves a consistent trace.** Everything that can be
//!   refused from the source and the saved sessions is refused before anything is written. While
//!   a disc is copied, its session record exists before any of its files reach a manifest.
//! - **Known deviation: XA/STR streams are not copied.** CD-XA Mode 2 Form 2 files (FMV, audio)
//!   cannot be read as 2048-byte sectors and are skipped and reported. A session is `complete`
//!   when every disc is in; `all_files_copied` is the stricter flag.

#![deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(
    test,
    allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)
)]

pub mod commands;
pub mod cue;
pub mod image;
pub mod iso9660;
pub mod scan;
pub mod serial;
pub mod session;
pub mod steam;
/// SYNTHETIC test-data builders (fake ISO images). Never game data, never surfaced by the CLI.
#[doc(hidden)]
pub mod synthetic;
