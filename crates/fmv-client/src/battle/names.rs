//! The names the battle windows show that the battle engine does not carry: spells, items and the
//! attacks of the enemies (a fighter's own name travels with it, see `Combatant::name`).
//!
//! They come from this machine's OWN battle data, like everything else about a battle: the
//! spell and item names are the text tables of `kernel.bin` (sections 18 and 19, an FF text table:
//! pointers, then strings), the enemy attack names are the attack name tables of `scene.bin`,
//! keyed by the scene of the formation. Nothing here is ever sent anywhere.
//!
//! A name is looked up by the id the engine uses, never assumed to sit at that index of a table:
//! an id the tables do not name (or name with nothing) gets a plain label (`Spell 12`), so a
//! mismatch of numbering is visible and harmless.
//!
//! The text tables of `kernel2.bin` (what the Steam game itself reads) have the same layout, behind a
//! 4-byte header each (a little-endian length and a zero), and differ from `kernel.bin`'s in a
//! handful of entries; [`NameBook::from_tables`] takes the decoded strings from whichever file
//! the caller read them from.

use fmv_ff7_battle::kernel::Kernel;
use fmv_ff7_battle::scene::SceneBin;
use std::collections::HashMap;

/// `kernel.bin` section of the magic names.
pub const SECTION_MAGIC_NAMES: usize = 18;
/// `kernel.bin` section of the item names.
pub const SECTION_ITEM_NAMES: usize = 19;
/// Most entries read from a text table (the real ones have 256 and 128).
const MAX_ENTRIES: usize = 1024;
/// Most characters of a name shown anywhere.
const MAX_NAME_CELLS: usize = 24;

/// A name as the UI font can draw it: printable ASCII, nothing else (anything the font lacks would
/// be a box), at most [`MAX_NAME_CELLS`] characters, no padding.
pub fn clean_name(raw: &str) -> String {
    let ascii: String = raw.chars().map(|c| if (' '..='~').contains(&c) { c } else { '?' }).collect();
    ascii.trim().chars().take(MAX_NAME_CELLS).collect()
}

/// Spell, item and enemy attack names. Cheap to share: built once per process and read-only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameBook {
    spells: Vec<String>,
    items: Vec<String>,
    /// `(scene index, attack id)` to the name in that scene's attack table.
    attacks: HashMap<(u16, u16), String>,
}

impl NameBook {
    /// A book from decoded name tables (the strings in table order, blanks included); no enemy
    /// attacks.
    pub fn from_tables(spells: &[String], items: &[String]) -> Self {
        let clean = |names: &[String]| names.iter().take(MAX_ENTRIES).map(|n| clean_name(n)).collect();
        Self { spells: clean(spells), items: clean(items), attacks: HashMap::new() }
    }

    /// The book of this machine's own battle data.
    pub fn from_data(kernel: &Kernel, scenes: &SceneBin) -> Self {
        let table = |section: usize| -> Vec<String> {
            (0..MAX_ENTRIES).map_while(|i| kernel.text_entry(section, i)).collect()
        };
        let mut book = Self::from_tables(&table(SECTION_MAGIC_NAMES), &table(SECTION_ITEM_NAMES));
        for scene in scenes.scenes() {
            let Ok(index) = u16::try_from(scene.index) else { continue };
            for attack in &scene.attacks {
                let name = clean_name(&attack.name);
                if !name.is_empty() {
                    book.attacks.insert((index, attack.id), name);
                }
            }
        }
        book
    }

    fn entry(table: &[String], id: u16) -> Option<&str> {
        table.get(usize::from(id)).map(String::as_str).filter(|n| !n.is_empty())
    }

    /// The name of spell `id`, or `Spell <id>` when the tables have none.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator names the engine's spells with it
    pub fn spell(&self, id: u16) -> String {
        Self::entry(&self.spells, id).map_or_else(|| format!("Spell {id}"), str::to_string)
    }

    /// The name of item `id`, or `Item <id>` when the tables have none.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator names the inventory with it
    pub fn item(&self, id: u16) -> String {
        Self::entry(&self.items, id).map_or_else(|| format!("Item {id}"), str::to_string)
    }

    /// The name of attack `id` of the scene of `formation` (`scene * 4 + slot`), if the scene names
    /// it.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator names the enemies' actions with it
    pub fn enemy_attack(&self, formation: u16, id: u16) -> Option<&str> {
        self.attacks.get(&(formation / 4, id)).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn names_are_looked_up_by_id_and_unnamed_ids_get_a_plain_label() {
        let book = NameBook::from_tables(&names(&["Bolt", "", "Mend"]), &names(&["Tonic", "Elixir"]));
        assert_eq!((book.spell(0).as_str(), book.spell(2).as_str()), ("Bolt", "Mend"));
        assert_eq!(book.spell(1), "Spell 1", "a blank entry is not a name");
        assert_eq!(book.spell(300), "Spell 300", "past the table");
        assert_eq!((book.item(1).as_str(), book.item(9).as_str()), ("Elixir", "Item 9"));
        assert_eq!(book.enemy_attack(8, 0x10), None);
        assert_eq!(NameBook::default().spell(0), "Spell 0");
    }

    #[test]
    fn a_name_is_made_drawable_and_short() {
        assert_eq!(clean_name("  Bolt  "), "Bolt");
        assert_eq!(clean_name("Caf\u{e9} \u{fffd}"), "Caf? ?");
        assert_eq!(clean_name(""), "");
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME_CELLS);
        let book = NameBook::from_tables(&names(&["Fire\n"]), &[]);
        assert_eq!(book.spell(0), "Fire?");
    }

    /// The names of the owner's own copy, on request (`FMV_FF7_LIBRARY` names the state directory the
    /// library lives in). Prints only counts: the names are game data.
    #[test]
    #[ignore = "needs the owner's real FF7 library (FMV_FF7_LIBRARY)"]
    fn the_real_data_names_spells_items_and_enemy_attacks() {
        use crate::battle::data::open_names;
        use crate::ff7::Ff7Source;
        let Some(library) = std::env::var_os("FMV_FF7_LIBRARY") else {
            eprintln!("skipped: FMV_FF7_LIBRARY is not set");
            return;
        };
        let book = open_names(&Ff7Source::Library { state_dir: Some(library.into()), set: None }).expect("the battle data opens");
        let named = |f: &dyn Fn(u16) -> String, label: &str, count: u16| (0..count).filter(|i| f(*i) != format!("{label} {i}")).count();
        let spells = named(&|i| book.spell(i), "Spell", 256);
        let items = named(&|i| book.item(i), "Item", 128);
        let attacks = (0..4096_u16).filter(|f| (0..0x400_u16).any(|a| book.enemy_attack(*f, a).is_some())).count();
        eprintln!("named: {spells} spells of 256, {items} items of 128, formations with attack names: {attacks}");
        assert!(spells > 30 && items > 30 && attacks > 100, "{spells} {items} {attacks}");
        // Plain words only: nothing the UI font would draw as a box.
        for i in 0..256 {
            assert!(book.spell(i).chars().all(|c| (' '..='~').contains(&c)));
        }
    }
}
