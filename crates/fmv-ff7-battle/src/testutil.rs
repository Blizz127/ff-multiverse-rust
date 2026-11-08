//! Synthetic data builders for unit tests. Everything here is invented; no game bytes.

use std::io::Write;

use flate2::{write::GzEncoder, Compression};

use crate::kernel::{
    ARMOR_RECORD_SIZE, CHARACTER_COUNT, CURVE_COUNT, CURVE_SIZE, GROWTH_RECORD_SIZE,
    INIT_CHARACTER_SIZE, OFF_CURVES, OFF_HP_GAIN, OFF_MP_GAIN, OFF_SCENE_LOOKUP, OFF_STAT_GAIN,
    SECTION_TEXT_ARMOR_NAMES, SECTION_TEXT_COMMAND_NAMES, SECTION_TEXT_WEAPON_NAMES,
    WEAPON_RECORD_SIZE,
};
use crate::scene::layout::*;
use crate::scene::{ATTACK_RECORD_SIZE, BLOCK_HEADER_SIZE, BLOCK_POINTERS, BLOCK_SIZE, SCENE_SIZE};
use crate::text;

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

// --- synthetic scene.bin builders ---

/// A raw (decompressed) 7808-byte scene under construction. Unset bytes are `0xFF`, like the
/// unused regions of real scenes.
pub struct RawScene {
    pub raw: Vec<u8>,
}

impl RawScene {
    pub fn blank() -> Self {
        Self {
            raw: vec![0xFF; SCENE_SIZE],
        }
    }

    pub fn put(&mut self, off: usize, bytes: &[u8]) {
        self.raw[off..off + bytes.len()].copy_from_slice(bytes);
    }

    pub fn put_u8(&mut self, off: usize, v: u8) {
        self.raw[off] = v;
    }

    pub fn put_u16(&mut self, off: usize, v: u16) {
        self.put(off, &v.to_le_bytes());
    }

    pub fn put_i16(&mut self, off: usize, v: i16) {
        self.put(off, &v.to_le_bytes());
    }

    pub fn put_u32(&mut self, off: usize, v: u32) {
        self.put(off, &v.to_le_bytes());
    }

    pub fn enemy_ids(&mut self, ids: [u16; 3]) {
        for (k, id) in ids.iter().enumerate() {
            self.put_u16(OFF_ENEMY_IDS + k * 2, *id);
        }
        self.put_u16(OFF_ENEMY_IDS + 6, 0xFFFF);
    }

    pub fn setup(&mut self, slot: usize, location: u16, next: u16, layout: u8, camera: u8) {
        let o = OFF_SETUPS + slot * SETUP_SIZE;
        self.put_u16(o, location);
        self.put_u16(o + 2, next);
        self.put_u16(o + 4, 1);
        self.put_u16(o + 6, 0xFFFF);
        for k in 0..4 {
            self.put_u16(o + 8 + k * 2, 0x03E7);
        }
        self.put_u16(o + 16, 0x00A5); // invented flags word
        self.put_u8(o + 18, layout);
        self.put_u8(o + 19, camera);
    }

    pub fn camera(&mut self, slot: usize, position: [i16; 3], direction: [i16; 3]) {
        let o = OFF_CAMERAS + slot * CAMERA_SET_SIZE;
        for (i, v) in position.iter().enumerate() {
            self.put_i16(o + i * 2, *v);
        }
        for (i, v) in direction.iter().enumerate() {
            self.put_i16(o + 6 + i * 2, *v);
        }
    }

    pub fn formation_entry(
        &mut self,
        slot: usize,
        k: usize,
        enemy_id: u16,
        pos: [i16; 3],
        row: u16,
    ) {
        let o = OFF_FORMATIONS + slot * FORMATION_SIZE + k * ENTRY_SIZE;
        self.put_u16(o, enemy_id);
        for (i, v) in pos.iter().enumerate() {
            self.put_i16(o + 2 + i * 2, *v);
        }
        self.put_u16(o + 8, row);
        self.put_u16(o + 10, 0);
        self.put_u32(o + 12, 0x0000_0019); // invented initial flags
    }

