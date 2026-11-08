//! The `maplist` name list and gateways joined with it.
//!
//! `flevel.lgp` stores a raw (not compressed) entry called `maplist`: a `u16` count followed by
//! `count` NUL-padded 32-byte field names. A gateway's `target_field_id` (section 7) and a
//! `MAPJUMP` opcode's field id both index it. Measured on the owner's Steam copy: 788 names
//! (25 218 bytes), id 117 is `md1_1`, which `md1stin`'s only gateway targets.
//!
//! Sources: FFRTT "FF7/Field/Triggers" <https://wiki.ffrtt.ru/index.php/FF7/Field/Triggers> for
//! the gateway record (already decoded by [`crate::field::Triggers`]; this module only reads it)
//! and `crate::field`'s own verified notes for the `maplist` shape.

use super::{MapJump, ScriptError};
use crate::field::{Gateway, Triggers};
use crate::lgp::LgpArchive;
use crate::util::{nul_padded_name, slice_at, u16_at};
use std::io::{Read, Seek};

/// Bytes per name record.
pub const MAPLIST_NAME_LEN: usize = 32;
/// Name of the archive entry holding the list.
pub const MAPLIST_ENTRY_NAME: &str = "maplist";

/// The ordered list of field names; a field's id is its position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Maplist {
    names: Vec<String>,
}

impl Maplist {
    /// Parse the `maplist` entry's bytes. Trailing bytes after the last name are ignored; a
    /// count that promises more names than the data holds is an error.
    pub fn parse(data: &[u8]) -> Result<Self, ScriptError> {
        let count = usize::from(u16_at(data, 0).ok_or(ScriptError::Maplist("shorter than the 2-byte count"))?);
        let needed = count
            .checked_mul(MAPLIST_NAME_LEN)
            .and_then(|n| n.checked_add(2))
            .ok_or(ScriptError::Maplist("count overflows"))?;
        if data.len() < needed {
            return Err(ScriptError::Maplist("count promises more names than the data holds"));
        }
        let mut names = Vec::with_capacity(count);
        for i in 0..count {
            // i * 32 + 2 <= needed <= data.len(), so neither step can fail or overflow.
            let raw = slice_at(data, 2 + i * MAPLIST_NAME_LEN, MAPLIST_NAME_LEN).ok_or(ScriptError::Maplist("name record out of range"))?;
            names.push(nul_padded_name(raw));
        }
        Ok(Self { names })
    }

    /// Read and parse the `maplist` entry of an open `flevel.lgp`.
    pub fn from_archive<R: Read + Seek>(archive: &mut LgpArchive<R>) -> Result<Self, ScriptError> {
        let bytes = archive.read(MAPLIST_ENTRY_NAME).map_err(|e| ScriptError::MaplistRead(e.to_string()))?;
        Self::parse(&bytes)
    }

    /// Number of names.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Field name for `id` (`None` if the id is past the end of the list).
    pub fn name(&self, id: u16) -> Option<&str> {
        self.names.get(usize::from(id)).map(String::as_str)
    }

    /// Id of the first field called `name` (ASCII case-insensitive).
    pub fn id_of(&self, name: &str) -> Option<u16> {
        let index = self.names.iter().position(|n| n.eq_ignore_ascii_case(name))?;
        u16::try_from(index).ok()
    }

    /// All names in id order.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// A used gateway of a field with its target field name filled in.
///
/// `destination_*` are in the **target** field's walkmesh coordinates; `destination_triangle`
/// is the index of the target triangle that contains the arrival point (verified for all 978
/// gateways whose target is in the owned archive, see [`crate::field::Gateway`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGateway {
    /// Slot (0..12) in the triggers section.
    pub slot: usize,
    /// Exit line end points `(x, y, z)` in this field's walkmesh coordinates.
    pub exit_a: [i16; 3],
    pub exit_b: [i16; 3],
    pub target_field_id: u16,
    /// Name from the maplist, `None` when the id is outside the list.
    pub target_field: Option<String>,
    pub destination_x: i16,
    pub destination_y: i16,
    /// Walkmesh triangle index in the target field (the stored third destination value;
    /// `None` only for a negative, i.e. damaged, value).
    pub destination_triangle: Option<usize>,
}

impl ResolvedGateway {
    fn from_gateway(slot: usize, g: &Gateway, maplist: &Maplist) -> Self {
        let [destination_x, destination_y] = g.destination_xy();
        Self {
            slot,
            exit_a: g.exit_a,
            exit_b: g.exit_b,
            target_field_id: g.target_field_id,
            target_field: maplist.name(g.target_field_id).map(str::to_string),
            destination_x,
            destination_y,
            destination_triangle: g.destination_triangle(),
        }
    }
}

