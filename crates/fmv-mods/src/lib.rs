//! fmv-mods: optional community-mod layers over the owner's own FF7 content.
//!
//! The architecture is the owner's: *the accurate base game comes first; the mods are layers over it and can be
//! switched off.* This crate holds what the layers need and nothing the game itself does:
//!
//! * [`iro`]: a bounded, read-only reader for 7th Heaven's `.iro` mod archives.
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

pub mod iro;

pub use iro::{normalize, Compression, IroArchive, IroEntry, IroError};
