//! Telling the mods apart: the `mod.xml` at the root of a 7th Heaven mod archive names the mod (an `<ID>` GUID, a
//! `<Name>` and a `<Version>`), and that ID, not the file's name, says which layer an archive is.
//!
//! Only those three elements are read, by scanning for their tags (an XML library is not worth a dependency for
//! three strings, and nothing here evaluates the file's conditions: which of a mod's folders are active is this
//! project's own choice, see [`crate::layers`]).

/// The `<ID>` of `[Tsunamods] Echo-S 7` (script, voices, music), lower case.
pub const ECHO_S_ID: &str = "09e81530-3f09-46b9-831b-df431b8f319c";
/// The `<ID>` of `[Tsunamods] Cosmo Memory` (the sound overhaul), lower case.
pub const COSMO_ID: &str = "2e4c6623-97b9-4d5c-b2f3-b4f24f06a911";

/// The most of a `mod.xml` that is read (the two in use are 9 128 and 7 796 bytes).
pub const MAX_MOD_XML: u64 = 256 * 1024;

/// What a `mod.xml` says about its mod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModInfo {
    /// The GUID, lower case.
    pub id: String,
    pub name: String,
    pub version: String,
}

/// The text between `<tag>` and `</tag>` (the first one), trimmed.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)?.checked_add(open.len())?;
    let rest = xml.get(start..)?;
    let end = rest.find(&close)?;
    Some(rest.get(..end)?.trim())
}

/// Read `mod.xml`'s ID, name and version (`None` without an ID, or when the text is not UTF-8).
pub fn parse(xml: &[u8]) -> Option<ModInfo> {
    let text = std::str::from_utf8(xml.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(xml)).ok()?;
    let id = element(text, "ID")?.to_ascii_lowercase();
    if id.is_empty() || id.len() > 64 {
        return None;
    }
    let clip = |s: &str| s.chars().take(80).collect::<String>();
    Some(ModInfo { id, name: clip(element(text, "Name").unwrap_or("")), version: clip(element(text, "Version").unwrap_or("")) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_three_elements() {
        let xml = b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<ModInfo>\n\t<ID>09E81530-3F09-46B9-831B-DF431B8F319C</ID>\n\t<Name>[Test] Synthetic</Name>\n\t<Version>1.24</Version>\n</ModInfo>";
        let info = parse(xml).unwrap();
        assert_eq!(info.id, ECHO_S_ID);
        assert_eq!(info.name, "[Test] Synthetic");
        assert_eq!(info.version, "1.24");
    }

    #[test]
    fn a_file_without_an_id_is_not_a_mod_manifest() {
        assert_eq!(parse(b"<ModInfo><Name>x</Name></ModInfo>"), None);
        assert_eq!(parse(b"<ID></ID>"), None);
        assert_eq!(parse(&[0xFF, 0xFE, 0x00]), None);
        assert_eq!(parse(b""), None);
    }

    #[test]
    fn missing_name_and_version_are_empty() {
        let info = parse(b"<ID>abc</ID>").unwrap();
        assert_eq!((info.name.as_str(), info.version.as_str()), ("", ""));
    }

    #[test]
    fn a_huge_id_is_refused() {
        let xml = format!("<ID>{}</ID>", "a".repeat(500));
        assert_eq!(parse(xml.as_bytes()), None);
    }
}
