//! Checks of the sound-effect reader (`fmv_ff7::sound`) against the owner's REAL FF7 sound files
//! (FF7 Steam Edition, English; verified 2026-11-09).
//!
//! Every test is `#[ignore]` and also returns early when its environment variable is unset, so a
//! normal `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_AUDIO_DATA=<.../data> FMV_FF7_DATA=<.../data> \
//!   cargo test -p fmv-ff7 --release --test sound_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_AUDIO_DATA` is the `data/` directory that holds `sound/audio.fmt` and `sound/audio.dat`;
//! `FMV_FF7_DATA` is the game's `data/` directory (the one with `field/flevel.lgp`, used only by the
//! sound-id check). Both are only ever opened read-only. The assertions and the printed evidence are
//! counts, ids and measurements: no game bytes, no decoded audio and no paths are stored or printed.
//! Set `FMV_FF7_SFX_PRINT_HASHES=1` to print one `HASH index frames channels crc32` line per clip (the
//! CRC-32 of the little-endian 16-bit interleaved samples, the same as `zlib.crc32` of ffmpeg's
//! `-f s16le` output), to compare against another decoder: that is how the all-clips ffmpeg comparison
//! in the module docs was made.

mod script_common;

use fmv_ff7::script::{FieldScript, SCRIPTS_PER_ENTITY};
use fmv_ff7::sound::{entry_for_sound_id, PcmClip, SfxBank, MENU_BUZZER, MENU_CANCEL, MENU_CONFIRM, MENU_CURSOR};
use script_common::{data_dir, flevel_path, is_field_name, load_field};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const AUDIO_ENV: &str = "FMV_FF7_AUDIO_DATA";

fn audio_paths() -> Option<(PathBuf, PathBuf)> {
    let Some(dir) = std::env::var_os(AUDIO_ENV).map(PathBuf::from) else {
        eprintln!("{AUDIO_ENV} is not set; skipping");
        return None;
    };
    Some((dir.join("sound").join("audio.fmt"), dir.join("sound").join("audio.dat")))
}

fn open_bank() -> Option<SfxBank> {
    let (fmt, dat) = audio_paths()?;
    Some(SfxBank::open(&fmt, &dat).unwrap())
}

/// One raw record of `audio.fmt`, read here independently of the module under test.
#[derive(Debug, Clone, Copy)]
struct Raw {
    length: u32,
    offset: u32,
    flag: u32,
    count: u32,
    loop_start: u32,
    loop_end: u32,
    tag: u16,
    channels: u16,
    rate: u32,
    align: u16,
    bits: u16,
    extra: u16,
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}

/// Walk `audio.fmt` by the rule the module docs give: 42 bytes for a record of length 0, else
/// 42 + cbSize. Returns the records and the position where the walk ended.
fn raw_walk(fmt: &[u8]) -> (Vec<Raw>, usize) {
    let mut pos = 0;
    let mut records = Vec::new();
    while pos + 42 <= fmt.len() {
        let length = u32_at(fmt, pos);
        let mut r = Raw {
            length,
            offset: u32_at(fmt, pos + 4),
            flag: u32_at(fmt, pos + 8),
            count: u32_at(fmt, pos + 12),
            loop_start: u32_at(fmt, pos + 16),
            loop_end: u32_at(fmt, pos + 20),
            tag: 0,
            channels: 0,
            rate: 0,
            align: 0,
            bits: 0,
            extra: 0,
        };
        if length == 0 {
            // an empty slot: its 18 format bytes are the MSVC debug fill
            assert!(fmt[pos + 24..pos + 42].iter().all(|&b| b == 0xCD), "record {} format area", records.len());
            records.push(r);
            pos += 42;
            continue;
        }
        r.tag = u16_at(fmt, pos + 24);
        r.channels = u16_at(fmt, pos + 26);
        r.rate = u32_at(fmt, pos + 28);
        r.align = u16_at(fmt, pos + 36);
        r.bits = u16_at(fmt, pos + 38);
        r.extra = u16_at(fmt, pos + 40);
        pos += 42 + usize::from(r.extra);
        records.push(r);
    }
    (records, pos)
}

