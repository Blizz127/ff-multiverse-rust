//! FF7's sound from this machine's own copy of the game: which files exist, which track a field's
//! script names, and the menu sounds.
//!
//! Everything is read-only and stays on this machine (nothing of it is sent to a server or kept
//! anywhere else). [`tasks`] turns the copy into the closures the [`super::Worker`] runs; what goes
//! wrong on the way is one line through the logger and `None` (no sound), never an error.
//!
//! * **Files**: [`fmv_library::ff7::resolve_audio`] finds `music.idx`, the Ogg and WAV music, and the
//!   `audio.fmt` / `audio.dat` pair in the library set (or beside a direct `flevel.lgp`).
//! * **Which track a field plays**: [`fmv_ff7::music`] reads the field's script for `MUSIC`, `BMUSC`
//!   and `FMUSC` and maps the AKAO frame ids they name to track names through `music.idx`.
//! * **The menu sounds**: [`fmv_ff7::sound`] reads the four effects out of `audio.dat`.

use super::cues::Cue;
use super::director::AreaMusic;
use super::worker::Tasks;
use crate::ff7::Ff7Source;
use fmv_audio::{decode_music_file, Logger, SfxClip};
use fmv_ff7::music::{field_music_of, FieldMusic, MusicList, MusicUse};
use fmv_ff7::script::INIT_SCRIPT;
use fmv_ff7::sound::{self, PcmClip, SfxBank};
use fmv_ff7::Ff7Adapter;
use fmv_library::ff7::{resolve_audio, ResolvedAudio};
use std::sync::Arc;

/// The tasks of the FF7 soundtrack over this machine's copy, or `None` (after one line saying why)
/// when there is nothing to play.
pub fn tasks(source: &Ff7Source, adapter: Arc<Ff7Adapter>, log: &Logger) -> Option<Tasks> {
    let files = match resolve_audio(source) {
        Ok(files) => files,
        Err(error) => {
            log(&format!("cannot look for the sound files ({error}); playing without sound"));
            return None;
        }
    };
    if files.is_empty() {
        log("this library has no sound files, so the game plays without sound (to add them, import the install again: fmv-import import-pc with the same labels, or the launcher once with FMV_FF7_REIMPORT=1; the README says how)");
        return None;
    }
    let list = match files.music_idx.as_deref().map(std::fs::read) {
        Some(Ok(bytes)) => MusicList::parse(&bytes),
        Some(Err(error)) => {
            log(&format!("cannot read music.idx ({}); field music cannot be chosen", error.kind()));
            MusicList::default()
        }
        None => {
            log("the library has no music.idx: field music cannot be chosen (the battle theme and the fanfare still play)");
            MusicList::default()
        }
    };
    log(&format!(
        "sound files found: {} music tracks ({} ogg, {} wav), music.idx {} names, menu sounds {}",
        files.music_ogg.len() + files.music_wav.len(),
        files.music_ogg.len(),
        files.music_wav.len(),
        list.len(),
        if files.sfx_dat.is_some() && files.sfx_fmt.is_some() { "present" } else { "absent" },
    ));
    let files = Arc::new(files);
    let list = Arc::new(list);
    Some(Tasks {
        analyze: {
            let list = Arc::clone(&list);
            Arc::new(move |key| area_music(&adapter, &list, key))
        },
        decode: {
            let files = Arc::clone(&files);
            Arc::new(move |name| {
                let path = files.music_file(name).ok_or_else(|| "this library has no file for it".to_string())?;
                decode_music_file(path).map_err(|e| e.to_string())
            })
        },
        cues: {
            let files = Arc::clone(&files);
            Arc::new(move || load_cues(&files))
        },
    })
}

/// What the script of the field `key` says about music, as track names; an unreadable field says nothing.
fn area_music(adapter: &Ff7Adapter, list: &MusicList, key: &str) -> AreaMusic {
    let Ok(area) = adapter.load_field(key) else { return AreaMusic::default() };
    let Ok(music) = field_music_of(area.field_file()) else { return AreaMusic::default() };
    resolve(&music, list)
}

/// The frame id of the first use that sits in slot 0 of an entity, that is, in the entity's own init and
/// main script: the field's own setting. A use in another slot belongs to a talk, a touch or a line (the
/// music of a fight that script starts) and says nothing about the fights the field rolls by itself, so it
/// is not what every fight of the field gets.
fn field_wide(uses: &[MusicUse]) -> Option<u16> {
    uses.iter().find(|u| u.slot == INIT_SCRIPT).map(|u| u.frame_id)
}

/// A field's music commands as track names. Ids that name no track (id 1, past the list) are `None`: the
/// music then goes on, or the standard theme plays.
fn resolve(music: &FieldMusic, list: &MusicList) -> AreaMusic {
    let track = |id: Option<u16>| id.and_then(|id| list.track_for_frame_id(id)).map(str::to_string);
    AreaMusic {
        arrival: track(music.arrival()),
        battle: track(field_wide(&music.battle)),
        after_battle: track(field_wide(&music.after_battle)),
    }
}

/// The game's sound id of a menu cue (`fmv_ff7::sound` says how they were found; the confirm is the cursor's click).
fn sound_id(cue: Cue) -> u16 {
    match cue {
        Cue::Cursor => sound::MENU_CURSOR,
        Cue::Confirm => sound::MENU_CONFIRM,
        Cue::Cancel => sound::MENU_CANCEL,
        Cue::Buzzer => sound::MENU_BUZZER,
    }
}

