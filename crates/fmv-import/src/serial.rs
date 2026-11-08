//! PlayStation boot-serial parsing and the FF7 serial table.
//!
//! The table below was written from recall of community disc databases. NONE of it has been
//! checked against owned media: every entry is `[verify]`, every match is reported as
//! unverified, and unknown serials are accepted but flagged.

/// FF7 shipped on three discs in every edition.
pub const EXPECTED_DISCS: u8 = 3;

/// Whether the serial table has been verified against real discs. Always false for now; the
/// importer surfaces this next to every table match.
pub const SERIAL_TABLE_VERIFIED: bool = false;

/// Largest SYSTEM.CNF the importer will read (real ones are a few dozen bytes).
pub const MAX_SYSTEM_CNF: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownDisc {
    pub serial: &'static str,
    pub disc: u8,
    pub edition: &'static str,
    pub region: &'static str,
    pub language: &'static str,
}

const fn known(
    serial: &'static str,
    disc: u8,
    edition: &'static str,
    region: &'static str,
    language: &'static str,
) -> KnownDisc {
    KnownDisc {
        serial,
        disc,
        edition,
        region,
        language,
    }
}

/// Known FF7 PS1 discs. EVERY ENTRY IS [verify].
pub const FF7_DISCS: &[KnownDisc] = &[
    // NTSC-U original, discs 1-3
    known("SCUS-94163", 1, "PS1 original", "NTSC-U", "en"), // [verify]
    known("SCUS-94164", 2, "PS1 original", "NTSC-U", "en"), // [verify]
    known("SCUS-94165", 3, "PS1 original", "NTSC-U", "en"), // [verify]
    // PAL English
    known("SCES-00867", 1, "PS1 original", "PAL", "en"), // [verify]
    known("SCES-10867", 2, "PS1 original", "PAL", "en"), // [verify]
    known("SCES-20867", 3, "PS1 original", "PAL", "en"), // [verify]
    // PAL French
    known("SCES-00868", 1, "PS1 original", "PAL", "fr"), // [verify]
    known("SCES-10868", 2, "PS1 original", "PAL", "fr"), // [verify]
    known("SCES-20868", 3, "PS1 original", "PAL", "fr"), // [verify]
    // PAL German
    known("SCES-00869", 1, "PS1 original", "PAL", "de"), // [verify]
    known("SCES-10869", 2, "PS1 original", "PAL", "de"), // [verify]
    known("SCES-20869", 3, "PS1 original", "PAL", "de"), // [verify]
    // PAL Spanish
    known("SCES-00900", 1, "PS1 original", "PAL", "es"), // [verify]
    known("SCES-10900", 2, "PS1 original", "PAL", "es"), // [verify]
    known("SCES-20900", 3, "PS1 original", "PAL", "es"), // [verify]
    // NTSC-J original
    known("SLPS-00700", 1, "PS1 original", "NTSC-J", "ja"), // [verify]
    known("SLPS-00701", 2, "PS1 original", "NTSC-J", "ja"), // [verify]
    known("SLPS-00702", 3, "PS1 original", "NTSC-J", "ja"), // [verify]
    // NTSC-J International
    known("SLPS-01057", 1, "PS1 International", "NTSC-J", "ja"), // [verify]
    known("SLPS-01058", 2, "PS1 International", "NTSC-J", "ja"), // [verify]
    known("SLPS-01059", 3, "PS1 International", "NTSC-J", "ja"), // [verify]
];

pub fn lookup_serial(serial: &str) -> Option<&'static KnownDisc> {
    FF7_DISCS
        .iter()
        .find(|k| k.serial.eq_ignore_ascii_case(serial))
}

/// Table entries of one family (edition + region + language), ordered by disc number.
pub fn expected_serials(edition: &str, region: &str, language: &str) -> Vec<&'static KnownDisc> {
    let mut v: Vec<_> = FF7_DISCS
        .iter()
        .filter(|k| k.edition == edition && k.region == region && k.language == language)
        .collect();
    v.sort_by_key(|k| k.disc);
    v
}

