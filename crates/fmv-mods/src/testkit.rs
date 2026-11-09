//! Test support: synthetic mod archives built from invented names and bytes (never real game or mod data). Used by this
//! crate's tests and, through the `testkit` feature, by the tests of the crates that build on it.

// Test builders write fixed layouts into buffers they just made; the untrusted-data lints are for the readers.
#![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::iro::testkit::{build, stored, Spec};
use crate::modxml::ECHO_S_ID;

/// Printable ASCII as FF7 dialog bytes, terminated by `0xFF`.
pub fn dialog(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.bytes().map(|b| b.saturating_sub(0x20)).collect();
    v.push(0xFF);
    v
}

/// Where the synthetic character of [`script_section`] stands (its `XYZI`), in walkmesh units.
pub const NPC_AT: (i16, i16) = (100, 200);

/// A script section with one character entity called `entity` (script 0: `CHAR 0; XYZI` at [`NPC_AT`]; `RET`; `JMPB 0`) whose talk
/// script (slot 1) opens the dialogs in order, one `MESSAGE window 0` each, and whose other slots are a bare `RET`.
pub fn script_section(entity: &str, dialogs: &[&str]) -> Vec<u8> {
    let mut talk: Vec<u8> = Vec::new();
    for i in 0..dialogs.len() {
        talk.extend_from_slice(&[0x40, 0, u8::try_from(i).unwrap_or(0)]);
    }
    talk.push(0x00);
    script_section_code(entity, &talk, dialogs)
}

/// Like [`script_section`] with the talk script's code given (it must end the way a script does, with a `RET`).
pub fn script_section_code(entity: &str, talk: &[u8], dialogs: &[&str]) -> Vec<u8> {
    script_section_parts(entity, &[], talk, dialogs)
}

/// Like [`script_section_code`] with `init_extra` (code) run by the entity's init script after it has been placed, before its `RET`
/// (for example `[0x7E, 1]`, `TLKON 1`, which switches talking off).
pub fn script_section_parts(entity: &str, init_extra: &[u8], talk: &[u8], dialogs: &[&str]) -> Vec<u8> {
    const HEADER: usize = 0x20;
    const SLOTS: usize = 32;
    let code_start = HEADER + 8 + SLOTS * 2;
    // The init script of a model entity: CHAR 0; XYZI x y 0 0; RET; JMPB 0.
    let mut init: Vec<u8> = vec![0xA1, 0, 0xA5, 0, 0];
    for v in [NPC_AT.0, NPC_AT.1, 0] {
        init.extend(v.to_le_bytes());
    }
    init.extend(0u16.to_le_bytes());
    init.extend_from_slice(init_extra);
    init.extend([0x00, 0x12, 0]);
    // Slot 0 is the init script, slot 1 the talk script, every other slot shares one RET after them.
    let init_at = code_start;
    let talk_at = init_at + init.len();
    let ret_at = talk_at + talk.len();
    let string_offset = ret_at + 1;
    let mut out = vec![0u8; HEADER];
    out[0] = 2;
    out[1] = 5;
    out[2] = 1;
    out[3] = 1;
    out[4..6].copy_from_slice(&u16::try_from(string_offset).unwrap_or(0).to_le_bytes());
    out[8..10].copy_from_slice(&512u16.to_le_bytes());
    out[0x10..0x18].copy_from_slice(b"synth\0\0\0");
    out[0x18..0x20].copy_from_slice(b"fixture\0");
    let mut name = [0u8; 8];
    for (slot, b) in name.iter_mut().zip(entity.bytes()) {
        *slot = b;
    }
    out.extend_from_slice(&name);
    for slot in 0..SLOTS {
        let at = match slot {
            0 => init_at,
            1 => talk_at,
            _ => ret_at,
        };
        out.extend_from_slice(&u16::try_from(at).unwrap_or(0).to_le_bytes());
    }
    out.extend_from_slice(&init);
    out.extend_from_slice(talk);
    out.push(0x00);
    // The dialog table: count, pointers (from the table's start), strings.
    let strings: Vec<Vec<u8>> = dialogs.iter().map(|d| dialog(d)).collect();
    out.extend_from_slice(&u16::try_from(strings.len()).unwrap_or(0).to_le_bytes());
    let mut ptr = 2 + 2 * strings.len();
    for s in &strings {
        out.extend_from_slice(&u16::try_from(ptr).unwrap_or(0).to_le_bytes());
        ptr += s.len();
    }
    for s in &strings {
        out.extend_from_slice(s);
    }
    out
}

