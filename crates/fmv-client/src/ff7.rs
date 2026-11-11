//! Opening the FF7 adapter over this machine's own content.
//!
//! The client never receives game files: it reads its own copy and only compares fingerprints
//! with the server's offer (see `net::check_local_content`). Finding that copy is
//! `fmv_library::ff7`, the same code the server uses for its copy, because the area
//! fingerprint binds the provenance that resolution returns (a client that picked another
//! library set than the server's would be refused at join). This module turns the result into
//! an [`Ff7Adapter`] and indexes it.
//!
//! Everything here opens files read-only. Local paths appear in errors shown on this machine
//! only; they are never put on the wire (`net` sends path-free reasons).

use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, GameAdapter};
use fmv_core::GameId;
use fmv_ff7::Ff7Adapter;
pub use fmv_library::ff7::{movie_file, resolve_battle_lgp, resolve_char_lgp, resolve_flevel, resolve_movies_dir, Ff7Labels, Ff7ResolveError, Ff7Source};

#[derive(Debug, thiserror::Error)]
pub enum Ff7OpenError {
    #[error(transparent)]
    Resolve(#[from] Ff7ResolveError),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

/// An opened adapter, shareable between the network thread and the window (see [`SharedFf7`]).
#[derive(Clone)]
pub struct OpenedFf7 {
    pub adapter: Arc<Ff7Adapter>,
    pub origin: String,
}

impl std::fmt::Debug for OpenedFf7 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedFf7").field("origin", &self.origin).finish_non_exhaustive()
    }
}

/// Resolve and open the adapter (reads only the archive's table of contents; call
/// [`prewarm`] to build the area index).
pub fn open_ff7(source: &Ff7Source) -> Result<OpenedFf7, Ff7OpenError> {
    let resolved = resolve_flevel(source)?;
    let adapter = Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance)?;
    // The character models come from the same library set (or directory) as the field archive.
    // Missing models are no reason not to play: the window falls back to markers.
    let char_lgp = match resolve_char_lgp(source) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("FF7: no character models ({e}); players will be drawn as markers");
            None
        }
    };
    eprintln!(
        "FF7: character models: {}",
        if char_lgp.is_some() { "this machine's own char.lgp found" } else { "no char.lgp (players are drawn as markers)" }
    );
    // The window, several layers away, finds it again here (see `avatar::source`).
    crate::avatar::source::remember(char_lgp);
    // The battle models likewise (the same library set again); without them the battle scene draws
    // its stand-ins (see `battle::scene::model`).
    let battle_lgp = match resolve_battle_lgp(source) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("FF7: no battle models ({e}); battles are drawn with stand-ins");
            None
        }
    };
    eprintln!(
        "FF7: battle models: {}",
        if battle_lgp.is_some() { "this machine's own battle.lgp found" } else { "no battle.lgp (battles are drawn with stand-ins)" }
    );
    crate::battle::scene::model::remember_battle_lgp(battle_lgp);
    // The clips the enemies' attacks play are named by `scene.bin` (presentation only, read on this
    // machine like the models; a direct `flevel.lgp` has none and the enemies then play no attack clip).
    let scene_bin = fmv_library::ff7::resolve_battle_files(source).ok().map(|files| files.scene);
    crate::battle::scene::model::remember_scene_bin(scene_bin);
    Ok(OpenedFf7 { adapter: Arc::new(adapter), origin: resolved.origin })
}

/// What [`prewarm`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrewarmReport {
    /// Areas the archive offers.
    pub areas: usize,
    /// Archive entries that are not offered (the name list, unusable names, unreadable entries).
    pub skipped: usize,
    pub elapsed: Duration,
}

/// Build the adapter's area index (it reads and hashes the whole archive, which is why this runs
/// before the connection and its handshake deadline exist, not inside them).
pub fn prewarm(adapter: &Ff7Adapter) -> PrewarmReport {
    let started = Instant::now();
    let areas = adapter.prewarm();
    PrewarmReport { areas, skipped: adapter.skipped_entries().len(), elapsed: started.elapsed() }
}

/// `GameAdapter` over a shared [`Ff7Adapter`]: the network thread owns a `Box<dyn GameAdapter>`
/// while the window keeps its own `Arc` to read render data from the same archive.
pub struct SharedFf7(pub Arc<Ff7Adapter>);

impl GameAdapter for SharedFf7 {
    fn game(&self) -> GameId {
        self.0.game()
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        self.0.list_areas()
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        self.0.load_area(key)
    }
}

/// The adapter's identity for the build string sent in `Hello` (crate version and fingerprint
/// rule version, `fmv_ff7::adapter_version`, as one whitespace-free token). Two builds with
/// different fingerprint rules compute different fingerprints for identical media, so a mismatch
/// must be attributable to the builds and not to the players.
pub fn build_tag() -> String {
    fmv_ff7::adapter_version().split_whitespace().collect::<Vec<_>>().join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_archive_is_an_adapter_error_without_a_path() {
        let err = open_ff7(&Ff7Source::Flevel { path: "/definitely/not/here/flevel.lgp".into(), labels: Ff7Labels::default() })
            .unwrap_err();
        assert!(matches!(err, Ff7OpenError::Adapter(AdapterError::MissingContent(_))), "{err}");
        assert!(!err.to_string().contains("definitely"), "{err}");
    }

    #[test]
    fn library_problems_surface_as_resolve_errors_that_point_at_fmv_import() {
        let empty = std::env::temp_dir().join("fmv-client-test-no-such-library-dir");
        let err = open_ff7(&Ff7Source::Library { state_dir: Some(empty), set: None }).unwrap_err();
        assert!(matches!(err, Ff7OpenError::Resolve(Ff7ResolveError::NoLibrary { .. })), "{err}");
        assert!(err.to_string().contains("needs owned FF7 content imported via fmv-import"), "{err}");
    }

    #[test]
    fn the_build_tag_is_one_token() {
        let t = build_tag();
        assert!(t.starts_with("fmv-ff7/"), "{t}");
        assert!(!t.contains(char::is_whitespace), "{t}");
        assert!(t.contains("fingerprint-rule/"), "{t}");
    }
}
