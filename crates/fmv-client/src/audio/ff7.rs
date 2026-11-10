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
//! * **The sounds of a battle**: the clips of the sound ids a fight can need (its weapons, its enemies' attacks), read
//!   the same way, by id, when the fight starts (see `crate::battle::sounds` for which ids and why).
//! * **The sounds of a field**: what the field's own scripts play for the player's story ([`fmv_ff7::field_sound`]:
//!   the ambient loops, the sounds of its ladders and jumps, the lift and the doors), and the clips that names, read by
//!   id with the loop region `audio.fmt` gives them (a loop clip loops where the file says).

use super::cues::Cue;
use super::director::AreaMusic;
use super::field::{AmbientLoop, ExitPlan, FieldPlan, FieldRead, FlipPlan, LinkPlan, PlannedCall};
use super::worker::{AnalyzeTask, FieldTask, SoundsRead, Tasks, VoiceClip};
use crate::ff7::{resolve_flevel, Ff7ResolveError, Ff7Source};
use crate::mods::VoiceCue;
use fmv_audio::{decode_music_bytes, decode_music_file, DecodedMusic, Logger, Pull, SfxClip};
use fmv_mods::Mods;
use fmv_ff7::field_sound::{self, Timed};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::music::{field_music_of, FieldMusic, MusicList, MusicUse};
use fmv_ff7::script::{FieldScript, Maplist, INIT_SCRIPT};
use fmv_ff7::sound::{self, PcmClip, SfxBank, SoundError};
use fmv_ff7::story::{StoryState, TRACKED_VARS};
use fmv_ff7::Ff7Adapter;
use fmv_library::ff7::{resolve_audio, ResolvedAudio};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// The tasks of the FF7 soundtrack over this machine's copy, or `None` (after one line saying why)
/// when there is nothing to play. `mods` is the optional mod layers (`None`: the base game's sound): Echo-S's music and voices and
/// Cosmo Memory's sounds play where they have something, and the original plays everywhere else.
pub fn tasks(source: &Ff7Source, adapter: Arc<Ff7Adapter>, log: &Logger, mods: Option<Arc<Mods>>) -> Option<Tasks> {
    // The archive's name list (the doors' targets) is read when the first field is, on the worker's thread.
    let flevel = resolve_flevel(source).ok().map(|found| found.path);
    let for_music = Arc::clone(&adapter);
    let layer = SoundLayer::new(mods, log.clone());
    let for_field = layer.clone();
    tasks_with(
        resolve_audio(source),
        log,
        layer,
        move |list| Arc::new(move |key| area_music(&for_music, &list, key)),
        move |files| {
            let maplist: OnceLock<Option<Maplist>> = OnceLock::new();
            Arc::new(move |key, story, have| {
                let maplist = maplist.get_or_init(|| flevel.as_ref().and_then(read_maplist));
                read_field(&adapter, &files, maplist.as_ref(), key, story, have, &for_field)
            })
        },
    )
}

/// The archive's `maplist` (read-only), if it can be read.
fn read_maplist(path: &PathBuf) -> Option<Maplist> {
    let mut archive = LgpArchive::open_path(path).ok()?;
    Maplist::from_archive(&mut archive).ok()
}

/// What the mod layers add to the sound: Echo-S's music in place of the original track, Cosmo Memory's clip in place of the original
/// clip of a sound id, and the voices. Cheap to clone (it shares one note per sound id between the threads that read them).
#[derive(Clone)]
struct SoundLayer {
    mods: Option<Arc<Mods>>,
    log: Logger,
    /// The sound ids already said to come from a mod (once each per run: a battle asks for the same ids again and again).
    said: Arc<Mutex<HashSet<u16>>>,
    /// Field ambiences decoded lately, by clip name, the newest first (a story step reads its field again, and neighbouring
    /// fields share a clip: `md1stin`, `md1_1` and `md8_1` play the same one), at most [`AMBIENCE_CACHE`].
    ambiences: Arc<Mutex<VecDeque<(String, SfxClip)>>>,
    /// How many ambience clips were decoded (tests).
    decoded: Arc<std::sync::atomic::AtomicUsize>,
}

/// The most clips of one sound id that are played in turn (Cosmo names up to 7 for the sounds of a fight).
const MAX_VARIANTS: usize = 8;

/// How many decoded field ambiences [`SoundLayer`] keeps (a long one is tens of MB of samples).
const AMBIENCE_CACHE: usize = 3;

