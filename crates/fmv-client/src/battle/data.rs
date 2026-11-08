//! This machine's own battle data: `scene.bin` and `kernel.bin` of the same library set as the
//! field archive (`fmv_library::ff7::resolve_battle_files`, the resolver the server uses for its
//! copy), decoded read-only. The server never sends any of it; the client only compares the
//! fingerprint the server announces with its own (see `BattleClient`).

use super::names::NameBook;
use crate::ff7::Ff7Source;
use fmv_protocol::{BattleRules, Ff7BattleData};
use std::sync::Arc;

/// Open this machine's battle rules, with a path-free description of where they came from. The
/// error text is for the user of this machine (it may name a library directory).
pub fn open_rules(source: &Ff7Source) -> Result<(Arc<dyn BattleRules>, String), String> {
    let files = fmv_library::ff7::resolve_battle_files(source).map_err(|e| e.to_string())?;
    let data = Ff7BattleData::open(&files.scene, &files.kernel).map_err(|e| e.to_string())?;
    Ok((Arc::new(data), files.origin))
}

/// The names the battle windows show (spells, items, enemy attacks), read from the same files as
/// [`open_rules`] (see [`NameBook`]). `main` installs them with `install_local_names` next to the
/// rules.
#[cfg_attr(not(test), allow(dead_code))] // the integrator calls it from `main` next to `open_rules`
pub fn open_names(source: &Ff7Source) -> Result<NameBook, String> {
    let files = fmv_library::ff7::resolve_battle_files(source).map_err(|e| e.to_string())?;
    let data = Ff7BattleData::open(&files.scene, &files.kernel).map_err(|e| e.to_string())?;
    Ok(NameBook::from_data(data.kernel(), data.scenes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ff7::Ff7Labels;

    #[test]
    fn a_direct_flevel_has_no_battle_data_and_says_why() {
        let err = open_rules(&Ff7Source::Flevel { path: "/x/flevel.lgp".into(), labels: Ff7Labels::default() }).err().expect("no data");
        assert!(err.contains("library"), "{err}");
    }

    #[test]
    fn names_need_the_same_library_as_the_rules() {
        let err = open_names(&Ff7Source::Flevel { path: "/x/flevel.lgp".into(), labels: Ff7Labels::default() }).expect_err("no data");
        assert!(err.contains("library"), "{err}");
    }

    #[test]
    fn a_missing_library_points_at_fmv_import() {
        let empty = std::env::temp_dir().join("fmv-client-test-no-battle-library-dir");
        let err = open_rules(&Ff7Source::Library { state_dir: Some(empty), set: None }).err().expect("no data");
        assert!(err.contains("fmv-import"), "{err}");
    }
}