/// Region implied by a serial prefix (`SCUS`/`SLUS` NTSC-U, `SCES`/`SLES` PAL, `SCPS`/`SLPS`/`SCPM`/
/// `SLPM` NTSC-J). This is an inference from the publisher code, not a table match.
pub fn region_from_prefix(serial: &str) -> &'static str {
    match serial.get(2..4) {
        Some("US") => "NTSC-U",
        Some("ES") => "PAL",
        Some("PS") | Some("PM") => "NTSC-J",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootInfo {
    /// Executable name from the BOOT line, uppercased, e.g. `SCUS_941.63`.
    pub exe_name: String,
    /// Normalised serial, e.g. `SCUS-94163`.
    pub serial: String,
}

/// What the importer knows about a disc from its serial alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscIdentity {
    pub serial: String,
    pub edition: String,
    /// The table's region, or `"unknown"` for a serial the table does not know. A region read
    /// off the serial prefix is a guess and lives in `region_hint`, never here.
    pub region: String,
    pub language: String,
    /// Region implied by the prefix of a serial the table does not know (`Some("PAL")` for
    /// `SCES-...`). `None` for table matches, and when the prefix says nothing.
    pub region_hint: Option<String>,
    /// Disc number from the table (None for serials the table does not know).
    pub table_disc: Option<u8>,
    /// true when the serial matched a table entry. The table itself is UNVERIFIED.
    pub serial_known: bool,
}

pub fn identify(serial: &str) -> DiscIdentity {
    match lookup_serial(serial) {
        Some(k) => DiscIdentity {
            serial: k.serial.to_string(),
            edition: k.edition.to_string(),
            region: k.region.to_string(),
            language: k.language.to_string(),
            region_hint: None,
            table_disc: Some(k.disc),
            serial_known: true,
        },
        None => DiscIdentity {
            serial: serial.to_ascii_uppercase(),
            edition: "PS1 unknown".to_string(),
            region: "unknown".to_string(),
            language: "unknown".to_string(),
            region_hint: Some(region_from_prefix(serial))
                .filter(|r| *r != "unknown")
                .map(str::to_string),
            table_disc: None,
            serial_known: false,
        },
    }
}

/// Turn an executable name such as `SCUS_941.63` into `SCUS-94163`.
fn serial_from_exe_name(name: &str) -> Option<String> {
    let mut chars = name.trim().chars();
    let prefix: String = chars.by_ref().take(4).collect();
    if prefix.len() != 4 || !prefix.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let rest: String = chars.collect();
    let rest = rest.trim_start_matches(['_', '-']);
    if rest.matches('.').count() > 1 {
        return None;
    }
    let digits: String = rest.chars().filter(|c| *c != '.').collect();
    if digits.len() != 5 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}-{}", prefix.to_ascii_uppercase(), digits))
}