impl SoundLayer {
    fn new(mods: Option<Arc<Mods>>, log: Logger) -> Self {
        Self {
            mods,
            log,
            said: Arc::new(Mutex::new(HashSet::new())),
            ambiences: Arc::new(Mutex::new(VecDeque::new())),
            decoded: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// No layer: the original sound (what the tests use).
    #[cfg(test)]
    fn off(log: Logger) -> Self {
        Self::new(None, log)
    }

    /// The mixer clip that plays in place of sound id `id` (heard in the field `field`, when that is known), if the sound layer has
    /// clips for it and the first one decodes. A repeating sound plays that first clip each time (the layer's `sequential`/`shuffle`
    /// order is not followed: a known gap). No loop region is given: the original id's loop offsets are bytes of the original's PCM
    /// and mean nothing for another file, and a loop plays a clip without a region whole, over and over.
    /// The first of [`SoundLayer::replacements`] (the menu sounds, which play one clip each).
    fn replacement(&self, id: u16, field: Option<&str>) -> Option<SfxClip> {
        self.replacements(id, field).and_then(|clips| clips.into_iter().next())
    }

    /// The mixer clips that play in place of sound id `id`, in the order the layer's section names them (at most
    /// [`MAX_VARIANTS`]): the first decodes or none plays (the original does), a later one that does not decode is left out.
    fn replacements(&self, id: u16, field: Option<&str>) -> Option<Vec<SfxClip>> {
        let set = self.mods.as_ref()?.sfx(id, field)?;
        let source = set.clips.first()?.source;
        let mut clips = Vec::new();
        for (i, found) in set.clips.into_iter().take(MAX_VARIANTS).enumerate() {
            let clip = decode_music_bytes(found.bytes, extension_of(&found.name))
                .map_err(|e| e.to_string())
                .and_then(|music| pcm_of(&music).ok_or_else(|| "nothing to play".to_string()))
                .and_then(|(rate, samples)| SfxClip::from_stereo(rate, samples).ok_or_else(|| "an unusable format".to_string()));
            match clip {
                Ok(clip) => clips.push(clip),
                Err(e) if i == 0 => {
                    (self.log)(&format!("sound {id}: {} does not decode ({e}); the original plays", found.name));
                    return None;
                }
                Err(_) => {}
            }
        }
        if self.said.lock().unwrap_or_else(PoisonError::into_inner).insert(id) {
            (self.log)(&format!("sound {id} from {source}"));
        }
        Some(clips)
    }

    /// The ambience the sound layer has for the field with maplist id `field_id`, decoded as a mixer clip that loops whole, or
    /// `None` (no layer, no section, or the clip does not decode: said once, and the field plays without it).
    fn ambience(&self, field_id: u16) -> Option<super::field::ModAmbience> {
        self.decoded_ambience(self.mods.as_ref()?.field_ambience(field_id)?)
    }

    /// The ambience the sound layer has for a battle of formation `formation`, decoded as [`SoundLayer::ambience`] does.
    fn battle_ambience(&self, formation: u16) -> Option<super::field::ModAmbience> {
        self.decoded_ambience(self.mods.as_ref()?.battle_ambience(formation)?)
    }

    /// An ambience of the layer as a mixer clip that loops whole (the decoded clips are kept, see [`SoundLayer::ambiences`]).
    fn decoded_ambience(&self, found: fmv_mods::Ambience) -> Option<super::field::ModAmbience> {
        let cached = {
            let mut cache = self.ambiences.lock().unwrap_or_else(PoisonError::into_inner);
            let hit = cache.iter().position(|(name, _)| *name == found.clip.name);
            hit.and_then(|i| cache.remove(i)).map(|entry| {
                let clip = entry.1.clone();
                cache.push_front(entry);
                clip
            })
        };
        let clip = match cached {
            Some(clip) => clip,
            None => {
                let music = match decode_music_bytes(found.clip.bytes, extension_of(&found.clip.name)) {
                    Ok(music) => music,
                    Err(e) => {
                        (self.log)(&format!("ambience {}: {} does not decode ({e}); the field plays without it", found.section, found.clip.name));
                        return None;
                    }
                };
                let (rate, samples) = pcm_of(&music)?;
                let clip = SfxClip::from_stereo(rate, samples)?;
                self.decoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut cache = self.ambiences.lock().unwrap_or_else(PoisonError::into_inner);
                cache.push_front((found.clip.name.clone(), clip.clone()));
                cache.truncate(AMBIENCE_CACHE);
                clip
            }
        };
        let seconds = |s: f32| std::time::Duration::from_secs_f32(if s.is_finite() { s.clamp(0.0, 60.0) } else { 0.5 });
        Some(super::field::ModAmbience {
            section: found.section,
            name: found.clip.name,
            source: found.clip.source,
            clip,
            fade_in: seconds(found.fade_in),
            fade_out: seconds(found.fade_out),
        })
    }

    /// Echo-S's track `name` decoded, if the music layer has it and it decodes (else the original plays).
    fn track(&self, name: &str) -> Option<DecodedMusic> {
        let track = self.mods.as_ref()?.music(name)?;
        match decode_music_bytes(track.bytes, extension_of(&track.name)) {
            Ok(music) => {
                (self.log)(&format!("music {name} from {}", track.source));
                Some(music)
            }
            Err(e) => {
                (self.log)(&format!("music {name}: {} does not decode ({e}); the original plays", track.name));
                None
            }
        }
    }

    /// The task of the voice layer: the clip a cue asks for, decoded, as a mixer clip that ends in a silent loop region (so that its
    /// loop handle can stop it, see [`super::Live`]), with the length of the voice itself; `Ok(None)` when the layer has none.
    fn voice(&self, cue: &VoiceCue) -> Result<Option<VoiceClip>, String> {
        let Some(mods) = self.mods.as_ref() else { return Ok(None) };
        let found = match cue {
            VoiceCue::Say { field, window, dialog, page } => mods.voice(field, *window, *dialog, *page),
            VoiceCue::Choice { field, dialog, line } => mods.option_voice(field, *dialog, *line),
            VoiceCue::Stop => None,
        };
        let Some(found) = found else { return Ok(None) };
        let music = decode_music_bytes(found.bytes, extension_of(&found.name)).map_err(|e| format!("{}: {e}", found.name))?;
        let (rate, mut samples) = pcm_of(&music).ok_or_else(|| format!("{}: nothing to play", found.name))?;
        let frames = samples.len() / 2;
        let tail = (rate / 10).max(1) as usize;
        samples.extend(std::iter::repeat_n(0.0f32, tail * 2));
        let clip = SfxClip::from_stereo(rate, samples)
            .ok_or_else(|| format!("{}: unusable audio", found.name))?
            .with_loop(frames as u64, (frames + tail) as u64);
        Ok(Some(VoiceClip { name: found.name, clip, length: Duration::from_secs_f64(frames as f64 / f64::from(rate)) }))
    }
}

/// The file extension of an archive path (a hint to the decoder).
fn extension_of(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, ext)| ext)
}

/// All the frames of a decoded Ogg as interleaved stereo `f32` and its rate (`None` for a track with no frame).
fn pcm_of(music: &DecodedMusic) -> Option<(u32, Vec<f32>)> {
    let frames = usize::try_from(music.frames()).ok()?;
    let mut samples: Vec<f32> = Vec::with_capacity(frames.checked_mul(2)?);
    let mut source = music.source();
    let mut buf = vec![0.0f32; 8_192];
    // (`Pending` never comes from a source that is all in memory; it ends the read like `End` rather than spin.)
    while let Pull::Frames(n) = source.read(&mut buf) {
        samples.extend_from_slice(buf.get(..n.checked_mul(2)?)?);
    }
    (!samples.is_empty()).then_some((music.rate(), samples))
}

/// [`tasks`] over what [`resolve_audio`] said, with `analyze` building the task that reads a field's script
/// from the track list and `field` the one that reads what a field's scripts play from the sound files (the two
/// parts that need the adapter, so that everything else is testable without one). The original sound only.
#[cfg(test)]
fn tasks_from(
    resolved: Result<ResolvedAudio, Ff7ResolveError>,
    log: &Logger,
    analyze: impl FnOnce(Arc<MusicList>) -> AnalyzeTask,
    field: impl FnOnce(Arc<ResolvedAudio>) -> FieldTask,
) -> Option<Tasks> {
    tasks_with(resolved, log, SoundLayer::off(log.clone()), analyze, field)
}

