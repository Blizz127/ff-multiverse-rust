//! One stack of an account's inventory, shared by the wire protocol, the store and the server.
//!
//! Game-agnostic: `item` is the hosting game's own item index (FF7: the index into the kernel's item
//! table, `kernel.bin` section 4) and means nothing here. Like everything in this crate it holds
//! ids and numbers only, never game data.

use serde::{Deserialize, Serialize};

/// Most stacks (distinct items) an inventory holds, on the wire and in the store. FF7's own item
/// list has 320 slots; a sender never exceeds this and a receiver bounds what it keeps by it.
pub const MAX_INVENTORY_STACKS: usize = 320;

/// `count` of item `item`: one line of an inventory. A well-formed inventory has each item at most
/// once and no stack of zero (the server keeps it that way; readers do not rely on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InventoryEntry {
    /// The game's item index.
    pub item: u16,
    /// How many of it the account holds.
    pub count: u16,
}

impl InventoryEntry {
    pub const fn new(item: u16, count: u16) -> Self {
        Self { item, count }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_shape_is_pinned() {
        let e = InventoryEntry::new(3, 99);
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"item":3,"count":99}"#);
        assert_eq!(serde_json::from_str::<InventoryEntry>(r#"{"item":3,"count":99}"#).unwrap(), e);
        // Every field is required and both are 16-bit.
        assert!(serde_json::from_str::<InventoryEntry>(r#"{"item":3}"#).is_err());
        assert!(serde_json::from_str::<InventoryEntry>(r#"{"item":65536,"count":1}"#).is_err());
        assert!(serde_json::from_str::<InventoryEntry>(r#"{"item":1,"count":-1}"#).is_err());
    }

    #[test]
    fn the_stack_bound_is_ff7s_slot_count() {
        assert_eq!(MAX_INVENTORY_STACKS, 320);
    }
}