/// Join a field's used gateways with the maplist names. Unused slots are skipped; a gateway
/// whose id is outside the maplist keeps `target_field == None`.
pub fn resolve_gateways(triggers: &Triggers, maplist: &Maplist) -> Vec<ResolvedGateway> {
    triggers
        .gateways
        .iter()
        .enumerate()
        .filter(|(_, g)| g.is_used())
        .map(|(slot, g)| ResolvedGateway::from_gateway(slot, g, maplist))
        .collect()
}

/// A `MAPJUMP` found in a script with its target field name filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMapJump {
    pub jump: MapJump,
    /// Name from the maplist, `None` when the id is outside the list.
    pub target_field: Option<String>,
}

/// Join `MAPJUMP`s (see [`crate::script::FieldScript::map_jumps`]) with the maplist names.
pub fn resolve_map_jumps(jumps: &[MapJump], maplist: &Maplist) -> Vec<ResolvedMapJump> {
    jumps
        .iter()
        .map(|j| ResolvedMapJump { jump: j.clone(), target_field: maplist.name(j.field_id).map(str::to_string) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{CameraRange, Trigger};

    fn list(names: &[&str]) -> Vec<u8> {
        let mut v = (names.len() as u16).to_le_bytes().to_vec();
        for n in names {
            let mut rec = [0u8; MAPLIST_NAME_LEN];
            rec[..n.len()].copy_from_slice(n.as_bytes());
            v.extend_from_slice(&rec);
        }
        v
    }

    fn gw(target: u16, dest: [i16; 3]) -> Gateway {
        Gateway { exit_a: [1, 2, 3], exit_b: [4, 5, 6], destination: dest, target_field_id: target, unknown: [0; 4] }
    }

    fn triggers(gateways: Vec<Gateway>) -> Triggers {
        Triggers {
            name: "t".into(),
            control_direction: 0,
            focus_height: 0,
            camera_range: CameraRange { left: 0, bottom: 0, right: 0, top: 0 },
            bg_animation_sizes: [0; 4],
            gateways,
            triggers: Vec::<Trigger>::new(),
        }
    }

    #[test]
    fn parses_names_and_looks_up_ids() {
        let m = Maplist::parse(&list(&["alpha", "md1_1", ""])).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m.name(1), Some("md1_1"));
        assert_eq!(m.name(2), Some(""));
        assert_eq!(m.name(3), None);
        assert_eq!(m.id_of("MD1_1"), Some(1));
        assert_eq!(m.id_of("nope"), None);
    }

    #[test]
    fn malformed_lists_are_errors_not_panics() {
        assert!(Maplist::parse(&[]).is_err());
        assert!(Maplist::parse(&[1]).is_err());
        // count says 2 but only one record follows
        let mut bad = list(&["a"]);
        bad[0] = 2;
        assert!(Maplist::parse(&bad).is_err());
        // huge count
        assert!(Maplist::parse(&[0xFF, 0xFF, 0, 0]).is_err());
        // trailing bytes are tolerated
        let mut ok = list(&["a"]);
        ok.extend_from_slice(&[9, 9, 9]);
        assert_eq!(Maplist::parse(&ok).unwrap().len(), 1);
    }

    #[test]
    fn map_jumps_are_joined_with_names() {
        let m = Maplist::parse(&list(&["a", "b", "c"])).unwrap();
        let jump = |field_id: u16| MapJump {
            entity: 0,
            entity_name: "dir".into(),
            script: 0,
            offset: 0x10,
            field_id,
            x: 1,
            y: 2,
            triangle: 3,
            direction: 4,
            conditional: false,
        };
        let r = resolve_map_jumps(&[jump(1), jump(9)], &m);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].target_field.as_deref(), Some("b"));
        assert_eq!(r[0].jump.triangle, 3);
        assert_eq!(r[1].target_field, None);
    }

    #[test]
    fn gateways_are_joined_with_names_and_unused_slots_skipped() {
        let m = Maplist::parse(&list(&["a", "b", "c"])).unwrap();
        let t = triggers(vec![gw(2, [10, -20, 7]), gw(Gateway::UNUSED_FIELD_ID, [0; 3]), gw(500, [1, 1, 1])]);
        let r = resolve_gateways(&t, &m);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].slot, r[0].target_field.as_deref()), (0, Some("c")));
        assert_eq!((r[0].destination_x, r[0].destination_y, r[0].destination_triangle), (10, -20, Some(7)));
        assert_eq!((r[1].slot, r[1].target_field_id, r[1].target_field.as_deref()), (2, 500, None));
    }
}
