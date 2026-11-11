//! The PC movie table: which `.avi` a field script's `PMVIE` operand names.
//!
//! **Evidence.** Read from the owned English Steam executable (`ff7_1.02/ff7_en`, 5,997,027 bytes, SHA-256
//! `26998200234f7a49a700bdc1ae5d0a43fd001cc909871a366cf82bdffaa351c9`; static reading only, nothing executed, no executable
//! bytes kept here, only the names). The movie preparation routine at `0x40ADB0` (the video-name half at `0x40B055`–`0x40B0CB`)
//! picks the name this way:
//!
//! * the movie id is the `PMVIE` operand itself (the handler at `0x61A224` stores the script byte at the module's id word);
//! * ids `0..=19` index one table of 20 names, the same on every disc (table at `.data` offset `0x7BAE80`);
//! * ids `20..` subtract 20 and index the table of the current disc (disc byte `0xDC0BDC`, minus one): disc 1 at `0x7BAED0`
//!   (ids 20..=53), disc 2 at `0x7BAF58` (ids 20..=61, the 42nd slot null), disc 3 at `0x7BB000` (ids 20..=29).
//!
//! The native code never bounds-checks the index; [`video_basename`] does, and an id past its table, or a null slot, is `None`.
//! The same routine pairs each video with a `.cam` member of `cd/moviecam.lgp` (see [`crate::movie_camera`]); the camera names
//! are not needed here. Cross-checks that agree with the table: `PMVIE 53` in the owned `md1stin` opening is `opening`
//! (`docs/DISC1-IMPLEMENTATION.md`, the research capture's `native-table.json` has id 53 → slot 33 → `opening.avi`); the
//! retained loader analysis pins id 23 → `ontrain` and id 24 → `mainplr`. The pinned editor mapping the route manifest cites
//! (`makoureactor` `Data.cpp`) names `mainplr`, `earithdd` and `funeral` too. `tests/movie_table_real.rs` re-reads all five
//! tables from the owned executable (ignored without it) and asserts every entry here.
//!
//! These are file *names* only. Whether this port can play a file is the client's admission policy, not this table's.

/// Ids `0..=19`, the same on every disc.
const COMMON: [&str; 20] = [
    "fship2", "fship2n", "d_ropego", "d_ropein", "u_ropein", "u_ropego", "gold2", "gold3", "gold4", "gold6", "gold5", "boogup",
    "boogdown", "junair_u", "junair_d", "junelein", "junelego", "junin_in", "junin_go", "moriya",
];

/// Disc 1, ids `20..=53`.
const DISC1: [&str; 34] = [
    "mkup", "northmk", "mk8", "ontrain", "mainplr", "smk", "southmk", "plrexp", "fallpl", "monitor", "bike", "mtnvl", "mtnvl2",
    "brgnvl", "nvlmk", "nivlsfs", "jenova_e", "junon", "hiwind0", "mtcrl", "gold1", "biskdead", "boogdemo", "boogstar",
    "setogake", "rcktfail", "jairofly", "jairofal", "gold7", "gold7_2", "earithdd", "funeral", "car_1209", "opening",
];

/// Disc 2, ids `20..=60` (slot 41, id 61, is null in the executable).
const DISC2: [&str; 41] = [
    "greatpit", "c_scene1", "c_scene2", "c_scene3", "biglight", "meteosky", "weapon0", "weapon1", "weapon2", "weapon3", "weapon4",
    "weapon5", "hwindfly", "phoenix", "nrcrl", "nrcrl_b", "dumcrush", "zmind01", "zmind02", "zmind03", "gelnica", "rcketoff",
    "white2", "junsea", "rckethit0", "rckethit1", "meteofix", "canonon", "feelwin0", "feelwin1", "canonht1", "canonht2",
    "canonh3f", "parashot", "hwindjet", "canonht0", "wh2e2", "loslake1", "lslmv", "canonh1p", "canon",
];

/// Disc 3, ids `20..=29`.
const DISC3: [&str; 10] =
    ["last4_2", "last4_3", "last4_4", "lastmap", "lastflor", "ending1", "ending3", "fcar", "white2", "ending2"];

/// The first id the per-disc tables cover.
pub const FIRST_DISC_ID: u16 = 20;

/// The `PMVIE` id of the opening movie (disc 1).
pub const OPENING: u16 = 53;

/// The video file name, without its `.avi`, that `PMVIE id` selects on `disc` (1, 2 or 3). `None` for an id past the disc's
/// table, a null slot, or a disc outside 1..=3.
pub fn video_basename(disc: u8, id: u16) -> Option<&'static str> {
    if let Some(name) = COMMON.get(usize::from(id)) {
        return Some(name);
    }
    let table: &[&'static str] = match disc {
        1 => &DISC1,
        2 => &DISC2,
        3 => &DISC3,
        _ => return None,
    };
    table.get(usize::from(id.checked_sub(FIRST_DISC_ID)?)).copied()
}

/// The video file name on disc 1, with its extension: what the disc-1 field scripts play.
pub fn disc1_file(id: u16) -> Option<String> {
    video_basename(1, id).map(|name| format!("{name}.avi"))
}

/// Every `(id, basename)` of a disc, in id order (the common ids first).
pub fn disc_table(disc: u8) -> Vec<(u16, &'static str)> {
    (0..=u16::MAX).map_while(|id| video_basename(disc, id).map(|name| (id, name))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_established_disc_one_ids() {
        assert_eq!(video_basename(1, 53), Some("opening"));
        assert_eq!(video_basename(1, 23), Some("ontrain"));
        assert_eq!(video_basename(1, 24), Some("mainplr"));
        assert_eq!(video_basename(1, 20), Some("mkup"));
        assert_eq!(video_basename(1, 51), Some("funeral"));
        assert_eq!(video_basename(1, OPENING), Some("opening"));
        assert_eq!(disc1_file(24).as_deref(), Some("mainplr.avi"));
    }

    #[test]
    fn ids_past_a_table_or_in_a_null_slot_have_no_name() {
        assert_eq!(video_basename(1, 54), None, "the native code would read past the table here");
        assert_eq!(video_basename(1, 1000), None);
        assert_eq!(video_basename(2, 61), None, "disc 2's last slot is null");
        assert_eq!(video_basename(2, 60), Some("canon"));
        assert_eq!(video_basename(3, 30), None);
        assert_eq!(video_basename(0, 20), None);
        assert_eq!(video_basename(4, 20), None);
        assert_eq!(video_basename(0, 3), Some("d_ropein"), "ids below 20 do not depend on the disc");
        assert_eq!(video_basename(1, 19), Some("moriya"));
        assert_eq!(video_basename(1, u16::MAX), None);
    }

    #[test]
    fn table_sizes_and_unique_names_per_disc() {
        assert_eq!(disc_table(1).len(), 54);
        assert_eq!(disc_table(2).len(), 61);
        assert_eq!(disc_table(3).len(), 30);
        for disc in 1..=3 {
            let table = disc_table(disc);
            assert!(table.iter().all(|&(_, n)| !n.is_empty() && !n.contains(['.', '/', '\\'])), "bare file stems only");
        }
        let d1 = disc_table(1);
        let mut names: Vec<_> = d1.iter().map(|&(_, n)| n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), d1.len());
    }
}