#[test]
#[ignore = "needs the owner's FF7 sound files (FMV_FF7_AUDIO_DATA)"]
fn real_index_walks_exactly_and_parses() {
    let Some((fmt_path, dat_path)) = audio_paths() else { return };
    let fmt = std::fs::read(&fmt_path).unwrap();
    let dat_len = std::fs::metadata(&dat_path).unwrap().len();
    assert_eq!((fmt.len(), dat_len), (54_668, 71_738_528));

    // the independent walk: ends exactly at the end of the file, offsets chain exactly to the end of audio.dat
    let (records, end) = raw_walk(&fmt);
    assert_eq!(end, fmt.len(), "the record walk must end exactly at the end of audio.fmt");
    assert_eq!(records.len(), 750);
    let empties: Vec<usize> = records.iter().enumerate().filter(|(_, r)| r.length == 0).map(|(i, _)| i).collect();
    assert_eq!(empties, [198, 209, 475, 476, 487, 523, 574, 575, 579, 617, 730, 731, 732, 733, 734, 735, 736, 737, 738, 743, 744, 745, 746, 747, 748, 749]);
    assert_eq!(empties.len() * 42 + (records.len() - empties.len()) * 74, fmt.len());
    let mut running = 0u64;
    for (i, r) in records.iter().enumerate() {
        assert_eq!(u64::from(r.offset), running, "record {i} offset chain");
        running += u64::from(r.length);
    }
    assert_eq!(running, dat_len, "the clips tile audio.dat exactly");
    // the record fields as documented
    assert!(records.iter().all(|r| r.count == 0));
    assert!(records.iter().all(|r| r.flag <= 1));
    assert!(records.iter().filter(|r| r.length != 0).all(|r| r.tag == 2 && r.bits == 4 && r.rate == 44_100 && r.extra == 32));
    assert!(records.iter().filter(|r| r.length == 0).all(|r| r.flag == 0 && r.loop_start == 0 && r.loop_end == 0));
    assert!(records.iter().filter(|r| r.flag == 0).all(|r| r.loop_start == 0 && r.loop_end == 0));
    let mono = records.iter().filter(|r| r.length != 0 && r.channels == 1 && r.align == 1024).count();
    let stereo = records.iter().filter(|r| r.length != 0 && r.channels == 2 && r.align == 2048).count();
    assert_eq!((mono, stereo), (716, 8));
    let block_multiples = records.iter().filter(|r| r.length != 0 && r.length % u32::from(r.align) == 0).count();
    println!("clips whose length is a whole number of blocks: {block_multiples} of 724");
    assert_eq!(block_multiples, 117);

    // the module under test agrees
    let bank = SfxBank::parse(&fmt, dat_len).unwrap();
    assert_eq!((bank.len(), bank.clip_count()), (750, 724));
    assert_eq!(bank.dat_len(), dat_len);
    let mut looped = 0;
    let mut total_frames = 0u64;
    let mut slack = (u64::MAX, 0u64);
    for (i, r) in records.iter().enumerate() {
        let info = bank.info(i);
        assert_eq!(info.is_none(), r.length == 0, "entry {i}");
        let Some(info) = info else { continue };
        assert_eq!((info.rate, info.bytes), (44_100, r.length));
        assert_eq!(info.channels, r.channels);
        total_frames += info.frames;
        // loops: the flag, the units (decoded 16-bit bytes) and the bound all hold in every record
        assert_eq!(info.loop_region.is_some(), r.flag == 1, "entry {i} loop");
        if let Some(region) = info.loop_region {
            looped += 1;
            let unit = u64::from(r.channels) * 2;
            assert_eq!((region.start * unit, region.end * unit), (u64::from(r.loop_start), u64::from(r.loop_end)));
            assert!(region.start < region.end && region.end <= info.frames);
            let s = info.frames - region.end;
            slack = (slack.0.min(s), slack.1.max(s));
        }
    }
    println!("entries {} clips {} empty {} loops {looped} frames {total_frames} loop-end slack {slack:?}", bank.len(), bank.clip_count(), empties.len());
    assert_eq!(looped, 90);
    assert_eq!(total_frames, 134_432_642);
    assert_eq!(slack, (1, 2033));
    // the id convention: id n is entry n - 1; id 0 has none
    assert_eq!(entry_for_sound_id(0), None);
    assert_eq!(entry_for_sound_id(1), Some(0));
    assert!(bank.info(entry_for_sound_id(750).unwrap()).is_none(), "id 750 is an empty slot");
    assert!(bank.info(entry_for_sound_id(751).unwrap()).is_none(), "id 751 is past the end");
    // FFNx's anchors: id 493 is the looping Tiny Bronco engine drone, id 159 the footstep
    assert!(bank.info(492).unwrap().loop_region.is_some());
    assert!(bank.info(493).unwrap().loop_region.is_none());
    assert!(bank.info(158).unwrap().loop_region.is_none());
}

