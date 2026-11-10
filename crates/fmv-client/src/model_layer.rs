//! Read-only model replacements from the owner's Ninostyle archives.
use fmv_mods::{modxml, IroArchive};
use std::{collections::HashMap, io, path::Path, sync::{Arc, RwLock}};

const FIELD_ID: &str = "2b25060c-c60e-426e-ac46-f85439e903e2";
const BATTLE_ID: &str = "2b25060c-c60e-426e-ac46-f85439e903e3";
// Dynamic weapon folders require equipment-aware selection, which this layer does not yet provide.
const FIELD_FOLDERS: &[&str] = &["lazaro\\char\\", "efryt\\fb\\char\\", "fb\\char\\"];
const BATTLE_FOLDERS: &[&str] = &[
    "ninostyle\\models\\mains\\cloud\\model\\battle\\", "ninostyle\\models\\mains\\barret\\model v3.0\\battle\\",
    "ninostyle\\models\\mains\\tifa\\model\\battle\\", "ninostyle\\models\\mains\\aerith\\model\\battle\\",
    "ninostyle\\models\\mains\\cid\\model\\battle\\", "ninostyle\\models\\mains\\red xiii\\model\\battle\\",
    "ninostyle\\models\\mains\\yuffie\\model\\battle\\", "ninostyle\\models\\mains\\cait sith\\model\\battle\\",
    "ninostyle\\models\\mains\\vincent\\model\\battle\\", "ninostyle\\models\\mains\\sephiroth\\model\\battle\\",
    "ninostyle\\models\\enemies\\battle\\", "ninostyle\\models\\summons\\battle\\",
];

struct Layer { archive: IroArchive, files: HashMap<String, String> }
impl Layer {
    fn open(path: &Path, id: &str, folders: &[&str]) -> Option<Self> {
        let archive = IroArchive::open_path(path).ok()?;
        let manifest = archive.find("mod.xml")?;
        let info = modxml::parse(&archive.read_limited(manifest, modxml::MAX_MOD_XML).ok()?)?;
        if info.id != id { return None; }
        let mut files = HashMap::new();
        for folder in folders {
            for entry in archive.entries() {
                let key = fmv_mods::normalize(&entry.name);
                if let Some(name) = key.strip_prefix(folder) {
                    if !name.contains('\\') { files.entry(name.to_string()).or_insert_with(|| entry.name.clone()); }
                }
            }
        }
        Some(Self { archive, files })
    }
    fn read(&self, name: &str) -> Option<io::Result<Vec<u8>>> {
        let path = self.files.get(&name.to_ascii_lowercase())?;
        let entry = self.archive.find(path)?;
        Some(self.archive.read_limited(entry, u64::from(fmv_ff7::lgp::MAX_ENTRY_LEN)).map_err(io::Error::other))
    }
}

#[derive(Default)]
struct Layers { field: Option<Arc<Layer>>, battle: Option<Arc<Layer>> }
static ACTIVE: RwLock<Option<Layers>> = RwLock::new(None);

pub fn install(dir: Option<&Path>, enabled: bool) {
    let mut layers = Layers::default();
    if enabled {
        if let Some(dir) = dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten().take(64) {
                    let path = entry.path();
                    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iro")) { continue; }
                    if layers.field.is_none() { layers.field = Layer::open(&path, FIELD_ID, FIELD_FOLDERS).map(Arc::new); }
                    if layers.battle.is_none() { layers.battle = Layer::open(&path, BATTLE_ID, BATTLE_FOLDERS).map(Arc::new); }
                }
            }
        }
    }
    eprintln!("mods: models={} field={} battle={}", if enabled { "on" } else { "off" }, layers.field.is_some(), layers.battle.is_some());
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(layers);
}