    pub fn enemy(&mut self, k: usize, name: &str, stats: EnemySpec) {
        let o = OFF_ENEMIES + k * ENEMY_SIZE;
        let mut name_bytes = text::encode(name).unwrap();
        name_bytes.push(0x00); // trailing space, as in real data
        name_bytes.resize(32, 0xFF);
        self.put(o, &name_bytes);
        self.put_u8(o + 0x20, stats.level);
        self.put_u8(o + 0x21, stats.speed);
        self.put_u8(o + 0x22, stats.luck);
        self.put_u8(o + 0x23, stats.evade);
        self.put_u8(o + 0x24, stats.strength);
        self.put_u8(o + 0x25, stats.defense);
        self.put_u8(o + 0x26, stats.magic);
        self.put_u8(o + 0x27, stats.magic_defense);
        self.put(o + 0x28, &stats.element_codes);
        self.put(o + 0x30, &stats.element_rates);
        for i in 0..16 {
            let id = stats.attack_ids.get(i).copied().unwrap_or(0xFFFF);
            self.put_u16(o + 0x48 + i * 2, id);
        }
        self.put_u16(o + 0x9C, stats.mp);
        self.put_u16(o + 0x9E, stats.ap);
        self.put_u8(o + 0xA2, 12); // invented back-attack multiplier
        self.put_u32(o + 0xA4, stats.hp);
        self.put_u32(o + 0xA8, stats.exp);
        self.put_u32(o + 0xAC, stats.gil);
        self.put_u32(o + 0xB0, stats.status_immunities);
    }

    pub fn attack(
        &mut self,
        k: usize,
        id: u16,
        name: &str,
        accuracy: u8,
        damage_calc: u8,
        power: u8,
    ) {
        let o = OFF_ATTACKS + k * ATTACK_RECORD_SIZE;
        self.put(o, &[0u8; ATTACK_RECORD_SIZE]);
        self.put_u8(o, accuracy);
        self.put_u8(o + 0x0C, 0x03);
        self.put_u8(o + 0x0E, damage_calc);
        self.put_u8(o + 0x0F, power);
        self.put_u16(OFF_ATTACK_IDS + k * 2, id);
        let mut n = text::encode(name).unwrap();
        n.resize(ATTACK_NAME_SIZE, 0xFF);
        self.put(OFF_ATTACK_NAMES + k * ATTACK_NAME_SIZE, &n);
    }
}

#[derive(Clone)]
pub struct EnemySpec {
    pub level: u8,
    pub speed: u8,
    pub luck: u8,
    pub evade: u8,
    pub strength: u8,
    pub defense: u8,
    pub magic: u8,
    pub magic_defense: u8,
    pub element_codes: [u8; 8],
    pub element_rates: [u8; 8],
    pub attack_ids: Vec<u16>,
    pub mp: u16,
    pub ap: u16,
    pub hp: u32,
    pub exp: u32,
    pub gil: u32,
    pub status_immunities: u32,
}

impl Default for EnemySpec {
    fn default() -> Self {
        Self {
            level: 1,
            speed: 50,
            luck: 0,
            evade: 0,
            strength: 10,
            defense: 5,
            magic: 0,
            magic_defense: 0,
            element_codes: [0xFF; 8],
            element_rates: [0xFF; 8],
            attack_ids: Vec::new(),
            mp: 0,
            ap: 0,
            hp: 100,
            exp: 0,
            gil: 0,
            status_immunities: 0xFFFF_FFFF,
        }
    }
}

