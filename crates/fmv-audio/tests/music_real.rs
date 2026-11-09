//! Decoding the owner's REAL FF7 music files (Steam Edition, read-only).
//!
//! Every test is `#[ignore]` and returns early when `FMV_FF7_AUDIO_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_AUDIO_DATA=<.../data> cargo test --release -p fmv-audio --test music_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_AUDIO_DATA` is the game's `data/` directory (the one holding `music_ogg/` and `music/`).
//! The files are only opened for reading and nothing decoded is written anywhere; the assertions
//! record counts, rates and tag numbers, never audio.

use fmv_audio::{decode_music_file, LoopPoints, Pull};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("FMV_FF7_AUDIO_DATA") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => {
            eprintln!("FMV_FF7_AUDIO_DATA is not set; skipping");
            None
        }
    }
}

fn files_with(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("the music directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(extension)))
        .collect();
    found.sort();
    found
}

fn name(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[test]
#[ignore = "needs the owner's FF7 audio (FMV_FF7_AUDIO_DATA)"]
fn every_ogg_decodes_with_its_loop_tags_and_the_length_the_container_declares() {
    let Some(data) = data_dir() else { return };
    let oggs = files_with(&data.join("music_ogg"), "ogg");
    assert_eq!(oggs.len(), 94, "the Steam Edition's music_ogg holds 94 files");
    let (mut looping, mut whole, mut total_frames, mut bytes) = (0usize, 0usize, 0u64, 0usize);
    let (mut slowest, mut longest) = (("".to_string(), 0.0f64), ("".to_string(), 0.0f64));
    let started = Instant::now();
    for path in &oggs {
        let t = Instant::now();
        let music = decode_music_file(path).unwrap_or_else(|e| panic!("{}: {e}", name(path)));
        let took = t.elapsed().as_secs_f64();
        assert_eq!(music.rate(), 44_100, "{}", name(path));
        // The decode is exactly as long as the Ogg stream says it is: nothing lost, nothing added.
        assert_eq!(Some(music.frames()), music.declared_frames(), "{}", name(path));
        match music.loop_points() {
            Some(LoopPoints { start, end }) => {
                looping += 1;
                assert!(start < music.frames(), "{}", name(path));
                if let Some(end) = end {
                    assert!(end > start && end <= music.frames(), "{}", name(path));
                }
            }
            None => whole += 1,
        }
        total_frames += music.frames();
        bytes += music.bytes();
        if took > slowest.1 {
            slowest = (name(path), took);
        }
        if music.duration().as_secs_f64() > longest.1 {
            longest = (name(path), music.duration().as_secs_f64());
        }
    }
    println!(
        "{} ogg files decoded in {:.1} s: {} with loop tags, {} without; {} frames ({:.1} min) in all, {} MB as i16; slowest {} ({:.2} s), longest {} ({:.0} s)",
        oggs.len(),
        started.elapsed().as_secs_f64(),
        looping,
        whole,
        total_frames,
        total_frames as f64 / 44_100.0 / 60.0,
        bytes / 1_000_000,
        slowest.0,
        slowest.1,
        longest.0,
        longest.1
    );
    assert!(looping > 80, "most tracks loop: {looping}");
}

#[test]
#[ignore = "needs the owner's FF7 audio (FMV_FF7_AUDIO_DATA)"]
fn the_battle_theme_and_the_fanfare_have_the_tags_and_lengths_ffprobe_reads() {
    let Some(data) = data_dir() else { return };
    // ffprobe on the same files says: bat has LOOPSTART=192767 LOOPLENGTH=3087428 and lasts 7,350,665
    // frames (166.68 s); fanfare has no loop tags and lasts 5.20 s; fan2 has loop tags (182255,
    // 811877) and lasts 54.54 s. ffmpeg cuts the last block of an Ogg stream where its final granule
    // position says; this decoder keeps the whole block (bat: 439 frames, 10 ms, more), which only
    // ever lengthens a track's very end and never moves a sample or a loop point.
    let bat = decode_music_file(&data.join("music_ogg/bat.ogg")).expect("bat");
    assert_eq!(bat.loop_points(), Some(LoopPoints { start: 192_767, end: Some(192_767 + 3_087_428) }));
    assert!((7_350_665..7_350_665 + 2_048).contains(&bat.frames()), "{}", bat.frames());
    let fanfare = decode_music_file(&data.join("music_ogg/fanfare.ogg")).expect("fanfare");
    assert_eq!(fanfare.loop_points(), None, "the victory jingle is a one-shot");
    assert!((fanfare.duration().as_secs_f64() - 5.1978).abs() < 0.05, "{:?}", fanfare.duration());
    let fan2 = decode_music_file(&data.join("music_ogg/fan2.ogg")).expect("fan2");
    assert_eq!(fan2.loop_points(), Some(LoopPoints { start: 182_255, end: Some(182_255 + 811_877) }));
    assert!((fan2.duration().as_secs_f64() - 54.5377).abs() < 0.05, "{:?}", fan2.duration());
}

#[test]
#[ignore = "needs the owner's FF7 audio (FMV_FF7_AUDIO_DATA)"]
fn the_four_wav_files_decode_as_adpcm_stereo() {
    let Some(data) = data_dir() else { return };
    let wavs = files_with(&data.join("music"), "wav");
    assert_eq!(wavs.len(), 4);
    for path in &wavs {
        let music = decode_music_file(path).unwrap_or_else(|e| panic!("{}: {e}", name(path)));
        assert_eq!(music.rate(), 44_100, "{}", name(path));
        assert!(music.loop_points().is_none(), "a WAV has no tags: {}", name(path));
        println!("{}: {} frames, container declares {:?}", name(path), music.frames(), music.declared_frames());
        // ffprobe's sample counts, which come from each file's `fact` chunk: the ADPCM decoder returns
        // whole blocks (up to 924 frames more), and the music ends where the chunk says.
        let expected = match name(path).as_str() {
            "heart" => 60_847,
            "sato" => 500_765,
            "sensui" => 49_976,
            "wind" => 1_353_760,
            other => panic!("an unexpected wav: {other}"),
        };
        assert_eq!(music.frames(), expected, "{}", name(path));
    }
}

/// The ground truth for the decode itself: ffmpeg's own Vorbis decoder (read-only on the same file,
/// raw samples through a pipe, nothing written to disk) must agree with this crate's, sample for
/// sample to within rounding, from the very first frame (so that the loop tags, which count samples
/// of the canonical decode, point at the same audio here). Skipped when ffmpeg is not installed.
#[test]
#[ignore = "needs the owner's FF7 audio (FMV_FF7_AUDIO_DATA) and ffmpeg"]
fn the_decode_agrees_with_ffmpegs_from_the_first_sample() {
    let Some(data) = data_dir() else { return };
    for track in ["bat", "fanfare", "oa"] {
        let path = data.join(format!("music_ogg/{track}.ogg"));
        let music = decode_music_file(&path).expect("a track");
        let reference = match std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-t", "20", "-f", "s16le", "-acodec", "pcm_s16le", "-ac", "2", "-ar", "44100", "-"])
            .output()
        {
            Ok(done) if done.status.success() => done.stdout,
            _ => {
                eprintln!("no ffmpeg here; skipping the comparison");
                return;
            }
        };
        let reference: Vec<i16> = reference.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
        let frames = (reference.len() / 2).min(music.frames() as usize);
        assert!(frames > 40_000, "{track}: only {frames} frames to compare");
        let mut source = music.source();
        let mut ours = vec![0.0f32; frames * 2];
        assert_eq!(source.read(&mut ours), Pull::Frames(frames));
        let mut worst = 0i32;
        for (i, (&mine, &theirs)) in ours.iter().zip(&reference).enumerate() {
            let mine = (mine * 32_768.0).round() as i32;
            worst = worst.max((mine - i32::from(theirs)).abs());
            assert!((mine - i32::from(theirs)).abs() <= 4, "{track}: sample {i} (frame {}) differs: {mine} vs {theirs}", i / 2);
        }
        println!("{track}: {frames} frames compared with ffmpeg, worst difference {worst}");
    }
}