/// A decoded effect as a mixer clip (`None` for more than two channels, which the game's files never have).
fn clip_of(pcm: &PcmClip) -> Option<SfxClip> {
    let samples: Vec<f32> = pcm.samples.iter().map(|&s| f32::from(s) / 32_768.0).collect();
    match pcm.channels {
        1 => SfxClip::from_mono(pcm.rate, &samples),
        2 => SfxClip::from_stereo(pcm.rate, samples),
        _ => None,
    }
}

/// The four menu sounds out of `audio.dat` (only their bytes are read, not the whole 70 MB file).
fn load_cues(files: &ResolvedAudio) -> Result<Vec<(Cue, SfxClip)>, String> {
    let (Some(fmt), Some(dat)) = (&files.sfx_fmt, &files.sfx_dat) else {
        return Err("the library has no audio.fmt and audio.dat".to_string());
    };
    let bank = SfxBank::open(fmt, dat).map_err(|e| e.to_string())?;
    let mut clips = Vec::with_capacity(Cue::ALL.len());
    for cue in Cue::ALL {
        let entry = sound::entry_for_sound_id(sound_id(cue)).ok_or_else(|| format!("no sound id for the {} cue", cue.name()))?;
        let pcm = bank.decode(entry).map_err(|e| format!("the {} sound: {e}", cue.name()))?;
        clips.push((cue, clip_of(&pcm).ok_or_else(|| format!("the {} sound has an unusable format", cue.name()))?));
    }
    Ok(clips)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A use of the music command at `slot`, naming AKAO frame `frame_id`.
    fn used(frame_id: u16, slot: usize) -> MusicUse {
        MusicUse { frame_id, block: 0, entity: 0, slot, phase: None, offset: 0, conditional: false }
    }

    /// An invented music list: line n (counted from 1) is `t<n>`, so that frame id n + 1 names `t<n>`.
    fn list() -> MusicList {
        MusicList::parse((1..=20).map(|n| format!("t{n}\\r\\n")).collect::<String>().as_bytes())
    }

    #[test]
    fn a_fields_commands_become_track_names_through_the_list() {
        let music = FieldMusic {
            music: vec![used(3, 0), used(5, 0)],
            battle: vec![used(9, 0)],
            after_battle: vec![used(3, 0)],
            skipped: 0,
        };
        let area = resolve(&music, &list());
        assert_eq!(area.arrival.as_deref(), Some("t2"));
        assert_eq!(area.battle.as_deref(), Some("t8"));
        assert_eq!(area.after_battle.as_deref(), Some("t2"));
        // A field that says nothing says nothing; an id with no line is no track (never a guess).
        assert_eq!(resolve(&FieldMusic::default(), &list()), AreaMusic::default());
        let odd = FieldMusic { music: vec![used(1, 0)], battle: vec![used(500, 0)], ..FieldMusic::default() };
        assert_eq!(resolve(&odd, &list()), AreaMusic::default());
        // Without music.idx nothing can be named, and the standard battle theme and the fanfare still play.
        assert_eq!(resolve(&music, &MusicList::default()), AreaMusic::default());
    }

    #[test]
    fn only_a_field_wide_setting_counts_for_the_fights_the_field_rolls() {
        // The music a talk script sets for the fight IT starts is not the field's battle music.
        let music = FieldMusic { battle: vec![used(9, 3), used(12, 0)], after_battle: vec![used(4, 2)], ..FieldMusic::default() };
        let area = resolve(&music, &list());
        assert_eq!(area.battle.as_deref(), Some("t11"), "the slot 0 use, not the first one in script order");
        assert_eq!(area.after_battle, None, "a use outside slot 0 only: the field's music again");
        let talk_only = FieldMusic { battle: vec![used(9, 3)], ..FieldMusic::default() };
        assert_eq!(resolve(&talk_only, &list()).battle, None, "the standard theme");
    }

    #[test]
    fn a_decoded_effect_becomes_a_mixer_clip_at_its_own_rate() {
        let mono = PcmClip { rate: 44_100, channels: 1, samples: vec![0, 16_384, -32_768, 32_767], loop_region: None };
        let clip = clip_of(&mono).expect("a mono clip");
        assert_eq!((clip.rate(), clip.frames()), (44_100, 4));
        let stereo = PcmClip { rate: 22_050, channels: 2, samples: vec![100, -100, 200, -200], loop_region: None };
        assert_eq!(clip_of(&stereo).map(|c| (c.rate(), c.frames())), Some((22_050, 2)));
        let surround = PcmClip { rate: 44_100, channels: 6, samples: vec![0; 12], loop_region: None };
        assert!(clip_of(&surround).is_none());
    }

    #[test]
    fn every_cue_has_a_distinct_sound_id_except_confirm_which_is_the_cursors_click() {
        assert_eq!(sound_id(Cue::Cursor), 1);
        assert_eq!(sound_id(Cue::Confirm), sound_id(Cue::Cursor));
        assert_ne!(sound_id(Cue::Cancel), sound_id(Cue::Buzzer));
        for cue in Cue::ALL {
            assert!(sound::entry_for_sound_id(sound_id(cue)).is_some(), "{cue:?}");
        }
    }

    #[test]
    fn a_library_without_the_effect_files_gives_no_cues_and_says_why() {
        let none = ResolvedAudio::default();
        assert!(load_cues(&none).unwrap_err().contains("no audio.fmt"));
        // Present but unreadable: a path that does not exist is an error text, never a panic.
        let missing = ResolvedAudio {
            sfx_fmt: Some(std::path::PathBuf::from("/definitely/not/here/audio.fmt")),
            sfx_dat: Some(std::path::PathBuf::from("/definitely/not/here/audio.dat")),
            ..ResolvedAudio::default()
        };
        assert!(load_cues(&missing).is_err());
    }
}