pub fn field() -> Option<impl FnMut(&str) -> Option<io::Result<Vec<u8>>> + Send + 'static> {
    let layer = ACTIVE.read().ok()?.as_ref()?.field.clone()?;
    Some(move |name: &str| layer.read(name))
}
pub fn battle() -> Option<impl FnMut(&str) -> Option<io::Result<Vec<u8>>> + Send + 'static> {
    let layer = ACTIVE.read().ok()?.as_ref()?.battle.clone()?;
    Some(move |name: &str| layer.read(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_folders_prefer_configured_overrides_and_ignore_unrelated_paths() {
        use fmv_mods::iro::testkit::{build, stored};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("field.iro");
        let xml = format!("<ID>{FIELD_ID}</ID>");
        std::fs::write(&path, build(&[
            stored("mod.xml", xml.as_bytes()),
            stored("fb\\char\\aaaa.hrc", b"base"),
            stored("Lazaro\\char\\AAAA.HRC", b"override"),
            stored("fc\\char\\other.hrc", b"inactive"),
        ])).unwrap();
        let layer = Layer::open(&path, FIELD_ID, FIELD_FOLDERS).unwrap();
        assert_eq!(layer.read("AaAa.hrc").unwrap().unwrap(), b"override");
        assert!(layer.read("other.hrc").is_none());
        assert!(Layer::open(&path, BATTLE_ID, BATTLE_FOLDERS).is_none());
    }

    #[test]
    fn battle_folder_selects_default_model_only() {
        use fmv_mods::iro::testkit::{build, stored};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("battle.iro");
        let xml = format!("<ID>{BATTLE_ID}</ID>");
        std::fs::write(&path, build(&[
            stored("mod.xml", xml.as_bytes()),
            stored("NinoStyle\\Models\\Mains\\Cloud\\Model\\Battle\\rtaa", b"default"),
            stored("NinoStyle\\Models\\Mains\\Tifa\\Model - Cowgirl\\Battle\\rtaa", b"alternate"),
        ])).unwrap();
        let layer = Layer::open(&path, BATTLE_ID, BATTLE_FOLDERS).unwrap();
        assert_eq!(layer.read("RTAA").unwrap().unwrap(), b"default");
        assert!(layer.read("rtab").is_none());
    }

    #[test]
    #[ignore = "requires the owner's Ninostyle IRO and FF7 char.lgp"]
    fn owned_archives_load_field_cast_and_original_fallback() {
        let dir = std::env::var("FMV_NINO_TEST").expect("FMV_NINO_TEST");
        let char_path = std::env::var("FMV_CHAR_LGP").expect("FMV_CHAR_LGP");
        let layer = Arc::new(Layer::open(&Path::new(&dir).join("Ninostyle Chibi.iro"), FIELD_ID, FIELD_FOLDERS).unwrap());
        let mut original = fmv_ff7::lgp::LgpArchive::open_path(&char_path).unwrap();
        let original_cloud = original.read("aaaa.hrc").unwrap();
        let mut archive = fmv_ff7::lgp::LgpArchive::open_path(char_path).unwrap();
        archive.set_overlay(move |name| layer.read(name));
        assert_ne!(archive.read("aaaa.hrc").unwrap(), original_cloud);
        for model in fmv_ff7::playable::PLAYABLE_FIELD_MODELS {
            fmv_ff7::playable::load_playable(&mut archive, model).unwrap_or_else(|e| panic!("{}: {e}", model.appearance_ref));
        }
        for body in fmv_ff7::playable::PLACEHOLDER_BODIES {
            fmv_ff7::playable::load_playable(&mut archive, &body.model).unwrap_or_else(|e| panic!("{}: {e}", body.archetype));
        }
    }

    #[test]
    #[ignore = "requires the owner's Ninostyle IRO and FF7 battle.lgp"]
    fn owned_battle_archive_loads_canon_models() {
        use fmv_ff7::battle_model::{BattleCharacter, BattleModels};
        let dir = std::env::var("FMV_NINO_TEST").expect("FMV_NINO_TEST");
        let path = std::env::var("FMV_BATTLE_LGP").expect("FMV_BATTLE_LGP");
        let layer = Arc::new(Layer::open(&Path::new(&dir).join("Ninostyle Battle.iro"), BATTLE_ID, BATTLE_FOLDERS).unwrap());
        let mut original = fmv_ff7::lgp::LgpArchive::open_path(&path).unwrap();
        let cloud = original.read("rtaa").unwrap();
        let mut archive = fmv_ff7::lgp::LgpArchive::open_path(path).unwrap();
        archive.set_overlay(move |name| layer.read(name));
        assert_ne!(archive.read("rtaa").unwrap(), cloud);
        let mut models = BattleModels::from_archive(archive).unwrap();
        for character in BattleCharacter::ALL {
            models.load_character(character).unwrap_or_else(|e| panic!("{character:?}: {e}"));
        }
    }
}