/// [`tasks_from`] with the mod layers in `layer`.
fn tasks_with(
    resolved: Result<ResolvedAudio, Ff7ResolveError>,
    log: &Logger,
    layer: SoundLayer,
    analyze: impl FnOnce(Arc<MusicList>) -> AnalyzeTask,
    field: impl FnOnce(Arc<ResolvedAudio>) -> FieldTask,
) -> Option<Tasks> {
    // Echo-S voices and named music tracks can be decoded without the original audio files.
    // Keep the original-only behavior when those layers are off or the archive is absent.
    let mod_audio = layer.mods.as_ref().is_some_and(|mods| mods.has_echo() && (mods.layers().voice || mods.layers().music));
    let files = match resolved {
        Ok(files) => files,
        Err(error) if mod_audio => {
            log(&format!("cannot look for the original sound files ({error}); using available Echo-S audio"));
            ResolvedAudio::default()
        }
        Err(error) => {
            log(&format!("cannot look for the sound files ({error}); playing without sound"));
            return None;
        }
    };
    if files.is_empty() {
        if !mod_audio {
            log("this library has no sound files, so the game plays without sound (to add them, import the install again: fmv-import import-pc with the same labels, or the launcher once with FMV_FF7_REIMPORT=1; the README says how)");
            return None;
        }
        log("this library has no original sound files; using available Echo-S audio (field music needs music.idx, and original effects are unavailable)");
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
        analyze: analyze(Arc::clone(&list)),
        field: field(Arc::clone(&files)),
        decode: {
            let (files, layer) = (Arc::clone(&files), layer.clone());
            Arc::new(move |name| {
                // Echo-S's track of that name first (the music layer); the original is what plays without it.
                if let Some(music) = layer.track(name) {
                    return Ok(music);
                }
                let path = files.music_file(name).ok_or_else(|| "this library has no file for it".to_string())?;
                decode_music_file(path).map_err(|e| e.to_string())
            })
        },
        cues: {
            let (files, layer) = (Arc::clone(&files), layer.clone());
            Arc::new(move || load_cues_with(&files, &layer))
        },
        sounds: {
            let (files, layer) = (Arc::clone(&files), layer.clone());
            Arc::new(move |ids| load_sounds_with(&files, ids, &layer, None))
        },
        voice: {
            let layer = layer.clone();
            Arc::new(move |cue| layer.voice(cue))
        },
        battle_ambience: {
            let layer = layer.clone();
            Some(Arc::new(move |formation| layer.battle_ambience(formation)))
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

/// A decoded effect as a mixer clip, with the loop region `audio.fmt` gives it (a clip that loops plays its intro and
/// then that region over and over when it is played as a loop; as a one-shot it plays through once). `None` for more than
/// two channels, which the game's files never have.
fn clip_of(pcm: &PcmClip) -> Option<SfxClip> {
    let samples: Vec<f32> = pcm.samples.iter().map(|&s| f32::from(s) / 32_768.0).collect();
    let clip = match pcm.channels {
        1 => SfxClip::from_mono(pcm.rate, &samples),
        2 => SfxClip::from_stereo(pcm.rate, samples),
        _ => None,
    }?;
    Some(match pcm.loop_region {
        Some(region) => clip.with_loop(region.start, region.end),
        None => clip,
    })
}

/// The four menu sounds out of `audio.dat` (only their bytes are read, not the whole 70 MB file): the original's.
#[cfg(test)]
fn load_cues(files: &ResolvedAudio) -> Result<Vec<(Cue, SfxClip)>, String> {
    load_cues_with(files, &SoundLayer::off(Arc::new(|_| {})))
}

/// [`load_cues`] with the sound layer's clip for a cue's sound id where it has one.
fn load_cues_with(files: &ResolvedAudio, layer: &SoundLayer) -> Result<Vec<(Cue, SfxClip)>, String> {
    let (Some(fmt), Some(dat)) = (&files.sfx_fmt, &files.sfx_dat) else {
        return Err("the library has no audio.fmt and audio.dat".to_string());
    };
    let bank = SfxBank::open(fmt, dat).map_err(|e| e.to_string())?;
    let mut clips = Vec::with_capacity(Cue::ALL.len());
    for cue in Cue::ALL {
        if let Some(clip) = layer.replacement(sound_id(cue), None) {
            clips.push((cue, clip));
            continue;
        }
        let entry = sound::entry_for_sound_id(sound_id(cue)).ok_or_else(|| format!("no sound id for the {} cue", cue.name()))?;
        let pcm = bank.decode(entry).map_err(|e| format!("the {} sound: {e}", cue.name()))?;
        clips.push((cue, clip_of(&pcm).ok_or_else(|| format!("the {} sound has an unusable format", cue.name()))?));
    }
    Ok(clips)
}

/// The clips of the sound ids `ids` out of `audio.dat` (only their bytes are read, not the whole 70 MB file): each id that
/// names a clip comes back as a mixer clip, each that does not (an empty slot, an id past the bank, a clip that does not
/// decode) with a short reason. Never an error for the whole list: a battle plays what it can. The original's clips.
#[cfg(test)]
fn load_sounds(files: &ResolvedAudio, ids: &[u16]) -> SoundsRead {
    load_sounds_with(files, ids, &SoundLayer::off(Arc::new(|_| {})), None)
}

/// [`load_sounds`] with the sound layer's clip for an id where it has one (heard in `field`, when that is known: a field's own
/// sounds are asked for with it).
fn load_sounds_with(files: &ResolvedAudio, ids: &[u16], layer: &SoundLayer, field: Option<&str>) -> SoundsRead {
    let missing_all = |why: String| SoundsRead { missing: ids.iter().map(|id| (*id, why.clone())).collect(), ..SoundsRead::default() };
    let (Some(fmt), Some(dat)) = (&files.sfx_fmt, &files.sfx_dat) else {
        return missing_all("the library has no audio.fmt and audio.dat".to_string());
    };
    let bank = match SfxBank::open(fmt, dat) {
        Ok(bank) => bank,
        Err(e) => return missing_all(e.to_string()),
    };
    let mut read = SoundsRead::default();
    for &id in ids {
        if let Some(mut clips) = layer.replacements(id, field) {
            if clips.len() > 1 {
                read.variants.push((id, clips.clone()));
            }
            read.clips.push((id, clips.swap_remove(0)));
            continue;
        }
        match bank.decode_sound_id(id) {
            Ok(pcm) => match clip_of(&pcm) {
                Some(clip) => read.clips.push((id, clip)),
                None => read.missing.push((id, "an unusable format".to_string())),
            },
            Err(SoundError::EmptyEntry(_)) => read.missing.push((id, "an empty slot".to_string())),
            Err(e) => read.missing.push((id, e.to_string())),
        }
    }
    read
}

/// `story` with every bit the story model tracks turned on (or off): the story a script sees right after it sets (or clears) a
/// tracked bit, whatever else the story says.
fn with_tracked_bits(story: &StoryState, on: bool) -> StoryState {
    let mut state = story.clone();
    for var in TRACKED_VARS {
        for bit in (0..8u8).filter(|bit| 1u8.checked_shl(u32::from(*bit)).is_some_and(|mask| var.bits & mask != 0)) {
            state.set_bit(var.bank, var.address, bit, on);
        }
    }
    state
}

/// The calls of a run's events, as the plan keeps them.
fn calls_of(events: &[Timed]) -> Vec<PlannedCall> {
    events.iter().map(|e| PlannedCall { at: e.at, channel: e.call.channel, id: e.call.id, pan: e.call.pan }).collect()
}

/// What the scripts of the field `key` play for the progression `blob`, and the clips that names whose ids are not in `have`.
/// Everything the plan says comes from the field's own scripts ([`fmv_ff7::field_sound`]); the clips are read by id out of
/// `audio.dat` (only those). An error says why there is no plan (and so no sound): the progression is not one this
/// adapter reads, the library has no sound files, the field cannot be read.
fn read_field(
    adapter: &Ff7Adapter,
    files: &ResolvedAudio,
    maplist: Option<&Maplist>,
    key: &str,
    blob: &[u8],
    have: &[u16],
    layer: &SoundLayer,
) -> Result<FieldRead, String> {
    let story = StoryState::decode(blob).ok_or_else(|| "the progression is not one this adapter reads".to_string())?;
    let (Some(fmt), Some(dat)) = (&files.sfx_fmt, &files.sfx_dat) else { return Err("the library has no audio.fmt and audio.dat".to_string()) };
    let bank = SfxBank::open(fmt, dat).map_err(|e| e.to_string())?;
    let is_loop = |id: u16| bank.info_for_sound_id(id).is_some_and(|i| i.loop_region.is_some());
    let area = adapter.load_field(key).map_err(|e| e.to_string())?;
    let script = FieldScript::from_field(area.field_file()).map_err(|e| e.to_string())?;
    // What the field plays by itself for this story.
    let start = field_sound::start_up(&script, &story);
    let ambient: Vec<AmbientLoop> = start.ambient(is_loop).into_iter().map(|a| AmbientLoop { channel: a.channel, id: a.id, pan: a.pan, at: a.at }).collect();
    let mut unjudged = start.runs.iter().filter(|r| matches!(r.stop, field_sound::Stop::Unjudged { .. })).count();
    let unjudged_in = |runs: &[field_sound::RunInfo]| runs.iter().filter(|r| matches!(r.stop, field_sound::Stop::Unjudged { .. })).count();
    // What its ladders, jumps, lift and doors play (the lift's sounds are those that follow the story bit the lift sets or clears,
    // judged for the story right after that).
    let (mut links, mut withheld_links) = (Vec::new(), 0);
    for link in field_sound::link_events(&script, area.walkmesh(), &story) {
        unjudged += unjudged_in(&link.runs);
        withheld_links += usize::from(link.withheld);
        // (a link that plays nothing in time is not in the plan)
        if !link.events.is_empty() {
            links.push(LinkPlan { line: link.line, to: link.to, needs_ok: link.needs_ok, calls: calls_of(&link.events) });
        }
    }
    let mut flips = Vec::new();
    for on in [true, false] {
        for flip in field_sound::flip_events(&script, &with_tracked_bits(&story, on)).into_iter().filter(|f| f.on == on) {
            unjudged += unjudged_in(&flip.runs);
            flips.push(FlipPlan { bank: flip.bank, address: flip.address, bit: flip.bit, on, calls: calls_of(&flip.events) });
        }
    }
    let mut exits = Vec::new();
    for exit in maplist.map(|m| field_sound::exit_events(&script, m, &story)).unwrap_or_default() {
        unjudged += unjudged_in(&exit.runs);
        exits.push(ExitPlan { target: exit.target, arrival: exit.arrival, line: exit.line, calls: calls_of(&exit.events) });
    }
    let plan = FieldPlan { key: key.to_string(), story: blob.to_vec(), ambient, links, flips, exits, unjudged, left_open: start.ambient_left_open(is_loop), withheld_links };
    // The clips the plan names, minus the ones the caller holds.
    let wanted: Vec<u16> = plan.ids().into_iter().filter(|id| !have.contains(id)).collect::<BTreeSet<_>>().into_iter().collect();
    let clips = load_sounds_with(files, &wanted, layer, Some(key));
    // The sound layer's ambience of the field, by the field's number in the archive's name list (what FFNx names it by).
    let ambience = maplist.and_then(|m| m.id_of(key)).and_then(|id| layer.ambience(id)).map(Box::new);
    Ok(FieldRead { plan, clips, ambience })
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
        MusicList::parse((1..=20).map(|n| format!("t{n}\r\n")).collect::<String>().as_bytes())
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
    fn a_clip_that_the_file_says_loops_keeps_its_loop_region_and_one_that_does_not_has_none() {
        // (a loop region in frames, as `fmv_ff7::sound` gives it: the game's byte offsets divided by channels * 2)
        let looping = PcmClip { rate: 44_100, channels: 1, samples: vec![0; 100], loop_region: Some(fmv_ff7::sound::LoopRegion { start: 20, end: 90 }) };
        assert_eq!(clip_of(&looping).and_then(|c| c.loop_region()), Some((20, 90)));
        let stereo = PcmClip { rate: 44_100, channels: 2, samples: vec![0; 200], loop_region: Some(fmv_ff7::sound::LoopRegion { start: 0, end: 100 }) };
        assert_eq!(clip_of(&stereo).and_then(|c| c.loop_region()), Some((0, 100)));
        let once = PcmClip { rate: 44_100, channels: 1, samples: vec![0; 100], loop_region: None };
        assert_eq!(clip_of(&once).and_then(|c| c.loop_region()), None);
    }

    #[test]
    fn the_story_a_script_sees_after_it_sets_or_clears_a_tracked_bit_has_every_tracked_bit_so() {
        let before = StoryState::at(11);
        let on = with_tracked_bits(&before, true);
        assert_eq!((on.moment, on.known_bit(1, 225, 0)), (11, Some(true)), "the lift bit on, the moment as it was");
        assert_eq!(with_tracked_bits(&on, false), before, "and off again is the story it was");
        assert_eq!(before.known_bit(1, 225, 0), Some(false));
    }

    #[test]
    fn the_calls_of_a_run_keep_their_frame_channel_id_and_pan() {
        use fmv_ff7::field_sound::{Call, Source};
        use fmv_ff7::script::ScriptRef;
        let event = |at, channel, id, pan| Timed { at, call: Call { channel, id, pan, source: Source::Akao }, by: ScriptRef { entity: 0, slot: 0 }, offset: 0, run: 0 };
        let calls = calls_of(&[event(0, 3, 38, 64), event(15, 1, 62, 0)]);
        assert_eq!(calls, [PlannedCall { at: 0, channel: 3, id: 38, pan: 64 }, PlannedCall { at: 15, channel: 1, id: 62, pan: 0 }]);
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

    fn lines() -> (Logger, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let log: Logger = Arc::new(move |line| sink.lock().expect("log").push(line.to_string()));
        (log, seen)
    }

    fn no_script(_: Arc<MusicList>) -> AnalyzeTask {
        Arc::new(|_| AreaMusic::default())
    }

    fn no_field(_: Arc<ResolvedAudio>) -> FieldTask {
        Arc::new(|key, _, _| Err(format!("no field {key}")))
    }

    /// A 16-bit mono PCM WAV file of `frames` frames (an invented one, no game data).
    fn wav(frames: usize) -> Vec<u8> {
        let data = u32::try_from(frames * 2).expect("small");
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[1, 0, 1, 0]); // PCM, mono
        out.extend_from_slice(&22_050u32.to_le_bytes());
        out.extend_from_slice(&44_100u32.to_le_bytes());
        out.extend_from_slice(&[2, 0, 16, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        out.extend((0..frames).flat_map(|i| (i as i16).to_le_bytes()));
        out
    }

    #[test]
    fn a_library_without_sound_is_no_tasks_and_exactly_one_line() {
        let (log, seen) = lines();
        assert!(tasks_from(Ok(ResolvedAudio::default()), &log, no_script, no_field).is_none());
        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].contains("no sound files") && seen[0].contains("FMV_FF7_REIMPORT=1"), "{}", seen[0]);
    }

    #[test]
    fn echo_voice_and_named_music_work_without_original_audio_files() {
        let mut spec = EchoSpec::default();
        spec.blobs.push(("VO\\Voice\\md1stin\\4a.wav", synthetic_wav(8_000, &[500; 800])));
        spec.blobs.push(("EchoeS Of The Planet\\vgmstream\\bat.wav", synthetic_wav(8_000, &[500; 800])));
        let (_tmp, layer, _) = layer(&spec, None, Layers::ALL);
        let (log, seen) = lines();
        let tasks = tasks_with(Ok(ResolvedAudio::default()), &log, layer.clone(), no_script, no_field).expect("Echo-S has audio");
        let cue = VoiceCue::Say { field: "md1stin".into(), window: 0, dialog: 4, page: 0 };
        assert_eq!((tasks.voice)(&cue).unwrap().unwrap().length, Duration::from_millis(100));
        assert_eq!((tasks.decode)("bat").unwrap().frames(), 800);
        assert!((tasks.cues)().is_err(), "missing original effects are not claimed to play");
        assert!(seen.lock().unwrap().iter().any(|s| s.contains("field music needs music.idx")));
        let no_audible_mods = SoundLayer::new(layer.mods.as_ref().map(|m| Arc::new(m.with_layers(Layers { voice: false, music: false, ..Layers::ALL }))), log.clone());
        let error = Ff7ResolveError::NoLibrary { dir: "/nowhere".into() };
        let tasks = tasks_with(Err(error), &log, layer, no_script, no_field).expect("Echo-S survives a resolver error");
        assert!((tasks.voice)(&cue).unwrap().is_some());
        assert!(tasks_with(Ok(ResolvedAudio::default()), &log, no_audible_mods, no_script, no_field).is_none());
    }

    #[test]
    fn a_library_that_cannot_be_read_is_no_tasks_and_exactly_one_line() {
        let (log, seen) = lines();
        let error = Ff7ResolveError::NoLibrary { dir: "/nowhere".into() };
        assert!(tasks_from(Err(error), &log, no_script, no_field).is_none());
        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].starts_with("cannot look for the sound files"), "{}", seen[0]);
    }

    #[test]
    fn tracks_without_a_list_still_decode_and_a_track_that_is_missing_says_so() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("bat.wav"), wav(500)).expect("a track");
        let files = ResolvedAudio { music_wav: [("bat".to_string(), dir.path().join("bat.wav"))].into(), ..ResolvedAudio::default() };
        let (log, seen) = lines();
        let tasks = tasks_from(Ok(files), &log, no_script, no_field).expect("a track is something to play");
        assert_eq!((tasks.analyze)("md1stin"), AreaMusic::default());
        assert_eq!((tasks.field)("md1stin", &[], &[]).err().as_deref(), Some("no field md1stin"), "the field task is the one that was given");
        // The track decodes through its file (a blob has no extension: the content says what it is).
        assert_eq!((tasks.decode)("bat").expect("bat decodes").frames(), 500);
        assert!((tasks.decode)("Bat").is_ok(), "names match without regard to case");
        assert!((tasks.decode)("fanfare").unwrap_err().contains("no file"));
        // No effect files: no menu sounds, with a reason, and none of a battle either.
        assert!((tasks.cues)().unwrap_err().contains("no audio.fmt"));
        assert_eq!((tasks.sounds)(&[18]).missing.len(), 1);
        // One line about the missing track list and one that says what was found: nothing else.
        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[0].contains("no music.idx"), "{}", seen[0]);
        assert!(seen[1].starts_with("sound files found: 1 music tracks (0 ogg, 1 wav), music.idx 0 names"), "{}", seen[1]);
    }

    #[test]
    fn the_track_list_goes_to_the_script_reader() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("music.idx"), b"oa\r\nob\r\ndun2\r\n").expect("a list");
        let files = ResolvedAudio { music_idx: Some(dir.path().join("music.idx")), ..ResolvedAudio::default() };
        let (log, seen) = lines();
        let names = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let kept = Arc::clone(&names);
        let tasks = tasks_from(
            Ok(files),
            &log,
            move |list| {
                *kept.lock().expect("names") = list.names().to_vec();
                Arc::new(|_| AreaMusic::default())
            },
            no_field,
        )
        .expect("a list alone is something");
        assert_eq!(*names.lock().expect("names"), ["oa", "ob", "dun2"]);
        assert!((tasks.decode)("oa").is_err(), "a name in the list without a file is a missing track");
        assert!(seen.lock().expect("log").iter().any(|l| l.contains("music.idx 3 names")));
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

    /// A tiny `audio.fmt`/`audio.dat` pair (invented): id 1 is a clip of 3 frames of 16-bit PCM, id 2 an empty slot, id 3 a clip of
    /// 2 frames. No game data.
    fn tiny_bank(dir: &std::path::Path) -> ResolvedAudio {
        let record = |length: u32, offset: u32| {
            let mut v = Vec::new();
            for word in [length, offset, 0, 0, 0, 0] {
                v.extend_from_slice(&word.to_le_bytes());
            }
            v.extend_from_slice(&1u16.to_le_bytes()); // PCM
            v.extend_from_slice(&1u16.to_le_bytes()); // mono
            v.extend_from_slice(&22_050u32.to_le_bytes());
            v.extend_from_slice(&44_100u32.to_le_bytes());
            v.extend_from_slice(&[2, 0, 16, 0, 0, 0]); // block align, bits, cbSize
            v
        };
        let mut empty = vec![0u8; 24];
        empty[4..8].copy_from_slice(&6u32.to_le_bytes());
        empty.extend_from_slice(&[0xCD; 18]);
        let fmt: Vec<u8> = [record(6, 0), empty, record(4, 6)].concat();
        let dat: Vec<u8> = [1i16, -2, 3, 100, -100].iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(dir.join("audio.fmt"), fmt).expect("a fmt");
        std::fs::write(dir.join("audio.dat"), dat).expect("a dat");
        ResolvedAudio { sfx_fmt: Some(dir.join("audio.fmt")), sfx_dat: Some(dir.join("audio.dat")), ..ResolvedAudio::default() }
    }

    #[test]
    fn the_clips_of_some_sound_ids_are_read_by_id_and_what_has_none_says_why() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let files = tiny_bank(dir.path());
        let read = load_sounds(&files, &[3, 1, 2, 0, 4]);
        let ids: Vec<(u16, usize)> = read.clips.iter().map(|(id, clip)| (*id, clip.frames())).collect();
        assert_eq!(ids, [(3, 2), (1, 3)], "id 3 is the second clip and id 1 the first: ids are 1-based");
        assert!(read.clips.iter().all(|(_, clip)| clip.rate() == 22_050));
        let missing: Vec<(u16, &str)> = read.missing.iter().map(|(id, why)| (*id, why.as_str())).collect();
        assert_eq!(missing, [(2, "an empty slot"), (0, "sound id 0 is no sound (ids are 1-based)"), (4, "there is no sound-effect entry 3")]);
        // Nothing asked for, nothing read.
        let none = load_sounds(&files, &[]);
        assert!(none.clips.is_empty() && none.missing.is_empty());
    }

    #[test]
    fn without_effect_files_every_id_is_missing_with_the_same_reason_and_a_bad_pair_is_one_reason_too() {
        let none = load_sounds(&ResolvedAudio::default(), &[5, 18]);
        assert!(none.clips.is_empty());
        assert_eq!(none.missing.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [5, 18]);
        assert!(none.missing.iter().all(|(_, why)| why.contains("no audio.fmt")));
        let gone = ResolvedAudio {
            sfx_fmt: Some(std::path::PathBuf::from("/definitely/not/here/audio.fmt")),
            sfx_dat: Some(std::path::PathBuf::from("/definitely/not/here/audio.dat")),
            ..ResolvedAudio::default()
        };
        let read = load_sounds(&gone, &[1]);
        assert!(read.clips.is_empty() && read.missing.len() == 1 && !read.missing[0].1.contains("/definitely"), "{:?}", read.missing);
    }

    // ---- the mod layers' sound ----------------------------------------------------------------------

    use fmv_mods::testkit::{cosmo_archive_blobs, echo_archive, wav as synthetic_wav, EchoSpec};
    use fmv_mods::{Layers, ModSources};

    /// A sound layer over synthetic Echo-S and Cosmo archives (WAV clips: the decoder takes them as it takes Ogg), with its log.
    fn layer(echo: &EchoSpec<'_>, cosmo: Option<Vec<u8>>, layers: Layers) -> (tempfile::TempDir, SoundLayer, Arc<Mutex<Vec<String>>>) {
        let tmp = tempfile::tempdir().unwrap();
        let echo_path = tmp.path().join("echo.iro");
        std::fs::write(&echo_path, echo_archive(echo)).unwrap();
        let cosmo_path = cosmo.map(|bytes| {
            let p = tmp.path().join("cosmo.iro");
            std::fs::write(&p, bytes).unwrap();
            p
        });
        let mods = Mods::open(&ModSources { echo_s: Some(echo_path), cosmo: cosmo_path, dir: None }, layers);
        assert!(mods.notes().is_empty(), "{:?}", mods.notes());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let log: Logger = Arc::new(move |l| sink.lock().unwrap().push(l.to_string()));
        (tmp, SoundLayer::new(Some(Arc::new(mods)), log), lines)
    }

    fn said(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        lines.lock().unwrap().clone()
    }

    #[test]
    fn a_sound_id_plays_the_clip_its_section_names_and_the_log_says_so_once() {
        let cosmo = cosmo_archive_blobs(&[("Base\\sfx\\6303.wav", synthetic_wav(8_000, &[100, -100, 200, -200]))], Some("[6]\nsequential = [ 6303 ]\n"));
        let (_t, layer, lines) = layer(&EchoSpec::default(), Some(cosmo), Layers::ALL);
        let clip = layer.replacement(6, None).expect("Cosmo's clip for id 6");
        assert_eq!((clip.rate(), clip.frames()), (8_000, 4));
        assert_eq!(clip.loop_region(), None, "the original's loop offsets are never applied to another file");
        assert!(layer.replacement(6, Some("md1stin")).is_some());
        assert_eq!(said(&lines), ["sound 6 from cosmo"], "said once for the id, whichever field asked");
        // An id nothing names: the original plays.
        assert!(layer.replacement(7, None).is_none());
    }

    #[test]
    fn a_fields_ambience_is_decoded_once_however_often_the_field_is_read_and_a_shared_clip_once_for_all_its_fields() {
        let wav = synthetic_wav(8_000, &[100, -100, 200, -200, 300, -300]);
        let config = "[field_116]\nsequential = [ \"1503\" ]\nfade_in = 0.5\nfade_out = 0.5\n[field_117]\nsequential = [ \"1503\" ]\n\
                      [field_118]\nsequential = [ \"1511\" ]\n";
        let blobs = [
            ("FA\\Ambient\\config.toml", config.as_bytes().to_vec()),
            ("Ambience\\Ambient\\1503.wav", wav.clone()),
            ("Ambience\\Ambient\\1511.wav", wav),
        ];
        let (_t, layer, lines) = layer(&EchoSpec::default(), Some(cosmo_archive_blobs(&blobs, None)), Layers::ALL);
        let a = layer.ambience(116).expect("md1stin's ambience");
        assert_eq!((a.section.as_str(), a.name.as_str(), a.source, a.clip.rate(), a.clip.frames()), ("field_116", "Ambience\\Ambient\\1503.wav", "cosmo", 8_000, 6));
        assert_eq!((a.fade_in, a.fade_out), (Duration::from_millis(500), Duration::from_millis(500)));
        let decoded = || layer.decoded.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(decoded(), 1);
        // The field read again (a story step), and the next field with the same clip: no decoding.
        assert!(layer.ambience(116).is_some() && layer.ambience(117).is_some());
        assert_eq!(decoded(), 1);
        assert_eq!(layer.ambience(117).unwrap().section, "field_117");
        // Another clip is decoded once; a field nothing names has none; nothing went wrong.
        assert!(layer.ambience(118).is_some() && layer.ambience(118).is_some() && layer.ambience(1).is_none());
        assert_eq!(decoded(), 2);
        assert!(said(&lines).is_empty(), "{:?}", said(&lines));
    }

    #[test]
    fn a_sound_ids_clips_come_in_the_order_its_section_names_them_and_a_bad_later_one_is_left_out() {
        let blobs = [
            ("Base\\sfx\\6303.wav", synthetic_wav(8_000, &[100, -100])),
            ("Base\\sfx\\6304.ogg", b"OggS-but-not-audio".to_vec()),
            ("Base\\sfx\\6305.wav", synthetic_wav(8_000, &[100, -100, 200, -200, 300, -300])),
        ];
        let cosmo = cosmo_archive_blobs(&blobs, Some("[6]\nsequential = [ 6303, 6304, 6305 ]\n"));
        let (_t, layer, _lines) = layer(&EchoSpec::default(), Some(cosmo), Layers::ALL);
        let clips = layer.replacements(6, None).expect("Cosmo's clips for id 6");
        assert_eq!(clips.iter().map(SfxClip::frames).collect::<Vec<_>>(), [2, 6]);
        assert_eq!(layer.replacement(6, None).map(|c| c.frames()), Some(2), "one clip: the first");
    }

    #[test]
    fn the_layer_off_or_a_clip_that_does_not_decode_leaves_the_original() {
        let blobs = [("Base\\sfx\\6303.ogg", b"OggS-but-not-audio".to_vec())];
        let cosmo = cosmo_archive_blobs(&blobs, Some("[6]\nsequential = [ 6303 ]\n"));
        let (_t, bad, lines) = layer(&EchoSpec::default(), Some(cosmo.clone()), Layers::ALL);
        assert!(bad.replacement(6, None).is_none());
        assert!(said(&lines).iter().any(|l| l.starts_with("sound 6: Base\\sfx\\6303.ogg does not decode")), "{:?}", said(&lines));
        let (_t, off, lines) = layer(&EchoSpec::default(), Some(cosmo), Layers { sound: false, ..Layers::ALL });
        assert!(off.replacement(6, None).is_none() && said(&lines).is_empty());
        // No mods at all.
        assert!(SoundLayer::off(Arc::new(|_| {})).replacement(6, None).is_none());
    }

    #[test]
    fn a_track_comes_from_echo_s_and_the_original_plays_without_one() {
        let mut spec = EchoSpec::default();
        spec.blobs.push(("EchoeS Of The Planet\\vgmstream\\BAT.wav", synthetic_wav(8_000, &[1_000; 80])));
        let (_t, layer, lines) = layer(&spec, None, Layers::ALL);
        let music = layer.track("bat").expect("Echo-S's bat");
        assert_eq!((music.rate(), music.frames()), (8_000, 80));
        assert_eq!(said(&lines), ["music bat from echo-s"]);
        assert!(layer.track("aseri").is_none());
        let (_t, off, _) = layer_with_off(&spec);
        assert!(off.track("bat").is_none());
    }

    fn layer_with_off(spec: &EchoSpec<'_>) -> (tempfile::TempDir, SoundLayer, Arc<Mutex<Vec<String>>>) {
        layer(spec, None, Layers { music: false, ..Layers::ALL })
    }

    #[test]
    fn a_voice_is_a_clip_that_ends_in_a_silent_loop_region_and_knows_its_own_length() {
        let mut spec = EchoSpec::default();
        spec.blobs.push(("VO\\Voice\\md1stin\\4a.wav", synthetic_wav(8_000, &[500; 800])));
        spec.blobs.push(("VO\\Voice\\md1stin\\9.ogg", b"OggS-but-not-audio".to_vec()));
        let (_t, layer, _) = layer(&spec, None, Layers::ALL);
        let say = |dialog| VoiceCue::Say { field: "md1stin".into(), window: 0, dialog, page: 0 };
        let found = layer.voice(&say(4)).unwrap().expect("the page-lettered clip is found");
        assert_eq!(found.name, "VO\\Voice\\md1stin\\4a.wav");
        assert_eq!(found.length, Duration::from_millis(100), "800 frames at 8 kHz");
        // The clip is the voice, then 100 ms of silence that loops, so that the loop handle stops it.
        assert_eq!(found.clip.frames(), 800 + 800);
        assert_eq!(found.clip.loop_region(), Some((800, 1_600)));
        // A window with no clip is no error; a clip that does not decode is a reason, not a panic.
        assert!(layer.voice(&say(5)).unwrap().is_none());
        assert!(layer.voice(&VoiceCue::Stop).unwrap().is_none());
        assert!(layer.voice(&say(9)).unwrap_err().starts_with("VO\\Voice\\md1stin\\9.ogg"));
        // Without mods there is nothing.
        assert!(SoundLayer::off(Arc::new(|_| {})).voice(&say(4)).unwrap().is_none());
    }

    /// The owner's real mod clips, read-only and silent (decoding makes no sound): every Echo-S track the music layer offers decodes,
    /// the first clip of every sound id the sound layer names decodes, and every voice found for a window of the reactor fields'
    /// scripts decodes. Counts only; nothing is stored, printed or played.
    #[test]
    #[ignore = "needs the owner's Cosmo Memory archive and field archive (FMV_COSMO_IRO, FMV_FF7_DATA)"]
    fn cosmos_field_and_battle_ambience_play_in_the_fields_and_fights_of_the_route() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let (Some(cosmo), Some(data)) = (var("FMV_COSMO_IRO"), var("FMV_FF7_DATA")) else {
            eprintln!("FMV_COSMO_IRO and FMV_FF7_DATA are needed; skipping");
            return;
        };
        let mods = Arc::new(Mods::open(&ModSources { echo_s: None, cosmo: Some(cosmo), dir: None }, Layers::ALL));
        assert!(mods.has_cosmo(), "{:?}", mods.notes());
        let maplist = read_maplist(&data.join("field").join("flevel.lgp")).expect("the archive's name list");
        let layer = SoundLayer::new(Some(Arc::clone(&mods)), Arc::new(|l: &str| eprintln!("{l}")));
        let route = ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5", "md8_4", "md8_1"];
        let mut with = 0;
        for key in route {
            let id = maplist.id_of(key).expect("a field of the name list");
            match layer.ambience(id) {
                Some(a) => {
                    with += 1;
                    println!("{key} (field {id}): {} {} from {}, {:.1} s of sound, fades {:.2} s in, {:.2} s out", a.section, a.name, a.source, a.clip.duration().as_secs_f32(), a.fade_in.as_secs_f32(), a.fade_out.as_secs_f32());
                    assert_eq!(a.section, format!("field_{id}"));
                    assert!(a.clip.duration() > Duration::from_millis(500), "{key}: a clip long enough to loop");
                }
                None => println!("{key} (field {id}): no ambience"),
            }
        }
        assert!(with >= route.len() / 2, "Cosmo names most fields of the route ({with} of {})", route.len());
        // And the fights of the route by their formations: md1stin's story fight, the window's debug fight, the Guard Scorpion.
        for formation in [300u16, 303, 324] {
            let a = layer.battle_ambience(formation).expect("Cosmo's battle ambience for the formation");
            println!("formation {formation}: {} {} from {}, {:.1} s of sound, fades {:.2} s in, {:.2} s out", a.section, a.name, a.source, a.clip.duration().as_secs_f32(), a.fade_in.as_secs_f32(), a.fade_out.as_secs_f32());
            assert_eq!(a.section, format!("bat_{formation}"));
        }
    }

    #[test]
    #[ignore = "needs the owner's mod archives and audio data (FMV_ECHO_S_IRO, FMV_COSMO_IRO, FMV_FF7_AUDIO_DATA)"]
    fn the_owners_mod_clips_decode() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let (Some(echo), Some(cosmo), Some(audio)) = (var("FMV_ECHO_S_IRO"), var("FMV_COSMO_IRO"), var("FMV_FF7_AUDIO_DATA")) else {
            eprintln!("FMV_ECHO_S_IRO, FMV_COSMO_IRO and FMV_FF7_AUDIO_DATA are needed; skipping");
            return;
        };
        let mods = Arc::new(Mods::open(&ModSources { echo_s: Some(echo), cosmo: Some(cosmo), dir: None }, Layers::ALL));
        assert!(mods.notes().is_empty(), "{:?}", mods.notes());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let log: Logger = Arc::new(move |l| sink.lock().unwrap().push(l.to_string()));
        let layer = SoundLayer::new(Some(Arc::clone(&mods)), log);
        // Music: every name of music.idx that Echo-S has a track for.
        let list = MusicList::parse(&std::fs::read(audio.join("music").join("music.idx")).expect("music.idx"));
        let (mut offered, mut decoded, mut looped, mut seconds) = (0usize, 0usize, 0usize, 0.0f64);
        for name in list.names() {
            if mods.music(name).is_some() {
                offered += 1;
                if let Some(music) = layer.track(name) {
                    decoded += 1;
                    looped += usize::from(music.loop_points().is_some());
                    seconds += music.duration().as_secs_f64();
                }
            }
        }
        println!("music: {offered} of the {} names of music.idx have an Echo-S track; {decoded} decode ({seconds:.0} s in all), {looped} carry loop tags the engine reads", list.len());
        assert!(offered > 0 && offered == decoded, "every track Echo-S offers decodes");
        // Sounds: the first clip of every id the layer names.
        let (mut named, mut ok, mut sounds_seconds) = (0usize, 0usize, 0.0f64);
        for id in 1..=750u16 {
            if mods.sfx(id, None).is_some() {
                named += 1;
                if let Some(clip) = layer.replacement(id, None) {
                    ok += 1;
                    sounds_seconds += clip.duration().as_secs_f64();
                }
            }
        }
        println!("sound: {named} of the 750 ids have clips; the first clip of {ok} decodes ({sounds_seconds:.0} s in all)");
        assert!(named > 600 && named == ok, "every first clip decodes");
        // Voices: every clip found for a window of the reactor fields' scripts decodes.
        let (mut windows, mut found, mut seconds) = (0usize, 0usize, 0.0f64);
        for field in ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"] {
            let section = mods.script_section(field).expect("Echo-S's script for a reactor field");
            let script = FieldScript::parse(&section).expect("script");
            for (window, dialog) in fmv_mods::compare::message_lines(&script) {
                windows += 1;
                match layer.voice(&VoiceCue::Say { field: field.to_string(), window, dialog, page: 0 }) {
                    Ok(Some(voice)) => {
                        found += 1;
                        seconds += voice.length.as_secs_f64();
                    }
                    Ok(None) => {}
                    Err(why) => panic!("a voice does not decode: {why}"),
                }
            }
        }
        println!("voice: {found} of the {windows} windows of the reactor fields have a first-page clip, all decode ({seconds:.0} s in all)");
        assert!(found > 0);
    }
}