/// Invented demo scene: two enemies, four formation slots, three attacks.
///
/// `variant` only changes the grunt's HP (`316 + variant`).
pub fn demo_scene(variant: u8) -> RawScene {
    let mut s = RawScene::blank();
    s.enemy_ids([0x0201, 0x0202, 0xFFFF]);
    s.setup(0, 0x0031, 0xFFFF, 0, 7);
    s.setup(1, 0x0031, 0x0123, 1, 7);
    s.setup(2, 0x0031, 0xFFFF, 0, 7);
    s.setup(3, 0x0031, 0xFFFF, 0, 7);
    for slot in 0..4 {
        s.camera(slot, [100, -200, 300], [-1, 2, -3]);
    }
    s.formation_entry(0, 0, 0x0201, [-111, 4, -2222], 1);
    s.formation_entry(0, 1, 0x0202, [333, 4, -2444], 1);
    s.formation_entry(1, 0, 0x0201, [-111, 4, -2222], 1);
    s.formation_entry(1, 1, 0x0201, [555, 4, -2666], 1);
    s.enemy(
        0,
        "Test Grunt",
        EnemySpec {
            level: 7,
            speed: 60,
            luck: 5,
            evade: 3,
            strength: 21,
            defense: 11,
            magic: 4,
            magic_defense: 9,
            element_codes: [0x00, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            element_rates: [0x04, 0x06, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            attack_ids: vec![0x0302, 0x0301],
            mp: 12,
            ap: 4,
            hp: 316 + u32::from(variant),
            exp: 77,
            gil: 55,
            status_immunities: 0xF0F0_0001,
        },
    );
    s.enemy(
        1,
        "Test Hound",
        EnemySpec {
            level: 4,
            speed: 70,
            luck: 2,
            evade: 6,
            strength: 14,
            defense: 5,
            magic: 0,
            magic_defense: 2,
            attack_ids: vec![0x0303],
            ap: 1,
            hp: 90,
            exp: 20,
            gil: 10,
            ..EnemySpec::default()
        },
    );
    s.attack(0, 0x0301, "Test Bite", 95, 0x11, 20);
    s.attack(1, 0x0302, "Test Fire", 100, 0x21, 30);
    s.attack(2, 0x0303, "Test Claw", 90, 0x11, 16);
    s
}

/// Pack gzip scenes into 8192-byte blocks, at most `per_block_max` scenes per block (and at
/// most what fits), padding each stream to a multiple of 4 bytes with `0xFF` as real files do.
pub fn pack_scene_bin(gz_scenes: &[Vec<u8>], per_block_max: usize) -> Vec<u8> {
    let per_block_max = per_block_max.clamp(1, BLOCK_POINTERS);
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < gz_scenes.len() {
        let mut block = vec![0xFFu8; BLOCK_SIZE];
        let mut cursor = BLOCK_HEADER_SIZE;
        let mut slot = 0;
        while idx < gz_scenes.len() && slot < per_block_max {
            let gz = &gz_scenes[idx];
            let padded = gz.len().div_ceil(4) * 4;
            if cursor + padded > BLOCK_SIZE {
                assert!(slot > 0, "scene too large for one block");
                break;
            }
            block[slot * 4..slot * 4 + 4].copy_from_slice(&((cursor / 4) as u32).to_le_bytes());
            block[cursor..cursor + gz.len()].copy_from_slice(gz);
            cursor += padded;
            slot += 1;
            idx += 1;
        }
        out.extend_from_slice(&block);
    }
    out
}

// --- synthetic kernel.bin builders ---

/// Build a kernel.bin-style archive: `(file_type, decompressed_bytes)` per section, each
/// preceded by the 6-byte header (u16 compressed size, u16 decompressed size, u16 type).
pub fn pack_kernel(sections: &[(u16, Vec<u8>)], trailing: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (ty, raw) in sections {
        let gz = gzip(raw);
        out.extend_from_slice(&(gz.len() as u16).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&gz);
    }
    out.extend_from_slice(trailing);
    out
}

/// Synthetic growth section with invented numbers.
///
/// Character `i` uses curve `i` for all six primary stats, HP curve `37 + i` and MP curve
/// `46 + i`. Every curve has gradient `100 + 10 * band + curve` and base `10 + band`, except
/// curves 37 (HP) and 46 (MP) of character 0, which carry hand-picked signed bases.
pub fn growth_section() -> Vec<u8> {
    let mut s = vec![0xFFu8; 0xF94];
    for i in 0..CHARACTER_COUNT {
        let r = i * GROWTH_RECORD_SIZE;
        for k in 0..6 {
            s[r + k] = i as u8; // all six primary stats on curve i
        }
        s[r + 6] = 37 + i as u8;
        s[r + 7] = 46 + i as u8;
        s[r + 8] = 55 + i as u8;
        s[r + 9] = 0xFF;
        // Invented signed level modifiers: 0, -1, -2, -3, 1, 2, 3, 4, 5.
        s[r + 10] = [0u8, 0xFF, 0xFE, 0xFD, 1, 2, 3, 4, 5][i];
    }
    // Invented gain tables (strictly increasing so tests can tell entries apart).
    s[OFF_STAT_GAIN..OFF_STAT_GAIN + 12].copy_from_slice(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6]);
    s[OFF_HP_GAIN..OFF_HP_GAIN + 12]
        .copy_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
    s[OFF_MP_GAIN..OFF_MP_GAIN + 12]
        .copy_from_slice(&[12, 22, 32, 42, 52, 62, 72, 82, 92, 102, 112, 122]);
    for n in 0..CURVE_COUNT {
        for b in 0..8usize {
            let o = OFF_CURVES + n * CURVE_SIZE + b * 2;
            s[o] = (100 + 10 * b + n) as u8;
            s[o + 1] = (10 + b) as u8;
        }
    }
    // HP curve of character 0 (curve 37): gradient 20 + 10 * band, signed bases.
    let hp_bases: [i8; 8] = [4, 0, -8, -16, -24, -32, -40, -48];
    for (b, base) in hp_bases.iter().enumerate() {
        let o = OFF_CURVES + 37 * CURVE_SIZE + b * 2;
        s[o] = (20 + b * 10) as u8;
        s[o + 1] = *base as u8;
    }
    // MP curve of character 0 (curve 46): gradient 60 + 10 * band.
    let mp_bases: [i8; 8] = [7, 1, -4, -10, -16, -22, -28, 61];
    for (b, base) in mp_bases.iter().enumerate() {
        let o = OFF_CURVES + 46 * CURVE_SIZE + b * 2;
        s[o] = (60 + b * 10) as u8;
        s[o + 1] = *base as u8;
    }
    // Scene lookup: invented block starts 0, 5, 9, 14, then unused.
    for (k, v) in [0u8, 5, 9, 14].iter().enumerate() {
        s[OFF_SCENE_LOOKUP + k] = *v;
    }
    s
}

/// Synthetic initialization section: records 6 and 7 carry ids 20 and 21 (so a lookup by
/// slot index would be wrong); every other slot holds its own index as id. Record `i` has
/// level `1 + i`, weapon id `i`, armor id `i % 3` and stats `10 + k + i`. Slot 3 starts in
/// the back row.
pub fn init_section() -> Vec<u8> {
    let mut s = vec![0u8; CHARACTER_COUNT * INIT_CHARACTER_SIZE];
    let ids = [0u8, 1, 2, 3, 4, 5, 20, 21, 8];
    for (i, id) in ids.iter().enumerate() {
        let r = i * INIT_CHARACTER_SIZE;
        s[r] = *id;
        s[r + 1] = 1 + i as u8;
        for k in 0..6 {
            s[r + 2 + k] = 10 + k as u8 + i as u8;
        }
        let mut name = text::encode("Hero").unwrap();
        name.resize(12, 0xFF);
        s[r + 0x10..r + 0x1C].copy_from_slice(&name);
        s[r + 0x1C] = i as u8;
        s[r + 0x1D] = (i % 3) as u8;
        s[r + 0x1E] = 0xFF;
        s[r + 0x20] = if i == 3 { 0xFE } else { 0xFF };
        s[r + 0x2E..r + 0x30].copy_from_slice(&(200 + i as u16).to_le_bytes());
        s[r + 0x32..r + 0x34].copy_from_slice(&(10 + i as u16).to_le_bytes());
    }
    s
}

pub fn command_section() -> Vec<u8> {
    let mut s = vec![0xFFu8; 256];
    // Command 1 ("Attack"): cursor action 7, target flags 0xFF, cameras 0xFFFF.
    s[8] = 7;
    // Command 2: cursor action 1, target flags 0x23, invented cameras 0x0123 / 0x0124.
    s[16] = 1;
    s[17] = 0x23;
    s[20..22].copy_from_slice(&0x0123u16.to_le_bytes());
    s[22..24].copy_from_slice(&0x0124u16.to_le_bytes());
    s
}

pub fn attack_section() -> Vec<u8> {
    let mut s = vec![0u8; 128 * ATTACK_RECORD_SIZE];
    // Record 3: accuracy 90, target flags 0x03, calc 0x11, power 24.
    let o = 3 * ATTACK_RECORD_SIZE;
    s[o] = 90;
    s[o + 0x0C] = 0x03;
    s[o + 0x0E] = 0x11;
    s[o + 0x0F] = 24;
    s
}

/// Weapons 0..8: even ids are short range (`0x23`), odd ids long range (`0x03`); attack
/// `10 + id`, crit `id`, accuracy `90 + id`, equip mask `1 << id`.
pub fn weapon_section() -> Vec<u8> {
    let mut s = vec![0xFFu8; 128 * WEAPON_RECORD_SIZE];
    for id in 0..8usize {
        let o = id * WEAPON_RECORD_SIZE;
        s[o] = if id % 2 == 0 { 0x23 } else { 0x03 };
        s[o + 2] = 0x11;
        s[o + 4] = 10 + id as u8;
        s[o + 7] = id as u8;
        s[o + 8] = 90 + id as u8;
        s[o + 0x0E..o + 0x10].copy_from_slice(&(1u16 << id).to_le_bytes());
    }
    s
}

/// Armor 0..4: defense `5 + 3 * id`, magic defense `2 + id`, evade `2 * id` percent.
pub fn armor_section() -> Vec<u8> {
    let mut s = vec![0xFFu8; 32 * ARMOR_RECORD_SIZE];
    for id in 0..4usize {
        let o = id * ARMOR_RECORD_SIZE;
        s[o + 2] = 5 + 3 * id as u8;
        s[o + 3] = 2 + id as u8;
        s[o + 4] = 2 * id as u8;
        s[o + 5] = 0;
        s[o + 0x12..o + 0x14].copy_from_slice(&0x01FFu16.to_le_bytes());
    }
    s
}

/// FF text table section from plain strings (pointer table, then terminated strings).
pub fn text_table(strings: &[&str]) -> Vec<u8> {
    let table_len = strings.len() * 2;
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    for s in strings {
        offsets.push((table_len + body.len()) as u16);
        body.extend(text::encode(s).unwrap());
        body.push(0xFF);
    }
    let mut out = Vec::new();
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend(body);
    out
}

/// Full synthetic kernel: typed sections 0..=3, raw data sections, and 18 text sections of
/// which only the command, weapon and armor name tables carry content.
pub fn synthetic_kernel_sections() -> Vec<(u16, Vec<u8>)> {
    let mut sections: Vec<(u16, Vec<u8>)> = vec![
        (0, command_section()),
        (1, attack_section()),
        (2, growth_section()),
        (3, init_section()),
        (4, vec![1, 2, 3]),
        (5, weapon_section()),
        (6, armor_section()),
        (7, vec![9; 16]),
        (8, vec![8; 20]),
    ];
    for i in 9..27usize {
        let table = match i {
            SECTION_TEXT_COMMAND_NAMES => text_table(&["Cmd0", "Attack", "Magic"]),
            SECTION_TEXT_WEAPON_NAMES => text_table(&["Test Blade", "Test Gun"]),
            SECTION_TEXT_ARMOR_NAMES => text_table(&["Test Bangle"]),
            _ => text_table(&["x"]),
        };
        sections.push((9, table));
    }
    sections
}

pub fn synthetic_kernel_bytes() -> Vec<u8> {
    pack_kernel(&synthetic_kernel_sections(), &[0, 0])
}
