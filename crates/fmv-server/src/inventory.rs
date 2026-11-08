//! Each account's items (milestone M2-e): what a player carries, and the changes that can happen to it.
//!
//! The server is the only authority over an inventory. The world keeps one [`Inventory`] per player
//! (the MIRROR, in memory, under the world lock) and hands every change to the saver as a
//! [`Change`], which applies the very same change to the stored record
//! (`Store::update_character_progress`). Both start equal and see the same changes in the same
//! order, so they agree (the store is the authority, like for character progress, see `progress`).
//!
//! # What an inventory is
//!
//! A list of stacks, ascending by item, one stack per item, none of zero, at most
//! [`MAX_INVENTORY_STACKS`] of them, and no stack above the game's cap (FF7: 99,
//! `BattleRules::max_item_stack`). What is stored is not trusted to fit that (a hand-edited file, a
//! record written under other rules): [`Inventory::from_entries`] brings any list inside it.
//!
//! # Where it starts
//!
//! A stored record with an inventory ([`fmv_persist::CharacterProgress::inventory`] of `Some`) is the
//! inventory, even when it is empty. A record without one (an account of an older build, or one that has
//! never held anything) and a brand-new account start with the game's starting stock
//! ([`BattleRules::starting_inventory`]: FF7's own, Potion x3, Ether x1 and Phoenix Down x1 on the Steam
//! copy), ONCE: the saver writes the stock into the record the first time an item is used or found, and
//! until then the stock is only what the mirror says, which is the same thing every time it is read.
//!
//! # Changes
//!
//! * [`Change::Used`]: one item taken (a battle command that the engine accepted). Taking an item the
//!   inventory does not hold does nothing (the mirror and the store can only disagree there if the
//!   store was edited by hand; neither goes negative).
//! * [`Change::Found`]: the items a won battle dropped. A stack stops at the cap, and a new stack is not
//!   started past [`MAX_INVENTORY_STACKS`].

use fmv_core::{InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_protocol::{BattleRules, ServerMsg};

/// One change to an inventory (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// One of this item was used.
    Used(u16),
    /// These items were found (a battle's drops).
    Found(Vec<InventoryEntry>),
}

/// An account's items. See the module docs for what it keeps true.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inventory {
    stacks: Vec<InventoryEntry>,
}

impl Inventory {
    /// The inventory `entries` come to inside the rules: repeated items merged, empty stacks dropped,
    /// every stack at most `max_stack`, ascending, and no more than [`MAX_INVENTORY_STACKS`] of them (the
    /// lowest items are kept).
    pub fn from_entries(entries: &[InventoryEntry], max_stack: u16) -> Self {
        let mut merged: Vec<InventoryEntry> = Vec::new();
        for entry in entries.iter().filter(|e| e.count > 0) {
            match merged.iter_mut().find(|m| m.item == entry.item) {
                Some(stack) => stack.count = stack.count.saturating_add(entry.count),
                None => merged.push(*entry),
            }
        }
        merged.sort_by_key(|e| e.item);
        merged.truncate(MAX_INVENTORY_STACKS);
        for stack in &mut merged {
            stack.count = stack.count.min(max_stack);
        }
        merged.retain(|e| e.count > 0);
        Self { stacks: merged }
    }

    /// The inventory an account starts a session with: what its record stores, and with no stored
    /// inventory (`None`) the game's starting stock (see the module docs).
    pub fn resolve(rules: &dyn BattleRules, stored: Option<&[InventoryEntry]>) -> Self {
        let max_stack = rules.max_item_stack();
        match stored {
            Some(entries) => Self::from_entries(entries, max_stack),
            None => Self::from_entries(&rules.starting_inventory(), max_stack),
        }
    }

    /// The stacks, ascending by item (what goes on the wire and into the store).
    pub fn entries(&self) -> &[InventoryEntry] {
        &self.stacks
    }

    /// How many of `item` the inventory holds.
    pub fn count(&self, item: u16) -> u16 {
        self.stacks.iter().find(|e| e.item == item).map_or(0, |e| e.count)
    }

    /// All the items together, for logs.
    pub fn total(&self) -> u32 {
        self.stacks.iter().map(|e| u32::from(e.count)).sum()
    }

    /// Add `count` of `item`, up to `max_stack` in the stack and without starting a stack past
    /// [`MAX_INVENTORY_STACKS`]. Returns how many were added.
    pub fn add(&mut self, item: u16, count: u16, max_stack: u16) -> u16 {
        if count == 0 {
            return 0;
        }
        match self.stacks.iter().position(|e| e.item == item) {
            Some(at) => {
                let stack = &mut self.stacks[at];
                let room = max_stack.saturating_sub(stack.count);
                let added = count.min(room);
                stack.count += added;
                added
            }
            None if self.stacks.len() >= MAX_INVENTORY_STACKS => 0,
            None => {
                let added = count.min(max_stack);
                if added > 0 {
                    let at = self.stacks.partition_point(|e| e.item < item);
                    self.stacks.insert(at, InventoryEntry::new(item, added));
                }
                added
            }
        }
    }

    /// Take one of `item`. `false` (and nothing changes) if there is none.
    pub fn take_one(&mut self, item: u16) -> bool {
        let Some(at) = self.stacks.iter().position(|e| e.item == item) else {
            return false;
        };
        self.stacks[at].count -= 1;
        if self.stacks[at].count == 0 {
            self.stacks.remove(at);
        }
        true
    }

