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

/// A script section with one entity called `entity` whose talk script (slot 1) opens the dialogs in order, one
/// `MESSAGE window 0` each, and whose other slots are a bare `RET`.
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
    const HEADER: usize = 0x20;
    const SLOTS: usize = 32;
    let code_start = HEADER + 8 + SLOTS * 2;
    // Slot 0 and every slot but 1 share one RET at the start; slot 1 follows it.
    let ret_at = code_start;
    let talk_at = code_start + 1;
    let string_offset = talk_at + talk.len();
    let mut out = vec![0u8; HEADER];
    out[0] = 2;
    out[1] = 5;
    out[2] = 1;
    out[3] = 0;
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
        let at = if slot == 1 { talk_at } else { ret_at };
        out.extend_from_slice(&u16::try_from(at).unwrap_or(0).to_le_bytes());
    }
    out.push(0x00);
    out.extend_from_slice(talk);
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
}

impl Default for EchoSpec<'_> {
    fn default() -> Self {
        Self { version: "1.24", fields: Vec::new(), voices: Vec::new(), music: Vec::new(), sounds: Vec::new(), sfx_config: None, music_config: None }
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
        owned.push((format!("Echo-S\\flevel.lgp\\{name}"), field_entry(&script_section("npc", dialogs))));
    }
    for path in spec.voices.iter().chain(&spec.music).chain(&spec.sounds) {
        owned.push(((*path).to_string(), fake_ogg(path)));
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
