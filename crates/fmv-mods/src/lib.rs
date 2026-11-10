//! fmv-mods: optional community-mod layers over the owner's own FF7 content.
//!
//! The architecture is the owner's: *the accurate base game comes first; the mods are layers over it and can be
//! switched off.* This crate holds what the layers need and nothing the game itself does:
//!
//! * [`iro`]: a bounded, read-only reader for 7th Heaven's `.iro` mod archives.
//! * [`modxml`]: which mod an archive is (the ID in its `mod.xml`).
//! * [`ffnx`]: the FFNx audio-engine `config.toml` subset the sound mods use to map the game's names to their clips.
//! * [`echo`]: Echo-S's folders and the file names FFNx looks its voices up by.
//! * [`mods`]: [`Mods`], the opened layers with a switch each, and the lookups: the text layer's script sections
//!   ([`ScriptSource`]), voices, music and sound effects. Everything a layer lacks is `None`, and the caller plays the original.
//!
//! Hard rules this crate follows: the archives are opened read-only and are never extracted, copied or
//! redistributed; no mod bytes and no mod text are logged, stored in a test or put in a commit; every parse of
//! an archive is bounded and never panics on any bytes.

#![forbid(unsafe_code)]
// Untrusted-data hygiene, enforced by `cargo clippy -p fmv-mods`: no unwrap/expect/panic or unchecked
// indexing in non-test code.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]

pub mod compare;
pub mod echo;
pub mod ffnx;
pub mod iro;
pub mod modxml;
pub mod mods;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use iro::{normalize, Compression, IroArchive, IroEntry, IroError};
pub use mods::{Ambience, Clip, Layers, ModSources, Mods, ScriptSource, SfxSet};
