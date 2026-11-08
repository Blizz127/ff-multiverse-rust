//! fmv-ff7 — FF7 (PC) adapter: format decoders and the FF7 area runtime.
//!
//! **UNVERIFIED AGAINST OWNED MEDIA.** No owned FF7 data existed on the machine where this
//! crate was written. Every format layout comes from public community documentation (URLs
//! are cited in each module) that was fetched through a summarising tool, not copied
//! verbatim, and cross-checked between two or more sources where each module says so.
//! Nothing has been run against real game files; all tests use small synthetic data built
//! in test code. Correctness must be confirmed on the owner's own files.
//!
//! Modules:
//!
//! * [`lgp`]   - LGP archive reader over any `Read + Seek` (a library blob path works).
//! * [`lzss`]  - FF7 LZSS decompressor (plus a test-only compressor).
//! * [`field`] - decompressed PC field file: sections, walkmesh, camera, palette,
//!   triggers/gateways, background tiles.
//! * [`area`]  - [`Ff7Adapter`] / [`Ff7FieldArea`] implementing the `fmv-adapter-api`
//!   traits over a `flevel.lgp` source. Synthetic provenance is labelled FIXTURE; the area
//!   fingerprint binds the edition provenance and is defined by [`area_fingerprint`].
//!   Movement is validated against the walkmesh *and* its edge-access table (internal
//!   `walk` module; see the `area` docs for the rule).
//!
//! # Wiring notes for the integrator
//!
//! * Call [`Ff7Adapter::prewarm`] on a blocking thread (for example `spawn_blocking`) before
//!   networking starts: the first `list_areas()` reads the whole archive.
//! * Fold [`adapter_version`] into the client/server build strings so two builds with
//!   different fingerprint rules ([`FINGERPRINT_RULE_VERSION`]) are not mistaken for two
//!   players with different media.
//! * Build the provenance honestly: `Platform::Synthetic` (and `SourceRef::Fixture`) for
//!   synthetic FF7-format data, which makes every area FIXTURE.
//!
//! Hard rules this crate follows: originals are opened read-only and never modified; no
//! game asset bytes are logged, serialised or sent anywhere; untrusted input (archives,
//! compressed streams, field files) is parsed without panicking and with every allocation
//! bounded.

#![forbid(unsafe_code)]
// Untrusted-data hygiene, enforced by `cargo clippy -p fmv-ff7`: no unwrap/expect/panic or
// unchecked indexing in non-test code.
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

pub mod area;
pub mod field;
pub mod lgp;
pub mod lzss;

mod util;
mod walk;

#[cfg(test)]
mod testutil;

pub use area::{
    adapter_version, area_fingerprint, Ff7Adapter, Ff7FieldArea, SkippedEntry, FF7_FIELD_MAX_SPEED_PLACEHOLDER,
    FINGERPRINT_RULE_VERSION, MAX_FIELD_BYTES, MAX_INDEX_READ_BYTES,
};
pub use walk::LinkReport;

/// Status string every surface that presents FF7 data from this crate should be able to show
/// until the decoders have been confirmed against owned media.
pub const VERIFICATION_STATUS: &str = "UNVERIFIED AGAINST OWNED MEDIA";