/// Parse SYSTEM.CNF and return the serial of the BOOT (or BOOT2) executable.
/// Accepts `BOOT = cdrom:\SCUS_941.63;1`, `BOOT=cdrom0:\SLES_008.67;1`, `cdrom:SCUS_941.63;1`, any
/// letter case, CRLF/LF/CR line ends. Never panics; anything unparsable yields None.
pub fn parse_boot_serial(system_cnf: &[u8]) -> Option<BootInfo> {
    let limited = system_cnf.get(..system_cnf.len().min(MAX_SYSTEM_CNF as usize))?;
    let text = String::from_utf8_lossy(limited);
    for line in text.split(['\r', '\n']) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !(key.eq_ignore_ascii_case("BOOT") || key.eq_ignore_ascii_case("BOOT2")) {
            continue;
        }
        let file_part = value.trim().rsplit(['\\', '/', ':']).next().unwrap_or("");
        let name = file_part.split(';').next().unwrap_or("").trim();
        if let Some(serial) = serial_from_exe_name(name) {
            return Some(BootInfo {
                exe_name: name.to_ascii_uppercase(),
                serial,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial(s: &str) -> Option<String> {
        parse_boot_serial(s.as_bytes()).map(|b| b.serial)
    }

    #[test]
    fn parses_boot_lines() {
        assert_eq!(
            serial("BOOT = cdrom:\\SCUS_941.63;1\r\nTCB = 4\r\nEVENT = 16\r\nSTACK = 801FFF00\r\n")
                .as_deref(),
            Some("SCUS-94163")
        );
        assert_eq!(
            serial("BOOT=cdrom:\\SCUS_941.63;1").as_deref(),
            Some("SCUS-94163")
        );
        assert_eq!(
            serial("boot = cdrom0:\\SLES_008.67;1\n").as_deref(),
            Some("SLES-00867")
        );
        assert_eq!(
            serial("TCB = 4\nBOOT = cdrom:SCES_108.67;1\n").as_deref(),
            Some("SCES-10867")
        );
        assert_eq!(
            serial("BOOT2 = cdrom0:/SLPS_010.57;1").as_deref(),
            Some("SLPS-01057")
        );
        assert_eq!(
            serial("BOOT = cdrom:\\SCUS_941.63\rTCB = 4\r").as_deref(),
            Some("SCUS-94163")
        );
        assert_eq!(
            serial("BOOT = cdrom:\\SCUS-94163;1").as_deref(),
            Some("SCUS-94163")
        );
        assert_eq!(
            parse_boot_serial(b"BOOT = cdrom:\\SCUS_941.63;1")
                .map(|b| b.exe_name)
                .as_deref(),
            Some("SCUS_941.63")
        );
    }

    #[test]
    fn rejects_junk_without_panicking() {
        for junk in [
            "",
            "BOOT",
            "BOOT =",
            "BOOT = cdrom:\\PSX.EXE;1",
            "BOOT = cdrom:\\SCUS_941.6;1",
            "BOOT = cdrom:\\SCUS_941.633;1",
            "BOOT = cdrom:\\SC_US941.63;1",
            "BOOT = cdrom:\\SCUS_9.41.63;1",
            "TCB = 4",
            "= = =",
            "BOOT = \u{feff}\u{0}\u{1}",
            "BOOT = cdrom:\\\u{e9}\u{e9}\u{e9}\u{e9}_941.63;1",
        ] {
            assert_eq!(serial(junk), None, "{junk:?}");
        }
        assert!(parse_boot_serial(&[0xff, 0xfe, 0x00, 0x42]).is_none());
        // oversized input is cut off, not scanned in full
        let mut big = vec![b'x'; 10_000];
        big.extend_from_slice(b"\nBOOT = cdrom:\\SCUS_941.63;1\n");
        assert!(parse_boot_serial(&big).is_none());
    }

    #[test]
    fn table_is_consistent_and_labelled_unverified() {
        // compile-time: flipping the flag must be a conscious change that also revisits this test
        const { assert!(!SERIAL_TABLE_VERIFIED) };
        let mut seen = std::collections::HashSet::new();
        for k in FF7_DISCS {
            assert!(seen.insert(k.serial), "duplicate serial {}", k.serial);
            assert!((1..=EXPECTED_DISCS).contains(&k.disc));
            assert_eq!(region_from_prefix(k.serial), k.region, "{}", k.serial);
        }
        // every family has exactly discs 1..=3
        for k in FF7_DISCS {
            let fam = expected_serials(k.edition, k.region, k.language);
            let discs: Vec<u8> = fam.iter().map(|d| d.disc).collect();
            assert_eq!(
                discs,
                vec![1, 2, 3],
                "{} {} {}",
                k.edition,
                k.region,
                k.language
            );
        }
    }

    #[test]
    fn identify_known_and_unknown() {
        let k = identify("scus-94163");
        assert!(k.serial_known);
        assert_eq!(
            (k.region.as_str(), k.language.as_str(), k.table_disc),
            ("NTSC-U", "en", Some(1))
        );
        assert_eq!(k.region_hint, None, "a table match needs no guess");
        let u = identify("SCES-99999");
        assert!(!u.serial_known);
        // the region of an unknown serial is "unknown": the prefix only gives a separate hint
        assert_eq!(
            (u.region.as_str(), u.edition.as_str(), u.table_disc),
            ("unknown", "PS1 unknown", None)
        );
        assert_eq!(u.region_hint.as_deref(), Some("PAL"));
        assert_eq!(identify("SLUS-00892").region_hint.as_deref(), Some("NTSC-U"));
        assert_eq!(identify("SLPM-86000").region_hint.as_deref(), Some("NTSC-J"));
        assert_eq!(identify("ABCD-12345").region, "unknown");
        assert_eq!(identify("ABCD-12345").region_hint, None);
        // a short/odd string must not panic in the region inference
        assert_eq!(identify("é").region, "unknown");
        assert_eq!(identify("").region_hint, None);
    }
}