    /// Apply one change (see the module docs).
    pub fn apply(&mut self, change: &Change, max_stack: u16) {
        match change {
            Change::Used(item) => {
                self.take_one(*item);
            }
            Change::Found(items) => {
                for entry in items {
                    self.add(entry.item, entry.count, max_stack);
                }
            }
        }
    }

    /// The message that tells the player what it carries.
    pub fn message(&self) -> ServerMsg {
        ServerMsg::Inventory { items: self.stacks.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_protocol::SyntheticRules;

    fn e(item: u16, count: u16) -> InventoryEntry {
        InventoryEntry::new(item, count)
    }

    #[test]
    fn any_list_is_brought_inside_the_rules() {
        let messy = [e(7, 1), e(0, 3), e(7, 2), e(5, 0), e(3, 150), e(0, 2)];
        let inv = Inventory::from_entries(&messy, 99);
        // Merged (Potion 3 + 2, Phoenix Down 1 + 2), no empty stack, capped at 99, ascending.
        assert_eq!(inv.entries(), [e(0, 5), e(3, 99), e(7, 3)]);
        assert_eq!((inv.count(0), inv.count(3), inv.count(7), inv.count(5), inv.count(9)), (5, 99, 3, 0, 0));
        assert_eq!(inv.total(), 107);
        // Counts that overflow while merging saturate instead of wrapping.
        assert_eq!(Inventory::from_entries(&[e(1, u16::MAX), e(1, u16::MAX)], 99).entries(), [e(1, 99)]);
        // A stack cap of zero keeps nothing; too many stacks keep the lowest items.
        assert!(Inventory::from_entries(&messy, 0).entries().is_empty());
        let wide: Vec<InventoryEntry> = (0..400u16).rev().map(|i| e(i, 1)).collect();
        let kept = Inventory::from_entries(&wide, 99);
        assert_eq!(kept.entries().len(), MAX_INVENTORY_STACKS);
        assert_eq!(kept.entries().first(), Some(&e(0, 1)));
        assert_eq!(kept.entries().last(), Some(&e(319, 1)));
    }

    #[test]
    fn a_stored_inventory_wins_even_when_empty_and_none_means_the_starting_stock() {
        let rules = SyntheticRules::default();
        let stock = [e(0, 3), e(3, 1), e(7, 1)];
        assert_eq!(Inventory::resolve(&rules, None).entries(), stock);
        assert_eq!(Inventory::resolve(&rules, Some(&[e(7, 2)])).entries(), [e(7, 2)]);
        assert!(Inventory::resolve(&rules, Some(&[])).entries().is_empty(), "a stored, empty inventory is empty: the stock is given once");
        // What is stored is not trusted to fit the rules.
        assert_eq!(Inventory::resolve(&rules, Some(&[e(0, 500), e(2, 0)])).entries(), [e(0, 99)]);
    }

    #[test]
    fn add_stops_at_the_cap_keeps_order_and_never_starts_too_many_stacks() {
        let mut inv = Inventory::default();
        assert_eq!(inv.add(5, 3, 99), 3);
        assert_eq!(inv.add(2, 1, 99), 1);
        assert_eq!(inv.add(9, 1, 99), 1);
        assert_eq!(inv.add(5, 2, 99), 2);
        assert_eq!(inv.entries(), [e(2, 1), e(5, 5), e(9, 1)], "ascending, one stack per item");
        // Room for 94 more of item 5 and no more.
        assert_eq!(inv.add(5, 200, 99), 94);
        assert_eq!(inv.count(5), 99);
        assert_eq!(inv.add(5, 1, 99), 0);
        // Nothing added, nothing started.
        assert_eq!(inv.add(4, 0, 99), 0);
        assert_eq!(inv.add(4, 5, 0), 0);
        assert_eq!(inv.entries().len(), 3);
        // The stack limit: a new item is refused past it, a held item still grows.
        let mut full = Inventory::from_entries(&(0..MAX_INVENTORY_STACKS as u16).map(|i| e(i, 1)).collect::<Vec<_>>(), 99);
        assert_eq!(full.add(1000, 1, 99), 0);
        assert_eq!(full.add(7, 1, 99), 1);
        assert_eq!(full.entries().len(), MAX_INVENTORY_STACKS);
    }

    #[test]
    fn taking_one_empties_a_stack_away_and_taking_nothing_changes_nothing() {
        let mut inv = Inventory::from_entries(&[e(0, 2), e(7, 1)], 99);
        assert!(inv.take_one(0));
        assert_eq!(inv.entries(), [e(0, 1), e(7, 1)]);
        assert!(inv.take_one(7));
        assert_eq!(inv.entries(), [e(0, 1)], "an empty stack is gone");
        assert!(!inv.take_one(7) && !inv.take_one(3));
        assert!(inv.take_one(0));
        assert!(inv.entries().is_empty() && inv.total() == 0);
        assert!(!inv.take_one(0));
    }

    #[test]
    fn changes_apply_in_order_and_the_message_is_the_whole_inventory() {
        let mut inv = Inventory::from_entries(&[e(0, 3), e(7, 1)], 99);
        inv.apply(&Change::Used(7), 99);
        inv.apply(&Change::Found(vec![e(3, 2), e(0, 1)]), 99);
        inv.apply(&Change::Used(9), 99); // not held: nothing
        assert_eq!(inv.entries(), [e(0, 4), e(3, 2)]);
        assert_eq!(inv.message(), ServerMsg::Inventory { items: vec![e(0, 4), e(3, 2)] });
        // Drops respect the cap the rules give.
        inv.apply(&Change::Found(vec![e(0, 100)]), 10);
        assert_eq!(inv.count(0), 10);
    }
}