/// CRC-32 (IEEE, the one `zlib.crc32` computes) lookup table.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// CRC-32 of the samples as little-endian 16-bit bytes.
fn crc32(samples: &[i16]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for s in samples {
        for b in s.to_le_bytes() {
            c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    !c
}

#[test]
#[ignore = "needs the owner's FF7 sound files (FMV_FF7_AUDIO_DATA)"]
fn real_every_clip_decodes() {
    let Some(bank) = open_bank() else { return };
    let print_hashes = std::env::var_os("FMV_FF7_SFX_PRINT_HASHES").is_some();
    let started = std::time::Instant::now();
    let (mut total, mut clips) = (0u64, 0);
    let mut shortest = (u64::MAX, 0usize);
    let mut longest = (0u64, 0usize);
    let (mut silent, mut clipped_peaks) = (0, 0);
    for index in 0..bank.len() {
        let Some(info) = bank.info(index) else {
            assert!(bank.decode(index).is_err(), "empty slot {index} must not decode");
            continue;
        };
        let clip = bank.decode(index).unwrap_or_else(|e| panic!("entry {index}: {e}"));
        assert_eq!(clip.frames() as u64, info.frames, "entry {index} frame count");
        assert_eq!(clip.samples.len() as u64, info.frames * u64::from(info.channels));
        assert_eq!((clip.rate, clip.channels, clip.loop_region), (info.rate, info.channels, info.loop_region));
        total += info.frames;
        clips += 1;
        shortest = shortest.min((info.frames, index));
        longest = longest.max((info.frames, index));
        let peak = clip.samples.iter().map(|s| i32::from(*s).abs()).max().unwrap_or(0);
        silent += usize::from(peak == 0);
        clipped_peaks += usize::from(peak >= 32767);
        if print_hashes {
            println!("HASH {index} {} {} {:08x}", info.frames, info.channels, crc32(&clip.samples));
        }
    }
    println!(
        "decoded {clips} clips, {total} frames in {:.1?}; shortest entry {} ({} frames), longest entry {} ({} frames); silent clips {silent}; clips peaking at full scale {clipped_peaks}",
        started.elapsed(),
        shortest.1,
        shortest.0,
        longest.1,
        longest.0
    );
    assert_eq!((clips, total), (724, 134_432_642));
    assert_eq!(shortest, (2036, 43));
    assert_eq!(longest, (3_872_472, 740));
    // the empty slots refuse to decode with the specific error
    assert_eq!(bank.decode(198).err().map(|e| e.to_string()), Some("sound-effect entry 198 is an empty slot".to_string()));
    // and nothing past the end
    assert!(bank.decode(750).is_err());
}

/// Measurements of a decoded clip: duration, peak, rms, zero-crossing rate and a spectral peak (a
/// Goertzel scan of a Hann-windowed first 8192 frames over 160 log-spaced bins from 40 Hz to 10 kHz).
#[derive(Debug)]
struct Stats {
    ms: f64,
    peak: i32,
    rms: f64,
    zcr_hz: f64,
    dominant_hz: f64,
    centroid_hz: f64,
}

fn analyse(clip: &PcmClip) -> Stats {
    let ch = usize::from(clip.channels);
    let mono: Vec<f64> = clip.samples.chunks(ch).map(|f| f.iter().map(|s| f64::from(*s)).sum::<f64>() / ch as f64).collect();
    let rate = f64::from(clip.rate);
    let n = mono.len();
    let peak = mono.iter().fold(0f64, |m, v| m.max(v.abs())) as i32;
    let rms = (mono.iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt();
    let crossings = mono.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    let seconds = n as f64 / rate;
    let seg = &mono[..n.min(8192)];
    let len = seg.len();
    let windowed: Vec<f64> = seg
        .iter()
        .enumerate()
        .map(|(k, v)| v * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / (len as f64 - 1.0)).cos()))
        .collect();
    let freqs: Vec<f64> = (0..160).map(|j| 40.0 * (10_000.0f64 / 40.0).powf(f64::from(j) / 159.0)).collect();
    let power: Vec<f64> = freqs
        .iter()
        .map(|f| {
            let c = 2.0 * (2.0 * std::f64::consts::PI * f / rate).cos();
            let (mut s1, mut s2) = (0.0, 0.0);
            for v in &windowed {
                let s = v + c * s1 - s2;
                s2 = s1;
                s1 = s;
            }
            s1 * s1 + s2 * s2 - c * s1 * s2
        })
        .collect();
    let best = (0..power.len()).max_by(|a, b| power[*a].total_cmp(&power[*b])).unwrap();
    let total: f64 = power.iter().sum();
    let centroid = power.iter().zip(&freqs).map(|(p, f)| p * f).sum::<f64>() / total;
    Stats { ms: 1000.0 * seconds, peak, rms, zcr_hz: crossings as f64 / 2.0 / seconds, dominant_hz: freqs[best], centroid_hz: centroid }
}

#[test]
#[ignore = "needs the owner's FF7 sound files (FMV_FF7_AUDIO_DATA)"]
fn real_menu_sounds_measure_like_menu_sounds() {
    let Some(bank) = open_bank() else { return };
    let measure = |id: u16| {
        let entry = entry_for_sound_id(id).unwrap();
        let clip = bank.decode(entry).unwrap();
        let stats = analyse(&clip);
        println!(
            "sound id {id} (entry {entry}): {} ch, {:.0} ms, peak {}, rms {:.0}, zero-crossing {:.0} Hz, spectral peak {:.0} Hz, centroid {:.0} Hz, loop {:?}",
            clip.channels, stats.ms, stats.peak, stats.rms, stats.zcr_hz, stats.dominant_hz, stats.centroid_hz, clip.loop_region
        );
        assert_eq!((clip.rate, clip.channels, clip.loop_region), (44_100, 1, None));
        stats
    };
    // the confirm is the cursor's click; id 2 is measured under its own name, a chime that is no menu constant
    assert_eq!(MENU_CONFIRM, MENU_CURSOR);
    let cursor = measure(MENU_CURSOR);
    let chime = measure(2);
    let buzzer = measure(MENU_BUZZER);
    let cancel = measure(MENU_CANCEL);

    // the cursor click: very short, loud, mid-high
    assert!((60.0..=250.0).contains(&cursor.ms) && cursor.peak >= 10_000);
    assert!((1_000.0..=4_000.0).contains(&cursor.dominant_hz));
    // id 2: a bright chime, several times longer than the click
    assert!((200.0..=700.0).contains(&chime.ms) && chime.peak >= 8_000);
    assert!((500.0..=2_500.0).contains(&chime.dominant_hz));
    assert!(chime.ms > 2.0 * cursor.ms);
    // the buzzer: a low tone (spectral peak and centroid well under 500 Hz), a few hundred ms
    assert!((200.0..=700.0).contains(&buzzer.ms) && buzzer.peak >= 8_000);
    assert!(buzzer.dominant_hz <= 250.0 && buzzer.centroid_hz <= 400.0, "{buzzer:?}");
    assert!(buzzer.dominant_hz < cursor.dominant_hz / 4.0);
    // cancel: the shortest of the four and the highest
    assert!((30.0..=120.0).contains(&cancel.ms) && cancel.peak >= 8_000);
    assert!(cancel.dominant_hz >= 2_500.0);
    assert!(cancel.ms < cursor.ms);

    // FFNx's other anchors, as a sanity check of "id n is entry n - 1": the footstep (id 159) is a short
    // low thump that does not loop, the Tiny Bronco engine drone (id 493) loops
    let footstep = measure(159);
    assert!(footstep.ms < 400.0 && footstep.dominant_hz <= 250.0);
    let engine = bank.info(entry_for_sound_id(493).unwrap()).unwrap();
    assert!(engine.loop_region.is_some(), "id 493 must loop");
}

/// Scripts that play sounds, and how many of them also play a sound the bank marks as looping under each
/// reading of the ids (1-based: entry `id - 1`; 0-based: entry `id`).
#[derive(Default, Debug, Clone, Copy)]
struct LoopTally {
    scripts: usize,
    with_loop_one_based: usize,
    with_loop_zero_based: usize,
}

/// Sound ids the field scripts' `SOUND` opcode uses, over every field of `flevel.lgp`.
#[derive(Default)]
struct FieldSounds {
    fields: usize,
    instructions: usize,
    banked: usize,
    literal_ids: BTreeMap<i32, usize>,
    /// (key mask, sound id) -> count, for SOUNDs inside the block an IFKEY/IFKEYON/IFKEYOFF guards.
    guarded: BTreeMap<(u16, i32), usize>,
    /// Scripts that contain `SOUND 0`, and the others that play a sound.
    with_id_zero: LoopTally,
    without_id_zero: LoopTally,
}

fn survey_field_sounds(bank: &SfxBank) -> Option<FieldSounds> {
    let loops = |entry: i32| usize::try_from(entry).ok().and_then(|e| bank.info(e)).is_some_and(|i| i.loop_region.is_some());
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let mut lgp = fmv_ff7::lgp::LgpArchive::open_path(flevel_path(&dir)).unwrap();
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let mut out = FieldSounds::default();
    for name in names {
        let Ok(field) = load_field(&mut lgp, &name) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        out.fields += 1;
        let mut seen = BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
                let (mut zero, mut loop_one, mut loop_zero, mut any) = (false, false, false, false);
                for ins in d.instructions.iter().filter(|i| i.opcode == 0xF1) {
                    out.instructions += 1;
                    any = true;
                    match ins.literal("id") {
                        Some(id) => {
                            *out.literal_ids.entry(id).or_default() += 1;
                            zero |= id == 0;
                            loop_one |= id >= 1 && loops(id - 1);
                            loop_zero |= id >= 1 && loops(id);
                        }
                        None => out.banked += 1,
                    }
                }
                if any {
                    let tally = if zero { &mut out.with_id_zero } else { &mut out.without_id_zero };
                    tally.scripts += 1;
                    tally.with_loop_one_based += usize::from(loop_one);
                    tally.with_loop_zero_based += usize::from(loop_zero);
                }
                // SOUNDs inside the block a key test guards (the test falls through into the block)
                for key in d.instructions.iter().filter(|i| (0x30..=0x32).contains(&i.opcode)) {
                    let (Some(mask), Some(target)) = (key.literal("keys"), key.jump_target) else { continue };
                    for ins in d.instructions.iter().filter(|i| i.opcode == 0xF1 && i.offset > key.offset && i.offset < target) {
                        if let Some(id) = ins.literal("id") {
                            *out.guarded.entry((mask as u16, id)).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    Some(out)
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_sound_ids_in_field_scripts_are_one_based() {
    let Some(bank) = open_bank() else { return };
    let Some(sounds) = survey_field_sounds(&bank) else { return };
    let literal: usize = sounds.literal_ids.values().sum();
    let max_id = sounds.literal_ids.keys().next_back().copied().unwrap();
    let min_id = sounds.literal_ids.keys().next().copied().unwrap();
    println!(
        "{} fields, {} SOUND instructions ({} literal ids, {} banked), {} distinct literal ids from {min_id} to {max_id}; bank has {} entries",
        sounds.fields,
        sounds.instructions,
        literal,
        sounds.banked,
        sounds.literal_ids.len(),
        bank.len()
    );
    let zero = sounds.literal_ids.get(&0).copied().unwrap_or(0);
    let beyond: usize = sounds.literal_ids.range(751..).map(|(_, n)| n).sum();
    println!("literal id 0: {zero} uses; ids above 750: {beyond} uses");
    let mut by_use: Vec<(usize, i32)> = sounds.literal_ids.iter().map(|(id, n)| (*n, *id)).collect();
    by_use.sort_by(|a, b| b.cmp(a));
    println!("most used ids (uses, id): {:?}", &by_use[..by_use.len().min(12)]);
    let id_use = |id: i32| sounds.literal_ids.get(&id).copied().unwrap_or(0);
    println!("uses of the menu ids 1/2/3/4: {}/{}/{}/{}", id_use(1), id_use(2), id_use(3), id_use(4));

    // which reading of the ids ever lands on an empty slot? (compare only the ids the readings disagree on)
    let empty = |entry: i32| usize::try_from(entry).map_or(true, |e| bank.info(e).is_none());
    let (mut hits_one_based, mut hits_zero_based) = (BTreeMap::new(), BTreeMap::new());
    for (&id, &uses) in &sounds.literal_ids {
        if id >= 1 && empty(id - 1) {
            hits_one_based.insert(id, uses);
        }
        if empty(id) {
            hits_zero_based.insert(id, uses);
        }
    }
    println!("ids that land on an empty slot read 1-based (entry id - 1): {hits_one_based:?}");
    println!("ids that land on an empty slot read 0-based (entry id):     {hits_zero_based:?}");

    // id 0 is "stop the effect channel" (FFNx's play-on-channel: `if (id) play else stop`): scripts that
    // contain SOUND 0 should be the ones that start a looping sound, under the 1-based reading
    println!("scripts with SOUND 0:         {:?}", sounds.with_id_zero);
    println!("other scripts with a SOUND:   {:?}", sounds.without_id_zero);

    // evidence for the menu ids: which key mask guards the SOUNDs of ids 1 to 4 (masks: 0x20 OK, 0x40
    // cancel, 0x1000 up, 0x2000 right, 0x4000 down, 0x8000 left, by the FFRTT IFKEY page)
    for id in 1..=4 {
        let masks: Vec<(u16, usize)> = sounds.guarded.iter().filter(|((_, i), _)| *i == id).map(|((m, _), n)| (*m, *n)).collect();
        println!("SOUND {id} inside key-test blocks (mask, uses): {masks:?}");
    }

    // the facts of the owned data
    assert_eq!((sounds.fields, sounds.instructions, sounds.banked), (702, 4387, 0));
    assert_eq!((sounds.literal_ids.len(), min_id, max_id), (205, 0, 611));
    assert_eq!((zero, beyond), (197, 0));
    assert_eq!((id_use(1), id_use(2), id_use(3), id_use(4)), (81, 0, 62, 17));
    // the ids never leave the bank, and read 1-based (entry id - 1) none of them lands on an empty slot...
    assert!(hits_one_based.is_empty(), "{hits_one_based:?}");
    // ...whereas read 0-based four of them (28 uses) would play an empty slot
    assert_eq!(hits_zero_based, BTreeMap::from([(198, 22), (209, 1), (523, 4), (579, 1)]));
}