/// A stored (LZSS) field entry the way `flevel.lgp` holds one: the nine-section field file around `script`, run through a
/// literals-only LZSS stream behind its 4-byte size header.
pub fn field_entry(script: &[u8]) -> Vec<u8> {
    let sections: Vec<&[u8]> = std::iter::once(script).chain(std::iter::repeat_n(&[][..], 8)).collect();
    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&9u32.to_le_bytes());
    let mut at = 2 + 4 + 9 * 4;
    for s in &sections {
        file.extend_from_slice(&u32::try_from(at).unwrap_or(0).to_le_bytes());
        at += 4 + s.len();
    }
    for s in &sections {
        file.extend_from_slice(&u32::try_from(s.len()).unwrap_or(0).to_le_bytes());
        file.extend_from_slice(s);
    }
    let mut payload = Vec::new();
    for chunk in file.chunks(8) {
        // One control byte: a set bit is a literal byte (a group of fewer than eight ends the stream cleanly).
        payload.push(0xFF);
        payload.extend_from_slice(chunk);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// What a synthetic Echo-S archive holds.
pub struct EchoSpec<'a> {
    pub version: &'a str,
    /// The name of the one character entity of every replacement field (`npc`).
    pub entity: &'a str,
    /// Code the character's init script runs before its `RET` (none).
    pub init_extra: Vec<u8>,
    /// Replacement fields: name and the text of each dialog (printable ASCII).
    pub fields: Vec<(&'a str, Vec<&'a str>)>,
    /// Archive paths of voice clips (their bytes are `OggS` and the path: not audio).
    pub voices: Vec<&'a str>,
    /// Archive paths of music tracks.
    pub music: Vec<&'a str>,
    /// Archive paths of sound effect clips.
    pub sounds: Vec<&'a str>,
    pub sfx_config: Option<String>,
    pub music_config: Option<String>,
    /// Files with bytes of the test's own (a synthetic WAV where a test must decode something), by archive path.
    pub blobs: Vec<(&'a str, Vec<u8>)>,
}

impl Default for EchoSpec<'_> {
    fn default() -> Self {
        Self {
            version: "1.24",
            entity: "npc",
            init_extra: Vec::new(),
            fields: Vec::new(),
            voices: Vec::new(),
            music: Vec::new(),
            sounds: Vec::new(),
            sfx_config: None,
            music_config: None,
            blobs: Vec::new(),
        }
    }
}

/// A fake Ogg file for `path`: the capture pattern and the path, so that two clips differ.
pub fn fake_ogg(path: &str) -> Vec<u8> {
    let mut v = b"OggS".to_vec();
    v.extend_from_slice(path.as_bytes());
    v
}

/// The bytes of a synthetic Echo-S archive: its `mod.xml`, the replacement fields, the clips and the two configs.
pub fn echo_archive(spec: &EchoSpec<'_>) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\"?><ModInfo><ID>{}</ID><Name>[Test] Echo-S</Name><Version>{}</Version></ModInfo>",
        ECHO_S_ID.to_ascii_uppercase(),
        spec.version
    );
    let mut owned: Vec<(String, Vec<u8>)> = vec![("mod.xml".to_string(), xml.into_bytes())];
    for (name, dialogs) in &spec.fields {
        let talk: Vec<u8> = (0..dialogs.len()).flat_map(|i| [0x40, 0, u8::try_from(i).unwrap_or(0)]).chain([0x00]).collect();
        owned.push((format!("Echo-S\\flevel.lgp\\{name}"), field_entry(&script_section_parts(spec.entity, &spec.init_extra, &talk, dialogs))));
    }
    for path in spec.voices.iter().chain(&spec.music).chain(&spec.sounds) {
        owned.push(((*path).to_string(), fake_ogg(path)));
    }
    for (path, bytes) in &spec.blobs {
        owned.push(((*path).to_string(), bytes.clone()));
    }
    if let Some(c) = &spec.sfx_config {
        owned.push(("Sound\\sfx\\config.toml".to_string(), c.clone().into_bytes()));
    }
    if let Some(c) = &spec.music_config {
        owned.push(("EchoeS Of The Planet\\vgmstream\\config.toml".to_string(), c.clone().into_bytes()));
    }
    let files: Vec<Spec<'_>> = owned.iter().map(|(n, b)| stored(n, b)).collect();
    build(&files)
}

/// A 16-bit mono PCM WAV file of `samples` at `rate` Hz (synthetic audio for tests that decode something).
pub fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(0);
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// The bytes of a synthetic Cosmo Memory archive with files of the test's own bytes (see [`cosmo_archive`]).
pub fn cosmo_archive_blobs(blobs: &[(&str, Vec<u8>)], sfx_config: Option<&str>) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\"?><ModInfo><ID>{}</ID><Name>[Test] Cosmo</Name><Version>1.22</Version></ModInfo>",
        crate::modxml::COSMO_ID.to_ascii_uppercase()
    );
    let mut owned: Vec<(String, Vec<u8>)> = vec![("mod.xml".to_string(), xml.into_bytes())];
    for (path, bytes) in blobs {
        owned.push(((*path).to_string(), bytes.clone()));
    }
    if let Some(c) = sfx_config {
        owned.push(("Base\\sfx\\config.toml".to_string(), c.as_bytes().to_vec()));
    }
    let files: Vec<Spec<'_>> = owned.iter().map(|(n, b)| stored(n, b)).collect();
    build(&files)
}

/// The bytes of a synthetic Cosmo Memory archive: its `mod.xml` and the clips (under `Base\sfx` and the like) and its config.
pub fn cosmo_archive(clips: &[&str], sfx_config: Option<&str>) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\"?><ModInfo><ID>{}</ID><Name>[Test] Cosmo</Name><Version>1.22</Version></ModInfo>",
        crate::modxml::COSMO_ID.to_ascii_uppercase()
    );
    let mut owned: Vec<(String, Vec<u8>)> = vec![("mod.xml".to_string(), xml.into_bytes())];
    for path in clips {
        owned.push(((*path).to_string(), fake_ogg(path)));
    }
    if let Some(c) = sfx_config {
        owned.push(("Base\\sfx\\config.toml".to_string(), c.as_bytes().to_vec()));
    }
    let files: Vec<Spec<'_>> = owned.iter().map(|(n, b)| stored(n, b)).collect();
    build(&files)
}
