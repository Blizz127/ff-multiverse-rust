//! FF7 sound effects: the `audio.fmt` / `audio.dat` pair and its Microsoft ADPCM clips.
//!
//! **Verified on the owner's FF7 Steam Edition (en) on 2026-11-09** against `data/sound/audio.fmt`
//! (54 668 bytes) and `data/sound/audio.dat` (71 738 528 bytes); every other edition is unverified.
//! Measured facts (counts and sizes only, never game bytes):
//!
//! * `audio.fmt` holds **750 records**: 724 clips and 26 empty slots. Walked record by record
//!   (below) the records end exactly at the last byte of the file (26 * 42 + 724 * 74 = 54 668), the
//!   offsets chain without a gap or overlap (`offset[n + 1] == offset[n] + length[n]`) and the last
//!   record ends at exactly the size of `audio.dat`.
//! * Every clip is 44 100 Hz Microsoft ADPCM (`wFormatTag` 2, 4 bits) with the standard 7 coefficient
//!   pairs: 716 are mono (block align 1024, 2036 frames per block) and 8 are stereo (block align 2048,
//!   2036 frames per block). No clip is plain PCM, but the reader accepts PCM too.
//! * A clip's byte length is **not** a multiple of its block align (only 117 of the 724 are), so the
//!   last block is short; the short tails run from 7 bytes (two mono clips end in just their 7-byte
//!   block header, which still decodes to its 2 frames) to 1363 bytes.
//! * 90 clips carry the loop flag. Their loop offsets are multiples of the decoded frame size and
//!   lie inside the decoded clip in every case (the loop ends 1 to 2033 frames short of the last frame).
//! * The 26 empty slots are entries 198, 209, 475, 476, 487, 523, 574, 575, 579, 617, 730 to 738 and
//!   743 to 749 (ids 199, 210, 476, 477, ...: see "Sound ids").
//! * All 724 clips decode without error: 134 432 642 frames in total, about 3 s for the lot (the
//!   shortest clip, entry 43, is 2036 frames = 46 ms; the longest, entry 740, is 3 872 472 frames =
//!   87.8 s; the largest compressed clip is 3 159 379 bytes; none is silent and 24 peak at full scale).
//!   The decoder matches ffmpeg's `adpcm_ms` decoder on every clip: the same frame count and the same
//!   CRC-32 of all samples for all 724 (see "Verification").
//!
//! # Sources
//!
//! * FFNx, the Steam-era FF7/FF8 driver that reads this very file pair. Pinned to commit
//!   `4fc392ba5e1bfdd53d39aa6542464379a446083a` (2026-11-08):
//!   [`src/sfx.cpp`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/sfx.cpp)
//!   (line 38 `FF7_MAX_SFX_ID = 750`; lines 73 to 78 reject ids outside `1..=750` and read
//!   `sfx_fmt_header[id - 1]`; line 98 "Archive loop offsets address decoded PCM bytes, not compressed
//!   ADPCM bytes"; lines 129 to 131 `ff7_should_sfx_loop(id)` reads the entry's `loop` field; lines 805
//!   and 815 play id 159 for footsteps; line 848 `default_sfx_id = 493; // Tiny Bronco SFX`, a looping
//!   engine drone), [`src/ff7.h`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/ff7.h)
//!   (line 1834 `struct ff7_audio_fmt { length, offset, loop, count, loop_start, loop_end,
//!   wave_format }`) and
//!   [`misc/FFNx.SFX.toml`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/misc/FFNx.SFX.toml)
//!   (line 33, "SFX ID 1 ( menu cursor )"). The files were fetched with `curl` at that commit; the
//!   regions named here were read, not every line of them.
//! * Qhimm forum, sfxEdit / sfxdump / sfxpack thread <https://forums.qhimm.com/index.php?topic=12755.0>
//!   (through a summarising fetch; the readme itself is in a download that was not opened): the author
//!   calls the looping details "a guess on my part" and the tool refuses more than 750 effects.
//! * Qhimm forum, Cosmo audio-editor threads <https://forums.qhimm.com/index.php?topic=10237.0> (replacement
//!   clips must be "Microsoft ADPCM" at 44100 Hz, 4 bits, mono), <https://forums.qhimm.com/index.php?topic=11221.0>
//!   and <https://forums.qhimm.com/index.php?topic=17461.0> (tool pointers only, no layout).
//! * Qhimm forum, "Sound Effect Descriptions" <https://forums.qhimm.com/index.php?topic=15786.0> (post by
//!   White Wind, 2014-11-27: a hand-made list of what each sound is, numbered from 0; the first four are
//!   "Cursor Movement", "Save is Ready", "Invalid Choice", "Cancel"; the author says it is "from memory"
//!   and "probably" has mistakes).
//! * Qhimm forum, "[FF7 PC] Bonez' Sound Effect Suite" <https://forums.qhimm.com/index.php?topic=19598.25>
//!   (page 2, through a summarising fetch; reply #38, 2020-11-20, msg282664): the mod's UI sound sets go
//!   "beyond just cursor, back, error and ATB load": the four UI sounds it themes, no confirm among them.
//! * Microsoft ADPCM (`WAVE_FORMAT_ADPCM`): <https://wiki.multimedia.cx/index.php/Microsoft_ADPCM> (block
//!   header, nibble order, the two update formulas, the adaptation table, the 7 standard coefficient
//!   pairs and the samples-per-block formula). The page writes both scalings as `/ 256` and never says
//!   how negative values round; this decoder truncates toward zero (C `/`), which is what ffmpeg's
//!   `libavcodec/adpcm.c` does, and the match with ffmpeg on all 724 clips is the evidence for it.
//! * FFRTT wiki, "FF7/Field/Script/Opcodes/30_IFKEY" <https://wiki.ffrtt.ru/index.php/FF7/Field/Script/Opcodes/30_IFKEY>
//!   (through a summarising fetch; its table lists "L2" twice, so only the OK, cancel and d-pad rows are
//!   used): the key masks behind the "scripted menus" evidence below. Qhimm forum "Audio in Field Script
//!   Explained" <https://forums.qhimm.com/index.php?topic=16937.0>: the field `SOUND` opcode plays on
//!   effect channel 1, has a sound id and a pan, and loops when it sits in a `main` part of a script.
//! * Not reachable from this machine: the Qhimm and FFRTT wiki pages for the field `SOUND` opcode itself
//!   (HTTP 402 / index-only), so the field script side rests on the owned scripts (see "Sound ids").
//!
//! # Layout of `audio.fmt` (all integers little-endian)
//!
//! A bare sequence of records, no file header and no count. Each record is
//!
//! ```text
//! +0   u32  length       compressed bytes in audio.dat (0 = EMPTY SLOT)
//! +4   u32  offset       absolute offset of the clip in audio.dat
//! +8   u32  loop flag    1 = the game loops this sound while it plays, 0 = plays once (only 0 and 1 occur)
//! +12  u32  count        always 0 in the owned file; FFNx names it `count`, its meaning is unknown (ignored)
//! +16  u32  loop start   byte offset into the DECODED 16-bit PCM (0 unless the flag is set)
//! +20  u32  loop end     byte offset into the decoded PCM, one past the loop (0 unless the flag is set)
//! +24  18   WAVEFORMATEX: tag u16, channels u16, rate u32, avg bytes/s u32, block align u16, bits u16, cbSize u16
//! +42  cbSize  extra data (ADPCM: u16 samples per block, u16 coefficient count, then count * (i16, i16))
//! ```
//!
//! **An empty slot is only 42 bytes**: `length` is 0, `offset` is the running end of the previous
//! clip, the six header words after `length` and `offset` are zero and the 18 format bytes are
//! uninitialised memory (every one is `0xCD`, the MSVC debug fill), with no extra data. This is
//! why the file size is not a multiple of the 74-byte size of an ADPCM record (42 + 32), and why
//! walking it with each record's own `cbSize` goes wrong at the first empty slot: the garbage
//! `cbSize` is 0xCDCD. A reader must stop looking at the format as soon as `length` is 0. The
//! last seven empty slots have `offset` equal to the size of `audio.dat` (a zero-length entry at the
//! very end is valid).
//!
//! # Where the documentation and the data disagree
//!
//! * The sfxEdit thread says the loop information is a guess. FFNx and the data settle it: the flag
//!   is 0 or 1, and the start/end are byte offsets in the decoded 16-bit PCM (FFNx divides them by
//!   `channels * 2`), never in the compressed bytes. All 90 flagged clips satisfy
//!   `start < end <= decoded bytes` and start/end multiples of `channels * 2`.
//! * The "16 more bytes" after `length` and `offset` are the loop flag, the always-zero `count`, the
//!   loop start and the loop end (FFNx's `ff7_audio_fmt`).
//! * The format area of an empty slot is not a `WAVEFORMATEX` at all (see above).
//! * The average byte rate field takes two values per channel count (mono: 21 504 for 615 clips and
//!   22 179 for 101; stereo: 43 008 for 5 and 44 359 for 3; the exact figure is 22 179.3 for
//!   1024-byte blocks); the decoder never uses it.
//!
//! # Sound ids
//!
//! The game's sound id (the field script `SOUND` opcode's `id`, the menus' `play_sfx(id)`) is
//! **1-based: entry = id - 1**; id 0 is "no sound" (a `SOUND 0` stops the effect channel). Evidence:
//!
//! * FFNx's `sfx_fmt_header[id - 1]` and its `id < 1 || id > 750` check (sfx.cpp lines 73 to 78) and the
//!   toml example that calls sound id 1 the menu cursor.
//! * FFNx plays id 493 (comment: "Tiny Bronco SFX") looping as the Highwind engine drone. Entry 492 is
//!   the one with the loop flag (entries 491 and 494 loop too, entry 493 does not), so the engine
//!   sound is entry `id - 1`.
//! * FFNx's `ff7_sfx_play_on_channel(pan, id, channel)` (sfx.cpp lines 381 to 411) plays `if (id)` and
//!   otherwise stops the channel (`else if (channel < 6)`): id 0 is "stop", never a sound.
//! * The field scripts of the owned `flevel.lgp` (702 fields; `tests/sound_real.rs`): 4 387 `SOUND`
//!   instructions, every `id` operand a literal, from 0 to 611 (205 distinct ids), none above 750.
//!   Read 1-based (entry `id - 1`) **no used id lands on an empty slot**; read 0-based (entry `id`)
//!   four used ids would (198 used 22 times, 209 once, 523 four times, 579 once: 28 uses of silence).
//!   Id 0 is used 197 times, as the stop it is: of the 158 scripts that contain a `SOUND 0`, 76 also
//!   play a sound whose entry carries the loop flag (read 1-based), against 64 of the 2 169 other
//!   scripts that play a sound; read 0-based the two rates are alike (29 of 158 against 280 of 2 169),
//!   so only the 1-based reading makes "start a loop ... `SOUND 0` stops it" show up in the data.
//!
//! The 26 empty slots are the ids 199, 210, 476, 477, 488, 524, 575, 576, 580, 618, 731 to 739 and 744
//! to 750 (entries 198, 209, 475, 476, 487, 523, 574, 575, 579, 617, 730 to 738 and 743 to 749):
//! [`SfxBank::info`] gives `None` for them and [`SfxBank::decode`] gives [`SoundError::EmptyEntry`],
//! which callers should treat as "silent". Only ten ids are empty under one reading of the numbering
//! and not under the other (1-based: 199, 210, 477, 488, 524, 576, 580, 618, 739 and 750; 0-based:
//! 198, 209, 475, 487, 523, 574, 579, 617, 730 and 743): that disagreement set is what the field-script
//! check above can tell apart.
//!
//! # Battle sound ids
//!
//! The battle names its sounds in the data files, and **in the same id space as the field and the menus: 1-based,
//! entry `id - 1`, with 0 and 0xFFFF meaning "no sound"**. Where the ids are:
//!
//! * the weapon records of `kernel.bin` (section 5, 44 bytes): the sound bytes at `0x24` (a hit), `0x25` (a critical hit)
//!   and `0x26` (a miss) and, at `0x0B`, a mask whose bits 0, 1 and 2 add `0x100` to the three of them (a byte only reaches
//!   255 and the bank has 750 entries); read by `fmv_ff7_battle::kernel::Kernel::weapon_sounds`, which says which mask was
//!   found on the owner's copy. Source: the "Weapon data" pages of the flat wiki
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Weapon_data.html> and of the FFRTT wiki
//!   <https://wiki.ffrtt.ru/index.php/FF7/Weapon_data> (the same table; neither says which bit is for which sound: that
//!   is derived below);
//! * the attack records (28 bytes, `kernel.bin` section 1 and the 32 of each scene of `scene.bin`): "Impact Sound", a `u16`
//!   at `0x06`, <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Attack_data.html>. The battle's animation scripts play it
//!   with the opcodes that "play sound effect, queue reaction, and display damage" (`F7`, `BE`; the page of the flat wiki
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Battle/Battle_Animation/Animation_Script.html> does not say which sound).
//!
//! What was measured on the owner's FF7 Steam Edition (en), 2026-11-09 (ids, counts and lengths only):
//!
//! * **The attack records.** 124 of the kernel's 128 records and 1295 of the 2272 attacks of the 256 scenes have 0xFFFF; no
//!   record has 0. Every spell has 0xFFFF: its sound is made by its effect, and "all effects are hardcoded" (the same
//!   animation-script page, opcode `EC`), not named by any file the library holds. The four kernel records with a sound are
//!   112, 114, 116 and 118 (the physical moves of Vincent's forms). The scenes use 20 different impact ids, 5 to 424,
//!   and none of the 20 is an empty slot or a looping clip. The limit breaks (ids 128 and up) have no attack record and an
//!   item record has no sound field at all.
//! * **The weapon records.** The mask is `0xF8` (no bit) for Cloud, Red XIII, Aerith and most of Barret's weapons, `0xFB`
//!   (bits 0 and 1) for Tifa, Cid, Yuffie, Cait Sith, Sephiroth and the rest of Barret's, `0xF9` (bit 0) for Vincent; bit 2
//!   is never set, so a miss is never a high id. Every swing shares the miss sound 5 and the guns (Barret's, Vincent's) the
//!   miss sound 47.
//!
//! **Why the ids are 1-based in battle too.** Four independent pieces of evidence:
//!
//! 1. *FFNx has one id space for every mode.* Pinned to commit `4fc392ba5e1bfdd53d39aa6542464379a446083a`:
//!    [`src/sfx.cpp`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/sfx.cpp)
//!    `ff7_sfx_play_layered` (lines 274 to 329) takes ONE `id` in the field, the menus, the world map and battles, and in
//!    `MODE_BATTLE` (lines 293 to 301) names the replacement files `battle_char_%02X_%d` and `battle_enemy_%04X_%d` with that
//!    same id; the loop flag of the played id is `sfx_fmt_header[id - 1]` (`ff7_should_sfx_loop`, lines 129 to 132) and
//!    `sfx_load_original` rejects ids outside `1..=750` (lines 73 to 78). The battle code's own hooks give ids in that range
//!    to the same functions: `sfx_fix_omnislash_sound_loading` loads 0x188 and 0x285 (lines 774 to 781) and
//!    `sfx_fix_cait_sith_roulette` plays an id on channel 4 (lines 783 to 786).
//! 2. *The weapon byte and the animation's id are one number.*
//!    [`src/ff7/battle/animations.cpp`](https://github.com/julianxhokaxhiu/FFNx/blob/4fc392ba5e1bfdd53d39aa6542464379a446083a/src/ff7/battle/animations.cpp)
//!    `battle_play_sfx_delayed_427737` (lines 673 to 697) plays a sound id out of an animation script and carries
//!    `cloud_sfx_attack_id = 18` with the note "disable bugged battle SFX when Cid is attacking, but SFX ID is cloud's
//!    one": Cloud's attack animation names sound 18, and 18 is the hit byte of all sixteen of Cloud's weapon records.
//! 3. *The clips the ids name are the ones the community calls by those roles.* The Qhimm forum's hand-made list of the
//!    sound bank, <https://forums.qhimm.com/index.php?topic=15786.0> (a post by White Wind, 2014; "from memory", with
//!    "probably mistakes", see above), numbers the files from 0 **counting only the 724 clips, not the 26 empty slots**: a
//!    list number is the entry minus the empty slots before it, so the two agree up to entry 197 and the list is 2 below the
//!    entry from 210 to 474 (the slots 198 and 209 are empty). Read like that, **27 of the 27 ids the data names for
//!    something with a name in the list are the list's clip for it** (entry = `id - 1`; the same ids read as entry = `id`
//!    would each be one clip away, a miss on "Cure Spell / Potion", Cloud's sword hit on a clip nobody named):
//!
//!    | what names the id | id | entry | list no. | the list's name for that clip |
//!    |---|---|---|---|---|
//!    | every swing's miss | 5 | 4 | 4 | missed hit |
//!    | Cloud's hit, critical (0xF8) | 18, 27 | 17, 26 | 17, 26 | Cloud's sword hit, critical sword hit |
//!    | Barret's gun: hit, critical, miss | 16, 46, 47 | 15, 45, 46 | 15, 45, 46 | chain gun, critical gun hit, missed gun hit |
//!    | Aerith's staff: hit, critical | 50, 23 | 49, 22 | 49, 22 | staff hit, critical staff hit |
//!    | Red XIII's hit | 109 | 108 | 108 | Red XIII hit |
//!    | Tifa: hit 36, critical 94, each + 0x100 | 292, 350 | 291, 349 | 289, 347 | Tifa hit, Tifa critical hit |
//!    | Yuffie: hit 89 + 0x100 | 345 | 344 | 342 | Yuffie shuriken slicing |
//!    | Vincent: hit 18 + 0x100, critical 123 | 274, 123 | 273, 122 | 271, 122 | two gun hits (see below) |
//!    | enemy `Machine Gun` and other guns | 15 | 14 | 14 | gun hit |
//!    | enemy `Bite`, `Fang` | 25 | 24 | 24 | enemy biting-type attack |
//!    | enemy `Hit`, `Combo` | 28 | 27 | 27 | punch |
//!    | enemy `Bodyblow`, `Punch`, `Bird Kick` | 35 | 34 | 34 | enemy punch-type attack |
//!    | enemy `Hand Grenade`, `Normal Shell` | 20 | 19 | 19 | grenade explosion |
//!    | enemy `Slap` | 118 | 117 | 117 | slapping |
//!    | enemy `Electro-mag Rod` | 144 | 143 | 143 | electricity |
//!    | enemy `Tentacle`, `Whip` | 221 | 220 | 218 | enemy tentacle hit |
//!    | enemy `Claw`, `Sickle` | 226 | 225 | 223 | enemy claw-type attack |
//!    | enemy `Beak` | 227 | 226 | 224 | enemy horn-type attack |
//!    | enemy `Needle`, `Blood Suck` | 228 | 227 | 225 | enemy sting-type attack |
//!    | enemy `Rage Bomber` | 250 | 249 | 247 | big bomber attack |
//!    | enemy `Short Staff`, `Flutilizer` | 348 | 347 | 345 | rod hit |
//!
//!    This also settles the mask: Tifa's, Yuffie's and Vincent's high ids land exactly on the clips the list names for them
//!    (in those rows the list's number is two below the entry, because of the two empty slots before them), so **bit 0 is
//!    the hit and bit 1 the critical hit**; and Vincent's hit id (274) is the id an enemy's `Shotgun` attack names as its impact
//!    sound in `scene.bin`. The one place the list and the data disagree is Vincent: the list calls its number 122 "Vincent
//!    gun hit" and 271 "Vincent critical gun hit", the record has the critical byte 123 (entry 122) and the hit byte 18
//!    (entry 273, list number 271). The client follows the record, which is the data (and 274 is a plain impact for the
//!    enemy that uses it); the list is from memory.
//! 4. *The lengths fit.* The critical clip is longer than the hit clip for ten of the twelve distinct weapon sets (Cloud's
//!    921 ms against 475 ms; Vincent's 976 ms against 857 ms, which is also a small point for following his record rather
//!    than the list's labels; the exceptions are Barret's guns, a burst against a single shot, and Cid's). Every miss is a
//!    short swish (307 ms) and the guns' miss a little longer (389 ms). Explosions (a grenade 902 ms, a bomber 1489 ms) are
//!    longer than a punch (432 ms), a bite (499 ms) or a whip (274 ms).
//!
//! **Confidence.** Very high that a weapon's three bytes and an attack's impact id are entries `id - 1` of this bank, and
//! that bit 0 of the mask is the hit and bit 1 the critical hit (27 of 27 clips are the named ones). **What the data does
//! NOT hold, so what is not played from it:** the sound of a spell, an item or a limit break (made by the effect code, not
//! named in any file the library has), of an enemy's or a boss's death, of a dodge, of a summon. Two sounds the client may
//! play beyond the data are named only by that list: entry 21 (id 22), "Enemy Death", and entry 25 (id 26), "Escaping from
//! a Battle" (the list's number for entries below 198 is the entry). They are identifications by ear, not data, and
//! nothing says when the game plays them (a boss has a clip of its own, "Boss Death"): the client plays them only when asked
//! (`FMV_BATTLE_SOUNDS=all`).
//!
//! # The menu sounds
//!
//! The first four entries of the file are the menu's (ids 1 to 4). Documented sources first: FFNx's toml
//! example calls **id 1 the menu cursor** (it shuffles it with ids 2, 3 or 4), and the Qhimm "Sound Effect
//! Descriptions" list, numbered from 0 (so its entries 0 to 3 are ids 1 to 4), names its first four
//! "Cursor Movement", "Save is Ready", "Invalid Choice" and "Cancel" (its author says it is from memory).
//! Then measured (all four are mono, 44 100 Hz, no loop; "spectral peak" is the strongest bin of a
//! Goertzel scan of the first 8192 frames, 160 log-spaced bins from 40 Hz to 10 kHz, Hann window;
//! `tests/sound_real.rs` asserts these):
//!
//! | id | entry | length | peak | spectral peak (centroid) | zero crossings | reads as |
//! |----|-------|--------|------|--------------------------|----------------|----------|
//! | 1 | 0 | 129 ms | 26 290 | 2 096 Hz (1 875 Hz) | 1 844 Hz | a very short, loud click: [`MENU_CURSOR`] (and [`MENU_CONFIRM`]) |
//! | 2 | 1 | 386 ms | 14 780 | 1 046 Hz (716 Hz)   | 6 052 Hz | a bright chime, longer than the click: no menu constant |
//! | 3 | 2 | 353 ms | 16 597 | 89 Hz (155 Hz)      | 265 Hz   | a low tone: the error buzzer, [`MENU_BUZZER`] |
//! | 4 | 3 | 62 ms  | 13 861 | 3 179 Hz (4 110 Hz) | 5 496 Hz | the shortest and highest blip: [`MENU_CANCEL`] |
//!
//! The field scripts back up ids 1 and 3 (a `SOUND` inside the block an `IFKEY*` test guards; key masks
//! from the FFRTT IFKEY page: 0x1000 up, 0x2000 right, 0x4000 down, 0x8000 left, 0x20 OK, 0x40 cancel,
//! 0x800 start): `SOUND 1` is played 63 times under the d-pad keys (right 36, left 24, down 2, up 1) and
//! 9 times under OK/cancel keys, `SOUND 3` 11 times under the d-pad keys (right 4, left 4, down 2, up 1)
//! and 4 times under OK/cancel/start keys: scripted menus click when the cursor moves and buzz when it
//! cannot. `SOUND 4` (17 uses) is never inside a key test. FFNx's other ids check out too: id 159 (the
//! footstep) is a 204 ms thump with its spectral peak at 89 Hz and no loop.
//!
//! **The confirm sound is taken to be the cursor click (`MENU_CONFIRM == MENU_CURSOR`): the least certain
//! call in this module.** No source names a separate confirm sound (the Bonez list above names cursor,
//! back, error and ATB load); the blocks guarded by the plain OK key play id 1 (4 times) or the buzzer
//! id 3 (2 times) and never id 2; and no field script plays id 2 at all (0 of 4 387 `SOUND`s, against
//! 81, 62 and 17 for ids 1, 3 and 4). The chime at id 2 is what the Qhimm list calls "Save is Ready":
//! probably the save point's or the ATB-ready chime, which the engine plays itself, not the menu's OK.
//! If the owner's ear says the real OK has a sound of its own, point `MENU_CONFIRM` at it (id 2 is the
//! candidate).
//!
//! # Loops
//!
//! [`LoopRegion`] is in **frames**: the game's byte offsets divided by `channels * 2`. Many loops start
//! at 0 and end a little before the last frame (the encoder's padding), so a mixer must loop
//! `[start, end)` and not the whole clip. Only the 90 flagged clips have one.
//!
//! # Decoder notes
//!
//! * Block header: per channel a predictor index (byte), a signed initial delta (`i16`), then
//!   "sample 1" and "sample 2" (`i16`); the output starts with sample 2 and then sample 1, then one frame
//!   per nibble (mono: high nibble first; stereo: the high nibble is the left channel and the low
//!   nibble the right). Coefficients come from the record's own extra data (the table is not assumed to
//!   be the stock 7 pairs, but a predictor index outside the table is an error, never a panic).
//! * Every value is untrusted: the prediction is computed in `i64`, the delta is clamped to at most
//!   `i32::MAX / 768` as ffmpeg does (so a crafted block cannot overflow), and a negative initial delta
//!   is used as written for the first nibble and then floored to 16 like any other.
//! * A truncated last block decodes the nibbles that are there (header + `n` bytes gives
//!   `2 + 2n / channels` frames); a tail shorter than the block header contributes nothing.
//!   [`ms_adpcm_frame_count`] is the one formula behind both [`SfxInfo::frames`] and the decoder.
//!
//! # Verification
//!
//! Unit tests use synthetic data only, including five invented blocks whose expected output came from
//! ffmpeg's `adpcm_ms` (mono, stereo, a header-only tail, a negative initial delta, extreme values).
//! `tests/sound_real.rs` (`#[ignore]`; needs `FMV_FF7_AUDIO_DATA`, and `FMV_FF7_DATA` for the field survey)
//! walks, parses and decodes the real pair. The all-clips comparison with ffmpeg was done once outside
//! the repository: with `FMV_FF7_SFX_PRINT_HASHES=1` that test prints `HASH index frames channels crc32`
//! per clip, and the same line computed from ffmpeg's `-f s16le` output for the same clip (a WAV header
//! rebuilt from the record, piped to ffmpeg on stdin, so nothing was written to disk) was identical for all
//! 724 clips.
//!
//! # Known gaps
//!
//! * Only mono/stereo, 4-bit ADPCM and 8/16-bit PCM clips; the owned file needs nothing else.
//! * The `count` word and the average byte rate are ignored.
//! * Clip bytes above [`MAX_CLIP_BYTES`] are refused at decode time (the largest real one is
//!   3 159 379 bytes).
//! * Which sound the game plays for which action is not mapped here: the four menu sounds are named below, the battle's
//!   weapon and attack ids are in the data and documented in "Battle sound ids" (loading one is [`SfxBank::decode_sound_id`]),
//!   and [`MENU_CONFIRM`] being the cursor click is an inference, not a proof (see "The menu sounds").

use crate::util::{slice_at, u16_at, u32_at, u8_at};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Bytes of a record before its `WAVEFORMATEX`: six little-endian `u32`s.
const RECORD_HEADER_LEN: usize = 24;
/// Bytes of a `WAVEFORMATEX` without its `cbSize` extra data.
const WAVEFORMATEX_LEN: usize = 18;
/// The smallest record (and the size of an empty slot): header plus the format area.
const RECORD_MIN_LEN: usize = RECORD_HEADER_LEN + WAVEFORMATEX_LEN;

/// `wFormatTag` of plain PCM.
const WAVE_FORMAT_PCM: u16 = 1;
/// `wFormatTag` of Microsoft ADPCM.
const WAVE_FORMAT_ADPCM: u16 = 2;

/// Records a bank may list: sound ids are `u16` and id 0 means "no sound".
pub const MAX_ENTRIES: usize = 65_535;
/// Largest `audio.fmt` [`SfxBank::open`] reads (65 535 records of at most 74 bytes is 4.9 MB).
pub const MAX_FMT_BYTES: u64 = 8 * 1024 * 1024;
/// Largest compressed clip [`SfxBank::decode`] reads and expands (the largest real one is 3 159 379
/// bytes); 4-bit ADPCM expands about fourfold, so this bounds a decoded clip near 33 MB.
pub const MAX_CLIP_BYTES: u32 = 8 * 1024 * 1024;
/// Sample rates a record may declare.
const MIN_RATE: u32 = 1;
const MAX_RATE: u32 = 384_000;

/// The Microsoft ADPCM adaptation table (multimedia.cx), indexed by the raw 4-bit code.
const ADAPTATION: [i64; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
/// A delta never drops below 16 after an update.
const MIN_DELTA: i64 = 16;
/// Upper bound that keeps `delta * 768` far from overflow (ffmpeg clamps at the same value).
const MAX_DELTA: i64 = (i32::MAX / 768) as i64;

/// The seven coefficient pairs every real record carries (`coeff1`, `coeff2`; multimedia.cx). The
/// decoder reads the pairs from each record's own extra data; this is the reference table.
pub const MS_ADPCM_STANDARD_COEFS: [[i16; 2]; 7] =
    [[256, 0], [512, -256], [0, 0], [192, 64], [240, 0], [460, -208], [392, -232]];

/// The sound id of the menu cursor move (a very short click).
pub const MENU_CURSOR: u16 = 1;
/// The sound id of the menu confirm: the same click as [`MENU_CURSOR`]. No separate confirm sound is
/// documented, field scripts play id 1 for the OK key and never play id 2 (a bright chime); see "The
/// menu sounds" in the module docs. The least certain of the four.
pub const MENU_CONFIRM: u16 = MENU_CURSOR;
/// The sound id of the menu error buzzer (a low tone).
pub const MENU_BUZZER: u16 = 3;
/// The sound id of the menu cancel (a very short high blip).
pub const MENU_CANCEL: u16 = 4;

/// The entry (index into the bank) that holds the sound the game calls `id`: ids are 1-based, so this
/// is `id - 1` and id 0 ("no sound") has none. The id may still be past the end of a given bank or
/// name an empty slot; [`SfxBank::info`] and [`SfxBank::decode`] say so.
pub fn entry_for_sound_id(id: u16) -> Option<usize> {
    id.checked_sub(1).map(usize::from)
}

/// What can go wrong reading the sound files. The messages hold record numbers and counts, never
/// file bytes or paths.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SoundError {
    #[error("audio.fmt record {record} is cut short")]
    Truncated { record: usize },
    #[error("audio.fmt record {record} is not a supported format: {why}")]
    BadFormat { record: usize, why: &'static str },
    #[error("audio.fmt record {record} points past the end of audio.dat")]
    OutOfRange { record: usize },
    #[error("audio.fmt lists too many records")]
    TooManyEntries,
    #[error("audio.fmt is too large")]
    FmtTooLarge,
    #[error("sound id 0 is no sound (ids are 1-based)")]
    NoSoundId,
    #[error("there is no sound-effect entry {0}")]
    NoSuchEntry(usize),
    #[error("sound-effect entry {0} is an empty slot")]
    EmptyEntry(usize),
    #[error("sound-effect entry {0} is too large to decode")]
    ClipTooLarge(usize),
    #[error("this bank has no audio.dat to read clips from")]
    NoDataFile,
    #[error("audio.dat ends before sound-effect entry {0} does")]
    ShortData(usize),
    #[error("reading the sound files failed: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("sound-effect entry {entry} does not decode: {source}")]
    Adpcm {
        entry: usize,
        #[source]
        source: AdpcmError,
    },
}

fn io_error(e: std::io::Error) -> SoundError {
    SoundError::Io(e.kind())
}

/// What can go wrong decoding Microsoft ADPCM.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AdpcmError {
    #[error("only mono and stereo ADPCM is supported")]
    Channels,
    #[error("the block alignment is smaller than the block header")]
    BlockAlign,
    #[error("a block selects a predictor the coefficient table does not have")]
    Predictor,
    #[error("a block is shorter than its header")]
    ShortBlock,
    #[error("the decoded clip would be too large")]
    TooLarge,
}

/// A loop inside a clip, in frames: playback jumps from `end` back to `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    /// First frame of the loop.
    pub start: u64,
    /// One past the last frame of the loop (at most the clip's frame count).
    pub end: u64,
}

/// What `audio.fmt` says about one clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfxInfo {
    /// Entry number (the sound id minus one).
    pub index: usize,
    pub rate: u32,
    pub channels: u16,
    /// Frames the clip decodes to (one frame is one sample per channel).
    pub frames: u64,
    /// Compressed bytes of the clip in `audio.dat`.
    pub bytes: u32,
    /// The loop, when the entry has the loop flag and sane loop offsets.
    pub loop_region: Option<LoopRegion>,
}

/// A decoded clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmClip {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved 16-bit samples, `frames() * channels` of them.
    pub samples: Vec<i16>,
    pub loop_region: Option<LoopRegion>,
}

impl PcmClip {
    /// Frames in the clip.
    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(usize::from(self.channels)).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// Microsoft ADPCM with the record's own coefficient pairs.
    Adpcm { coefs: Vec<[i16; 2]> },
    /// Plain PCM, 8-bit unsigned or 16-bit signed.
    Pcm { bits: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Format {
    channels: u16,
    rate: u32,
    block_align: u16,
    kind: Kind,
}

impl Format {
    /// Frames `bytes` compressed bytes decode to.
    fn frames(&self, bytes: u32) -> u64 {
        match &self.kind {
            Kind::Adpcm { .. } => ms_adpcm_frame_count(u64::from(bytes), self.channels, self.block_align).unwrap_or(0),
            Kind::Pcm { bits } => {
                let frame = u64::from(self.channels) * u64::from(bits / 8);
                u64::from(bytes).checked_div(frame).unwrap_or(0)
            }
        }
    }

    /// Bytes of one frame in the unit the loop offsets use (decoded 16-bit PCM for ADPCM).
    fn loop_unit(&self) -> u64 {
        let width = match &self.kind {
            Kind::Adpcm { .. } => 2,
            Kind::Pcm { bits } => u64::from(bits / 8),
        };
        u64::from(self.channels) * width
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Compressed bytes in `audio.dat`; 0 for an empty slot.
    bytes: u32,
    offset: u32,
    loop_flag: bool,
    loop_start: u32,
    loop_end: u32,
    /// `None` for an empty slot (its format area is garbage and is never read).
    format: Option<Format>,
}

impl Entry {
    fn loop_region(&self, format: &Format, frames: u64) -> Option<LoopRegion> {
        if !self.loop_flag {
            return None;
        }
        let unit = format.loop_unit();
        if unit == 0 {
            return None;
        }
        let (start, end) = (u64::from(self.loop_start), u64::from(self.loop_end));
        let (first, last) = (start / unit, end / unit);
        // an offset that is not a whole frame is a damaged record: no loop rather than a wrong one
        if first * unit != start || last * unit != end || first >= last || last > frames {
            return None;
        }
        Some(LoopRegion { start: first, end: last })
    }
}

/// The parsed `audio.fmt` index, plus where `audio.dat` is.
///
/// Cloning is cheap enough (a few hundred small entries) and the bank is `Send + Sync`: it holds no
/// open file, [`SfxBank::decode`] opens `audio.dat` for each call and reads only the one clip.
#[derive(Debug, Clone)]
pub struct SfxBank {
    entries: Vec<Entry>,
    dat_len: u64,
    dat_path: Option<PathBuf>,
}

impl SfxBank {
    /// Parse the bytes of `audio.fmt`; `dat_len` is the size of `audio.dat`. Every clip record must
    /// fit the file (`offset + length <= dat_len`); the walk must end exactly at the end of `fmt`.
    /// An empty `fmt` is a valid bank with no entries. A bank made this way has no data file, so
    /// [`SfxBank::decode`] fails with [`SoundError::NoDataFile`]; use [`SfxBank::decode_from`].
    pub fn parse(fmt: &[u8], dat_len: u64) -> Result<SfxBank, SoundError> {
        let mut entries = Vec::new();
        let mut pos = 0usize;
        while pos < fmt.len() {
            let record = entries.len();
            if record >= MAX_ENTRIES {
                return Err(SoundError::TooManyEntries);
            }
            let truncated = || SoundError::Truncated { record };
            // every record has at least the header and the 18 format bytes, empty slots included
            let head = slice_at(fmt, pos, RECORD_MIN_LEN).ok_or_else(truncated)?;
            let word = |off: usize| u32_at(head, off).ok_or_else(truncated);
            let (length, offset, loop_flag) = (word(0)?, word(4)?, word(8)?);
            let (loop_start, loop_end) = (word(16)?, word(20)?);
            if length == 0 {
                // the format area of an empty slot is uninitialised memory: do not read it
                entries.push(Entry { bytes: 0, offset, loop_flag: false, loop_start: 0, loop_end: 0, format: None });
                pos += RECORD_MIN_LEN;
                continue;
            }
            if u64::from(offset) + u64::from(length) > dat_len {
                return Err(SoundError::OutOfRange { record });
            }
            let tag = u16_at(head, RECORD_HEADER_LEN).ok_or_else(truncated)?;
            let channels = u16_at(head, RECORD_HEADER_LEN + 2).ok_or_else(truncated)?;
            let rate = word(RECORD_HEADER_LEN + 4)?;
            let block_align = u16_at(head, RECORD_HEADER_LEN + 12).ok_or_else(truncated)?;
            let bits = u16_at(head, RECORD_HEADER_LEN + 14).ok_or_else(truncated)?;
            let extra_len = usize::from(u16_at(head, RECORD_HEADER_LEN + 16).ok_or_else(truncated)?);
            let extra = slice_at(fmt, pos + RECORD_MIN_LEN, extra_len).ok_or_else(truncated)?;
            let format = parse_format(record, tag, channels, rate, block_align, bits, extra)?;
            entries.push(Entry {
                bytes: length,
                offset,
                loop_flag: loop_flag != 0,
                loop_start,
                loop_end,
                format: Some(format),
            });
            pos += RECORD_MIN_LEN + extra_len;
        }
        Ok(SfxBank { entries, dat_len, dat_path: None })
    }

    /// Read `audio.fmt` (a small file) and remember where `audio.dat` is; the data file is only
    /// measured here, and later only seeked into when a clip is decoded.
    pub fn open(fmt: &Path, dat: &Path) -> Result<SfxBank, SoundError> {
        let mut bytes = Vec::new();
        let file = File::open(fmt).map_err(io_error)?;
        file.take(MAX_FMT_BYTES + 1).read_to_end(&mut bytes).map_err(io_error)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FMT_BYTES {
            return Err(SoundError::FmtTooLarge);
        }
        let dat_len = std::fs::metadata(dat).map_err(io_error)?.len();
        let mut bank = SfxBank::parse(&bytes, dat_len)?;
        bank.dat_path = Some(dat.to_path_buf());
        Ok(bank)
    }

    /// Records in the index, empty slots included (750 in the Steam edition).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records that hold a clip (724 in the Steam edition).
    pub fn clip_count(&self) -> usize {
        self.entries.iter().filter(|e| e.format.is_some()).count()
    }

    /// Size of the `audio.dat` the index was validated against.
    pub fn dat_len(&self) -> u64 {
        self.dat_len
    }

    /// What the index says about entry `index`; `None` when there is no such entry **or when it is an
    /// empty slot** (compare [`SfxBank::len`] with [`SfxBank::clip_count`]).
    pub fn info(&self, index: usize) -> Option<SfxInfo> {
        let entry = self.entries.get(index)?;
        let format = entry.format.as_ref()?;
        let frames = format.frames(entry.bytes);
        Some(SfxInfo {
            index,
            rate: format.rate,
            channels: format.channels,
            frames,
            bytes: entry.bytes,
            loop_region: entry.loop_region(format, frames),
        })
    }

    /// What the index says about the sound the game calls `id` (1-based, see "Sound ids"); `None` for id 0, an id past the
    /// end and an empty slot.
    pub fn info_for_sound_id(&self, id: u16) -> Option<SfxInfo> {
        self.info(entry_for_sound_id(id)?)
    }

    /// Decode the sound the game calls `id` ([`entry_for_sound_id`]) from the `audio.dat` this bank was opened with: what
    /// the battle's weapon and attack records name (see "Battle sound ids"). Id 0 is [`SoundError::NoSoundId`] and an
    /// empty slot is [`SoundError::EmptyEntry`], which a caller should treat as "silent".
    pub fn decode_sound_id(&self, id: u16) -> Result<PcmClip, SoundError> {
        self.decode(entry_for_sound_id(id).ok_or(SoundError::NoSoundId)?)
    }

    /// Decode entry `index` from the `audio.dat` this bank was opened with.
    pub fn decode(&self, index: usize) -> Result<PcmClip, SoundError> {
        let path = self.dat_path.as_deref().ok_or(SoundError::NoDataFile)?;
        self.clip(index)?; // a bad index never touches the disk
        let mut file = File::open(path).map_err(io_error)?;
        self.decode_from(index, &mut file)
    }

    /// Decode entry `index` from any `audio.dat` stream (tests use in-memory data): seeks to the clip,
    /// reads exactly its bytes (at most [`MAX_CLIP_BYTES`]) and expands them.
    pub fn decode_from<R: Read + Seek + ?Sized>(&self, index: usize, dat: &mut R) -> Result<PcmClip, SoundError> {
        let (entry, format) = self.clip(index)?;
        if entry.bytes > MAX_CLIP_BYTES {
            return Err(SoundError::ClipTooLarge(index));
        }
        dat.seek(SeekFrom::Start(u64::from(entry.offset))).map_err(io_error)?;
        let want = u64::from(entry.bytes);
        let mut raw = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
        (&mut *dat).take(want).read_to_end(&mut raw).map_err(io_error)?;
        if u64::try_from(raw.len()).ok() != Some(want) {
            return Err(SoundError::ShortData(index));
        }
        let samples = match &format.kind {
            Kind::Adpcm { coefs } => ms_adpcm_decode(&raw, format.channels, format.block_align, coefs)
                .map_err(|source| SoundError::Adpcm { entry: index, source })?,
            Kind::Pcm { bits } => decode_pcm(&raw, format.channels, *bits),
        };
        let frames = u64::try_from(samples.len() / usize::from(format.channels)).unwrap_or(0);
        Ok(PcmClip {
            rate: format.rate,
            channels: format.channels,
            loop_region: entry.loop_region(format, frames),
            samples,
        })
    }

    fn clip(&self, index: usize) -> Result<(&Entry, &Format), SoundError> {
        let entry = self.entries.get(index).ok_or(SoundError::NoSuchEntry(index))?;
        let format = entry.format.as_ref().ok_or(SoundError::EmptyEntry(index))?;
        Ok((entry, format))
    }
}

fn parse_format(
    record: usize,
    tag: u16,
    channels: u16,
    rate: u32,
    block_align: u16,
    bits: u16,
    extra: &[u8],
) -> Result<Format, SoundError> {
    let bad = |why: &'static str| SoundError::BadFormat { record, why };
    if !(1..=2).contains(&channels) {
        return Err(bad("only mono and stereo clips are supported"));
    }
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(bad("the sample rate is out of range"));
    }
    let kind = match tag {
        WAVE_FORMAT_PCM => {
            if bits != 8 && bits != 16 {
                return Err(bad("PCM clips must be 8 or 16 bits"));
            }
            Kind::Pcm { bits }
        }
        WAVE_FORMAT_ADPCM => {
            if bits != 4 {
                return Err(bad("ADPCM clips must be 4 bits"));
            }
            let header_len = 7 * channels;
            if block_align < header_len {
                return Err(bad("the block alignment is smaller than the ADPCM block header"));
            }
            let per_block = u16_at(extra, 0).ok_or_else(|| bad("the ADPCM extra data is too short"))?;
            let count = usize::from(u16_at(extra, 2).ok_or_else(|| bad("the ADPCM extra data is too short"))?);
            // cbSize is the two header words plus one (coeff1, coeff2) pair per table entry, exactly
            if count == 0 || count > 256 || extra.len() != 4 + 4 * count {
                return Err(bad("cbSize does not match the ADPCM coefficient count"));
            }
            let expected = 2 + u32::from(block_align - header_len) * 2 / u32::from(channels);
            if u32::from(per_block) != expected {
                return Err(bad("samples per block do not match the block alignment"));
            }
            let table = extra.get(4..).unwrap_or(&[]);
            let mut coefs = Vec::with_capacity(count);
            for pair in table.chunks(4) {
                let (Some(a), Some(b)) = (u16_at(pair, 0), u16_at(pair, 2)) else {
                    return Err(bad("the ADPCM coefficient table is cut short"));
                };
                coefs.push([a as i16, b as i16]);
            }
            Kind::Adpcm { coefs }
        }
        _ => return Err(bad("only PCM and Microsoft ADPCM clips are supported")),
    };
    Ok(Format { channels, rate, block_align, kind })
}

/// Frames that `bytes` bytes of Microsoft ADPCM decode to: every full block of `block_align` bytes
/// gives `2 + (block_align - 7 * channels) * 2 / channels`, a short last block gives the same
/// formula for its own length, and a tail shorter than the block header gives nothing. `None` when
/// `channels` is not 1 or 2 or `block_align` is smaller than the block header. This one formula is
/// behind [`SfxInfo::frames`] and [`ms_adpcm_decode`].
pub fn ms_adpcm_frame_count(bytes: u64, channels: u16, block_align: u16) -> Option<u64> {
    let ch = u64::from(channels);
    if !(1..=2).contains(&ch) {
        return None;
    }
    let header = 7 * ch;
    let align = u64::from(block_align);
    if align < header {
        return None;
    }
    // callers below only pass `len >= header`, so the subtraction cannot underflow
    let per_block = |len: u64| 2 + (len - header) * 2 / ch;
    let (full, tail) = (bytes / align, bytes % align);
    let mut frames = full.checked_mul(per_block(align))?;
    if tail >= header {
        frames = frames.checked_add(per_block(tail))?;
    }
    Some(frames)
}

/// One channel's decoder state. All maths in `i64`, so no header value can overflow it.
#[derive(Clone, Copy, Default)]
struct Channel {
    coeff1: i64,
    coeff2: i64,
    delta: i64,
    sample1: i64,
    sample2: i64,
}

impl Channel {
    /// Decode one 4-bit code into the next sample and adapt.
    fn step(&mut self, nibble: u8) -> i16 {
        let code = usize::from(nibble & 0x0f);
        let raw = i64::from(nibble & 0x0f);
        let signed = if raw >= 8 { raw - 16 } else { raw };
        // `/` truncates toward zero, as in the C code this format comes from
        let predicted = (self.sample1 * self.coeff1 + self.sample2 * self.coeff2) / 256 + signed * self.delta;
        let sample = predicted.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        self.sample2 = self.sample1;
        self.sample1 = sample;
        let weight = ADAPTATION.get(code).copied().unwrap_or(256);
        self.delta = (weight * self.delta / 256).clamp(MIN_DELTA, MAX_DELTA);
        i16::try_from(sample).unwrap_or(0)
    }
}

fn i16_at(data: &[u8], off: usize) -> Option<i16> {
    u16_at(data, off).map(|v| v as i16)
}

/// Decode one Microsoft ADPCM block (`block` is the whole block, at most `block_align` bytes) and
/// append its interleaved frames to `out`. `coefs` is the record's coefficient table. A short block is
/// decoded as far as it goes (header plus `n` bytes gives `2 + 2n / channels` frames).
pub fn ms_adpcm_decode_block(
    block: &[u8],
    channels: u16,
    coefs: &[[i16; 2]],
    out: &mut Vec<i16>,
) -> Result<(), AdpcmError> {
    let ch = usize::from(channels);
    if !(1..=2).contains(&ch) {
        return Err(AdpcmError::Channels);
    }
    let header_len = 7 * ch;
    if block.len() < header_len {
        return Err(AdpcmError::ShortBlock);
    }
    let mut states = [Channel::default(); 2];
    let mut first = [0i16; 2];
    let mut second = [0i16; 2];
    for (c, ((state, f), s)) in states.iter_mut().zip(first.iter_mut()).zip(second.iter_mut()).enumerate().take(ch) {
        let predictor = usize::from(u8_at(block, c).ok_or(AdpcmError::ShortBlock)?);
        let [coeff1, coeff2] = *coefs.get(predictor).ok_or(AdpcmError::Predictor)?;
        let delta = i16_at(block, ch + 2 * c).ok_or(AdpcmError::ShortBlock)?;
        let sample1 = i16_at(block, 3 * ch + 2 * c).ok_or(AdpcmError::ShortBlock)?;
        let sample2 = i16_at(block, 5 * ch + 2 * c).ok_or(AdpcmError::ShortBlock)?;
        *state = Channel {
            coeff1: i64::from(coeff1),
            coeff2: i64::from(coeff2),
            delta: i64::from(delta),
            sample1: i64::from(sample1),
            sample2: i64::from(sample2),
        };
        // the block's first two frames are stored verbatim: sample 2 comes out first
        *f = sample2;
        *s = sample1;
    }
    out.extend(first.iter().take(ch));
    out.extend(second.iter().take(ch));
    let mut c = 0usize;
    for &byte in block.get(header_len..).unwrap_or(&[]) {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(state) = states.get_mut(c) {
                out.push(state.step(nibble));
            }
            c += 1;
            if c == ch {
                c = 0;
            }
        }
    }
    Ok(())
}

/// Decode a run of Microsoft ADPCM blocks of `block_align` bytes (the last may be short) into
/// interleaved 16-bit samples; see [`ms_adpcm_frame_count`] for how many. `coefs` is the record's
/// coefficient table ([`MS_ADPCM_STANDARD_COEFS`] for the stock one).
pub fn ms_adpcm_decode(
    data: &[u8],
    channels: u16,
    block_align: u16,
    coefs: &[[i16; 2]],
) -> Result<Vec<i16>, AdpcmError> {
    if !(1..=2).contains(&channels) {
        return Err(AdpcmError::Channels);
    }
    let header_len = 7 * usize::from(channels);
    if usize::from(block_align) < header_len {
        return Err(AdpcmError::BlockAlign);
    }
    let bytes = u64::try_from(data.len()).map_err(|_| AdpcmError::TooLarge)?;
    let frames = ms_adpcm_frame_count(bytes, channels, block_align).ok_or(AdpcmError::BlockAlign)?;
    let total = usize::try_from(frames)
        .ok()
        .and_then(|f| f.checked_mul(usize::from(channels)))
        .ok_or(AdpcmError::TooLarge)?;
    let mut out = Vec::with_capacity(total);
    for block in data.chunks(usize::from(block_align)) {
        if block.len() < header_len {
            break; // only the last block can be short, and a tail without a header is padding
        }
        ms_adpcm_decode_block(block, channels, coefs, &mut out)?;
    }
    Ok(out)
}

/// Plain PCM into 16-bit samples (8-bit is unsigned); a trailing partial frame is dropped.
fn decode_pcm(data: &[u8], channels: u16, bits: u16) -> Vec<i16> {
    let mut samples: Vec<i16> = if bits == 8 {
        data.iter().map(|&b| (i16::from(b) - 128) << 8).collect()
    } else {
        data.chunks(2).filter_map(|pair| u16_at(pair, 0).map(|v| v as i16)).collect()
    };
    let whole = samples.len() - samples.len().checked_rem(usize::from(channels)).unwrap_or(0);
    samples.truncate(whole);
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---------- fixtures: synthetic records, never game data ----------

    fn adpcm_extra(block_align: u16, channels: u16) -> Vec<u8> {
        let per_block = 2 + (block_align - 7 * channels) * 2 / channels;
        let mut v = per_block.to_le_bytes().to_vec();
        v.extend_from_slice(&7u16.to_le_bytes());
        for [a, b] in MS_ADPCM_STANDARD_COEFS {
            v.extend_from_slice(&a.to_le_bytes());
            v.extend_from_slice(&b.to_le_bytes());
        }
        v
    }

    /// One synthetic `audio.fmt` record.
    struct Rec {
        length: u32,
        offset: u32,
        flag: u32,
        loop_start: u32,
        loop_end: u32,
        tag: u16,
        channels: u16,
        rate: u32,
        align: u16,
        bits: u16,
        extra: Vec<u8>,
    }

    impl Rec {
        fn adpcm(length: u32, offset: u32) -> Rec {
            Rec {
                length,
                offset,
                flag: 0,
                loop_start: 0,
                loop_end: 0,
                tag: 2,
                channels: 1,
                rate: 44_100,
                align: 1024,
                bits: 4,
                extra: adpcm_extra(1024, 1),
            }
        }

        fn pcm16(length: u32, offset: u32, channels: u16) -> Rec {
            Rec { tag: 1, channels, align: 2 * channels, bits: 16, extra: Vec::new(), ..Rec::adpcm(length, offset) }
        }

        fn bytes(&self) -> Vec<u8> {
            let mut v = Vec::new();
            for word in [self.length, self.offset, self.flag, 0, self.loop_start, self.loop_end] {
                v.extend_from_slice(&word.to_le_bytes());
            }
            v.extend_from_slice(&self.tag.to_le_bytes());
            v.extend_from_slice(&self.channels.to_le_bytes());
            v.extend_from_slice(&self.rate.to_le_bytes());
            v.extend_from_slice(&self.rate.wrapping_mul(2).to_le_bytes());
            v.extend_from_slice(&self.align.to_le_bytes());
            v.extend_from_slice(&self.bits.to_le_bytes());
            v.extend_from_slice(&(self.extra.len() as u16).to_le_bytes());
            v.extend_from_slice(&self.extra);
            v
        }
    }

    /// An empty slot as the real file stores it: zero length, the running offset, zero loop words and
    /// 18 bytes of `0xCD` where the format would be.
    fn empty_slot(offset: u32) -> Vec<u8> {
        let mut v = vec![0u8; RECORD_HEADER_LEN];
        v[4..8].copy_from_slice(&offset.to_le_bytes());
        v.extend_from_slice(&[0xCD; WAVEFORMATEX_LEN]);
        v
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn fnv64(samples: &[i16]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for s in samples {
            for b in s.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }

    // ---------- test-only MS ADPCM encoder ----------

    /// Quantise `frames` (interleaved, `channels` per frame) into MS ADPCM blocks of `align` bytes; a
    /// last partial block is stored short. Greedy and closed-loop: good enough to prove the decoder
    /// inverts a real encoder, nothing more. Uses its own arithmetic, not the decoder's `Channel`.
    fn encode(samples: &[i16], channels: usize, align: usize) -> Vec<u8> {
        let per_block = 2 + (align - 7 * channels) * 2 / channels;
        let frames: Vec<&[i16]> = samples.chunks(channels).collect();
        let mut out = Vec::new();
        for chunk in frames.chunks(per_block) {
            if chunk.len() < 2 {
                break;
            }
            let mut preds = Vec::new();
            let mut deltas = Vec::new();
            let mut codes: Vec<Vec<u8>> = Vec::new();
            for c in 0..channels {
                let signal: Vec<i32> = chunk.iter().map(|f| i32::from(f[c])).collect();
                let mut best: Option<(i64, usize, Vec<u8>)> = None;
                for (p, [c1, c2]) in MS_ADPCM_STANDARD_COEFS.iter().enumerate() {
                    let (mut s2, mut s1) = (i64::from(signal[0]), i64::from(signal[1]));
                    let mut delta = 16i64;
                    let mut err = 0i64;
                    let mut cs = Vec::new();
                    for &x in &signal[2..] {
                        let pred = (s1 * i64::from(*c1) + s2 * i64::from(*c2)) / 256;
                        let diff = i64::from(x) - pred;
                        let q = ((diff as f64) / (delta as f64)).round().clamp(-8.0, 7.0) as i64;
                        let rec = (pred + q * delta).clamp(-32768, 32767);
                        err += (i64::from(x) - rec).pow(2);
                        s2 = s1;
                        s1 = rec;
                        cs.push((q & 15) as u8);
                        let w = ADAPTATION[(q & 15) as usize];
                        delta = (w * delta / 256).max(16);
                    }
                    if best.as_ref().is_none_or(|b| err < b.0) {
                        best = Some((err, p, cs));
                    }
                }
                let (_, p, cs) = best.unwrap();
                preds.push(p as u8);
                deltas.push(16i16);
                codes.push(cs);
            }
            out.extend_from_slice(&preds);
            for d in &deltas {
                out.extend_from_slice(&d.to_le_bytes());
            }
            for sample in chunk[1] {
                out.extend_from_slice(&sample.to_le_bytes()); // sample 1: the second frame
            }
            for sample in chunk[0] {
                out.extend_from_slice(&sample.to_le_bytes()); // sample 2: the first frame
            }
            if channels == 1 {
                for pair in codes[0].chunks(2) {
                    out.push((pair[0] << 4) | pair.get(1).copied().unwrap_or(0));
                }
            } else {
                for (left, right) in codes[0].iter().zip(&codes[1]) {
                    out.push((left << 4) | right);
                }
            }
        }
        out
    }

    fn sine(frames: usize, channels: usize, amplitude: f64) -> Vec<i16> {
        let mut v = Vec::new();
        for i in 0..frames {
            for c in 0..channels {
                let hz = 440.0 * (1.0 + c as f64 * 0.5);
                v.push((amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / 44_100.0).sin()) as i16);
            }
        }
        v
    }

    // ---------- the decoder ----------

    #[test]
    fn decoder_matches_reference_vectors_from_ffmpeg() {
        // (name, channels, block align, data, frames, first samples, last samples, fnv-1a 64 of the output).
        // Invented blocks; the expected output is ffmpeg's `adpcm_ms` decode of the same bytes.
        struct Vector {
            channels: u16,
            align: u16,
            data: &'static str,
            samples: usize,
            first: &'static [i16],
            last: &'static [i16],
            fnv: u64,
        }
        let vectors = [
            // mono, one full block plus a short last block, predictors 3 and 5
            Vector {
                channels: 1,
                align: 40,
                data: "03280050fb20035e18fa9446d9be5d04ee3069fc6dbe52a39a52bdd8cc6fd62c605e8151e84498a2052c01c40930f8b6f45bec4760aa28",
                samples: 86,
                first: &[800, -1200, -500, -801, -669, -1102, -1143, -1936, -3613, -625, 1704, 6653, -117, -10017, -27397, -32768],
                last: &[32767, 32767, -32768, -32768, 32767, -32768],
                fnv: 0xce46_9019_adff_8c75,
            },
            // stereo, different predictors per channel, a short last block, clipping
            Vector {
                channels: 2,
                align: 36,
                data: "010419005a00e80348f4f4013cf61d4a791ec7837d9b4de6c9fb0de65e357f001765243b06001000e803e0b1204ec8b5085219ce4222dd38",
                samples: 64,
                first: &[500, -2500, 1000, -3000, 1525, -3082, 2138, -3369, 2933, -4278, 3790, -4776, 4427, -2069, 4544, 536],
                last: &[15228, 16382, 15584, 11168, 10113, -1320],
                fnv: 0x6b37_fa1f_63f9_521f,
            },
            // extreme header values and nibbles: the delta grows until its clamp, every sample clips
            Vector {
                channels: 1,
                align: 64,
                data: "06ff7fff7f008078877f80778878877f80778878877f80778878877f80778878877f80778878877f80778878877f80778878877f80778878877f807788777777",
                samples: 116,
                first: &[-32768, 32767, 32767, -32768, -32768, 32767, 32767, -32768, -32768, -20480, 32767, 32767, -32768, -32768, 32767, -32768],
                last: &[32767, 32767, 32767, 32767, 32767, 32767],
                fnv: 0x3aa4_2d6e_1046_4e4d,
            },
            // a negative initial delta, used as written for the first nibble, then floored to 16
            Vector {
                channels: 1,
                align: 27,
                data: "019cffd08a307500000000000000000000630398f6d4b8da6d8088",
                samples: 42,
                first: &[30000, -30000, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768, -32768],
                last: &[-17252, -17252, -32768, -32768, -32768, -32768],
                fnv: 0x24b9_ff16_91eb_abe6,
            },
            // a last block that is only the 7-byte header still yields its two frames
            Vector {
                channels: 1,
                align: 16,
                data: "0010007b0038fe6998e053aa1311bb930210000700f9ff",
                samples: 22,
                first: &[-456, 123, 219, -5, -537, -1993, -3085, -3085, -885, 1221, -2559, -10119, -7599, -807, 1227, 3054],
                last: &[-5151, -18256, -32768, -2642, -7, 7],
                fnv: 0xe805_3e6b_0cfd_b76a,
            },
        ];
        for (n, v) in vectors.iter().enumerate() {
            let data = hex(v.data);
            let out = ms_adpcm_decode(&data, v.channels, v.align, &MS_ADPCM_STANDARD_COEFS).unwrap();
            assert_eq!(out.len(), v.samples, "vector {n}");
            assert_eq!(&out[..v.first.len()], v.first, "vector {n}");
            assert_eq!(&out[out.len() - v.last.len()..], v.last, "vector {n}");
            assert_eq!(fnv64(&out), v.fnv, "vector {n}");
            // the frame count formula agrees with what the decoder produced
            let frames = ms_adpcm_frame_count(data.len() as u64, v.channels, v.align).unwrap();
            assert_eq!(frames as usize * usize::from(v.channels), out.len(), "vector {n}");
        }
    }

    #[test]
    fn a_hand_computed_block() {
        // predictor 0 (256, 0): each sample is the previous one plus nibble * delta.
        // header: predictor 0, delta 16, sample 1 = 100, sample 2 = 90
        let mut block = vec![0u8, 16, 0, 100, 0, 90, 0];
        block.push(0x1F); // codes 1 and F (-1)
        block.push(0x77); // codes 7 and 7
        let mut out = Vec::new();
        ms_adpcm_decode_block(&block, 1, &MS_ADPCM_STANDARD_COEFS, &mut out).unwrap();
        // frames: 90, 100 (the header's sample 2 and sample 1), then
        //   code 1:  100 + 1 * 16 = 116, delta = 230 * 16 / 256 = 14, floored to 16
        //   code F:  116 - 1 * 16 = 100, delta = 230 * 16 / 256 = 14, floored to 16
        //   code 7:  100 + 7 * 16 = 212, delta = 614 * 16 / 256 = 38
        //   code 7:  212 + 7 * 38 = 478
        assert_eq!(out, [90, 100, 116, 100, 212, 478]);
    }

    #[test]
    fn rounding_truncates_toward_zero_for_negative_values() {
        // predictor 3 (192, 64), sample 1 = -1, sample 2 = 0: (-1 * 192 + 0 * 64) / 256 = -0.75, which
        // truncates toward zero to 0 (a floor would give -1); then sample 1 = 0, sample 2 = -1 gives
        // (0 * 192 - 64) / 256 = -0.25: 0 again (floor: -1)
        let block = [3u8, 16, 0, 0xFF, 0xFF, 0, 0, 0x00];
        let mut out = Vec::new();
        ms_adpcm_decode_block(&block, 1, &MS_ADPCM_STANDARD_COEFS, &mut out).unwrap();
        assert_eq!(out, [0, -1, 0, 0]);
    }

    #[test]
    fn stereo_nibbles_alternate_left_then_right() {
        // predictor 0 both channels, delta 16; left starts at 0, right at 1000
        let header = [0u8, 0, 16, 0, 16, 0, 0, 0, 0xE8, 0x03, 0, 0, 0, 0];
        let mut block = header.to_vec();
        block.push(0x1F); // left +1, right -1
        let mut out = Vec::new();
        ms_adpcm_decode_block(&block, 2, &MS_ADPCM_STANDARD_COEFS, &mut out).unwrap();
        // (sample 2 = 0, 0), (sample 1 = 0, 1000), then left 0 + 16 and right 1000 - 16
        assert_eq!(out, [0, 0, 0, 1000, 16, 984]);
    }

    #[test]
    fn frame_count_formula() {
        // mono, 1024-byte blocks: 2036 frames each; a 7-byte tail is the header only: 2 frames
        assert_eq!(ms_adpcm_frame_count(0, 1, 1024), Some(0));
        assert_eq!(ms_adpcm_frame_count(1024, 1, 1024), Some(2036));
        assert_eq!(ms_adpcm_frame_count(2 * 1024, 1, 1024), Some(4072));
        assert_eq!(ms_adpcm_frame_count(1024 + 6, 1, 1024), Some(2036)); // 6 bytes: not even a header
        assert_eq!(ms_adpcm_frame_count(1024 + 7, 1, 1024), Some(2038));
        assert_eq!(ms_adpcm_frame_count(1024 + 8, 1, 1024), Some(2040));
        assert_eq!(ms_adpcm_frame_count(2858, 1, 1024), Some(2 * 2036 + 2 + (810 - 7) * 2));
        // stereo, 2048-byte blocks: 2036 frames each; one byte is one frame after the 14-byte header
        assert_eq!(ms_adpcm_frame_count(2048, 2, 2048), Some(2036));
        assert_eq!(ms_adpcm_frame_count(2048 + 13, 2, 2048), Some(2036));
        assert_eq!(ms_adpcm_frame_count(2048 + 14, 2, 2048), Some(2038));
        assert_eq!(ms_adpcm_frame_count(2048 + 15, 2, 2048), Some(2039));
        // not a layout the decoder supports
        assert_eq!(ms_adpcm_frame_count(10, 0, 1024), None);
        assert_eq!(ms_adpcm_frame_count(10, 3, 1024), None);
        assert_eq!(ms_adpcm_frame_count(10, 2, 13), None);
        // absurd sizes neither panic nor wrap: the product overflows and says so
        assert!(ms_adpcm_frame_count(u64::MAX, 1, 8).is_some());
        assert_eq!(ms_adpcm_frame_count(u64::MAX, 1, u16::MAX), None);
    }

    #[test]
    fn round_trip_through_a_test_encoder_is_close_and_exact_in_length() {
        for (channels, align, frames) in [(1usize, 256usize, 3000usize), (1, 1024, 5000), (2, 512, 3000), (2, 2048, 7000)] {
            let amplitude = 12_000.0;
            let pcm = sine(frames, channels, amplitude);
            let adpcm = encode(&pcm, channels, align);
            let decoded = ms_adpcm_decode(&adpcm, channels as u16, align as u16, &MS_ADPCM_STANDARD_COEFS).unwrap();
            let predicted = ms_adpcm_frame_count(adpcm.len() as u64, channels as u16, align as u16).unwrap();
            assert_eq!(decoded.len() as u64, predicted * channels as u64);
            // a short last block adds at most the nibble that padded an odd mono count
            assert!(decoded.len() >= pcm.len() && decoded.len() <= pcm.len() + 1, "{} vs {}", decoded.len(), pcm.len());
            let mut worst = 0i32;
            let mut sum_sq = 0f64;
            for (a, b) in pcm.iter().zip(&decoded) {
                let e = i32::from(*a) - i32::from(*b);
                worst = worst.max(e.abs());
                sum_sq += f64::from(e) * f64::from(e);
            }
            let rms = (sum_sq / pcm.len() as f64).sqrt();
            // a 4-bit codec on a clean sine: well under 5% of the amplitude at worst, 1% rms
            assert!(f64::from(worst) < 0.05 * amplitude, "ch {channels} align {align}: worst {worst}");
            assert!(rms < 0.01 * amplitude, "ch {channels} align {align}: rms {rms}");
        }
    }

    #[test]
    fn decoder_rejects_bad_input_without_panicking() {
        let std = &MS_ADPCM_STANDARD_COEFS[..];
        let mut out = Vec::new();
        // predictor index 7 does not exist in a 7-entry table
        assert_eq!(ms_adpcm_decode_block(&[7, 16, 0, 0, 0, 0, 0], 1, std, &mut out), Err(AdpcmError::Predictor));
        // an empty table has no predictor at all
        assert_eq!(ms_adpcm_decode_block(&[0, 16, 0, 0, 0, 0, 0], 1, &[], &mut out), Err(AdpcmError::Predictor));
        // shorter than the header (7 bytes mono, 14 bytes stereo)
        assert_eq!(ms_adpcm_decode_block(&[0; 6], 1, std, &mut out), Err(AdpcmError::ShortBlock));
        assert_eq!(ms_adpcm_decode_block(&[0; 13], 2, std, &mut out), Err(AdpcmError::ShortBlock));
        assert_eq!(ms_adpcm_decode_block(&[0; 14], 0, std, &mut out), Err(AdpcmError::Channels));
        assert_eq!(ms_adpcm_decode_block(&[0; 21], 3, std, &mut out), Err(AdpcmError::Channels));
        assert!(out.is_empty());
        assert_eq!(ms_adpcm_decode(&[0; 100], 1, 6, std), Err(AdpcmError::BlockAlign));
        assert_eq!(ms_adpcm_decode(&[0; 100], 2, 13, std), Err(AdpcmError::BlockAlign));
        assert_eq!(ms_adpcm_decode(&[0; 100], 0, 64, std), Err(AdpcmError::Channels));
        // a bad predictor in a later block is reported, not skipped
        let mut two = vec![0u8, 16, 0, 0, 0, 0, 0, 0x11];
        two.extend_from_slice(&[9, 16, 0, 0, 0, 0, 0, 0x11]);
        assert_eq!(ms_adpcm_decode(&two, 1, 8, std), Err(AdpcmError::Predictor));
        // empty input is an empty clip; a tail shorter than a header is ignored
        assert_eq!(ms_adpcm_decode(&[], 1, 64, std), Ok(Vec::new()));
        assert_eq!(ms_adpcm_decode(&[0, 1, 2], 1, 64, std), Ok(Vec::new()));
    }

    #[test]
    fn hostile_headers_and_nibbles_never_overflow() {
        // every extreme at once: coefficient pairs at i16::MIN / MAX, samples at the i16 limits,
        // deltas from the most negative to the largest, and long runs of the codes that grow the delta
        let extreme = [[i16::MIN, i16::MIN], [i16::MAX, i16::MAX], [i16::MIN, i16::MAX], [i16::MAX, i16::MIN]];
        let values = [i16::MIN, -1, 0, 1, i16::MAX];
        for pred in 0..extreme.len() as u8 {
            for &delta in &values {
                for &s1 in &values {
                    for &s2 in &values {
                        for fill in [0x77u8, 0x88, 0x78, 0x87, 0x00, 0xFF, 0x4C] {
                            let mut block = vec![pred];
                            block.extend_from_slice(&delta.to_le_bytes());
                            block.extend_from_slice(&s1.to_le_bytes());
                            block.extend_from_slice(&s2.to_le_bytes());
                            block.extend(std::iter::repeat_n(fill, 600));
                            let out = ms_adpcm_decode(&block, 1, 1024, &extreme).unwrap();
                            assert_eq!(out.len(), 2 + 600 * 2);
                        }
                    }
                }
            }
        }
        // stereo with the same abuse
        let mut block = vec![0u8, 1];
        block.extend_from_slice(&i16::MAX.to_le_bytes());
        block.extend_from_slice(&i16::MIN.to_le_bytes());
        block.extend_from_slice(&i16::MIN.to_le_bytes());
        block.extend_from_slice(&i16::MAX.to_le_bytes());
        block.extend_from_slice(&i16::MAX.to_le_bytes());
        block.extend_from_slice(&i16::MIN.to_le_bytes());
        block.extend(std::iter::repeat_n(0x7Fu8, 900));
        assert_eq!(ms_adpcm_decode(&block, 2, 2048, &extreme).unwrap().len(), 2 * (2 + 900));
    }

    #[test]
    fn random_blocks_never_panic() {
        // xorshift64: a cheap deterministic fuzzer over block bytes, sizes, channels and tables
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let len = (next() % 300) as usize;
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let channels = (next() % 4) as u16;
            let align = (next() % 70) as u16;
            let table: Vec<[i16; 2]> = (0..(next() % 9)).map(|_| [next() as i16, next() as i16]).collect();
            let _ = ms_adpcm_decode(&data, channels, align, &table);
            let _ = ms_adpcm_frame_count(next(), channels, align);
        }
    }

    #[test]
    fn plain_pcm_is_passed_through() {
        assert_eq!(decode_pcm(&[0x00, 0x80, 0xFF, 0x7F, 0x01], 1, 16), [i16::MIN, i16::MAX]);
        assert_eq!(decode_pcm(&[0, 128, 255, 1], 1, 8), [-32768, 0, 32512, -32512]);
        // a trailing partial frame (3 samples of a stereo clip) is dropped
        assert_eq!(decode_pcm(&[1, 0, 2, 0, 3, 0], 2, 16), [1, 2]);
    }

    // ---------- the index ----------

    fn two_clip_fmt() -> Vec<u8> {
        let mut fmt = Rec::adpcm(1024, 0).bytes();
        fmt.extend(empty_slot(1024));
        fmt.extend(Rec::adpcm(2858, 1024).bytes());
        fmt
    }

    #[test]
    fn parses_a_valid_index_with_an_empty_slot() {
        let fmt = two_clip_fmt();
        assert_eq!(fmt.len(), 74 + 42 + 74);
        let bank = SfxBank::parse(&fmt, 1024 + 2858).unwrap();
        assert_eq!((bank.len(), bank.clip_count(), bank.is_empty()), (3, 2, false));
        assert_eq!(bank.dat_len(), 3882);
        let first = bank.info(0).unwrap();
        assert_eq!(
            first,
            SfxInfo { index: 0, rate: 44_100, channels: 1, frames: 2036, bytes: 1024, loop_region: None }
        );
        assert_eq!(bank.info(1), None); // the empty slot
        assert_eq!(bank.info(2).unwrap().frames, 2 * 2036 + 2 + (810 - 7) * 2);
        assert_eq!(bank.info(3), None); // past the end
        assert_eq!(bank.decode(0), Err(SoundError::NoDataFile));
    }

    #[test]
    fn an_empty_file_is_a_bank_without_entries() {
        let bank = SfxBank::parse(&[], 0).unwrap();
        assert_eq!((bank.len(), bank.clip_count(), bank.is_empty()), (0, 0, true));
        assert_eq!(bank.info(0), None);
        let mut dat = Cursor::new(Vec::new());
        assert_eq!(bank.decode_from(0, &mut dat), Err(SoundError::NoSuchEntry(0)));
    }

    #[test]
    fn every_truncation_of_an_index_is_an_error_or_a_shorter_bank_never_a_panic() {
        let fmt = two_clip_fmt();
        let boundaries = [0usize, 74, 74 + 42, 74 + 42 + 74];
        for cut in 0..=fmt.len() {
            let result = SfxBank::parse(&fmt[..cut], 1024 + 2858);
            if boundaries.contains(&cut) {
                assert!(result.is_ok(), "cut at the record boundary {cut}");
            } else {
                // the record that was cut is the one the walk was in the middle of
                let record = boundaries.iter().filter(|&&b| b < cut).count() - 1;
                assert_eq!(result.err(), Some(SoundError::Truncated { record }), "cut {cut}");
            }
        }
    }

    #[test]
    fn a_cut_inside_the_adpcm_extra_data_is_truncated() {
        let fmt = Rec::adpcm(1024, 0).bytes();
        assert_eq!(fmt.len(), 74);
        // header (24) + format (18) intact, only 31 of the 32 extra bytes
        assert_eq!(SfxBank::parse(&fmt[..73], 1024).err(), Some(SoundError::Truncated { record: 0 }));
        assert!(SfxBank::parse(&fmt, 1024).is_ok());
    }

    #[test]
    fn a_record_past_the_end_of_the_data_file_is_rejected() {
        let fmt = two_clip_fmt();
        // the last clip ends at 3882: one byte too small a file makes record 2 point past the end
        assert_eq!(SfxBank::parse(&fmt, 3881).err(), Some(SoundError::OutOfRange { record: 2 }));
        assert!(SfxBank::parse(&fmt, 3882).is_ok());
        // an offset and a length that are both near u32::MAX must not overflow the sum
        let wild = Rec::adpcm(u32::MAX, u32::MAX).bytes();
        assert_eq!(SfxBank::parse(&wild, 1 << 32).err(), Some(SoundError::OutOfRange { record: 0 }));
        assert!(SfxBank::parse(&wild, 2 * u64::from(u32::MAX)).is_ok());
    }

    #[test]
    fn an_empty_slot_may_sit_exactly_at_the_end_of_the_data_file() {
        let mut fmt = Rec::adpcm(1024, 0).bytes();
        fmt.extend(empty_slot(1024));
        let bank = SfxBank::parse(&fmt, 1024).unwrap();
        assert_eq!((bank.len(), bank.clip_count()), (2, 1));
        // an empty slot's offset is never read, whatever it holds
        let mut fmt = empty_slot(u32::MAX);
        fmt.extend(Rec::adpcm(10, 0).bytes());
        assert_eq!(SfxBank::parse(&fmt, 10).unwrap().clip_count(), 1);
    }

    #[test]
    fn an_empty_slot_is_42_bytes_whatever_its_format_area_says() {
        // 0xCDCD would be a 52 685-byte cbSize if it were read: the walk must not go there
        let mut fmt = empty_slot(0);
        fmt.extend(empty_slot(0));
        fmt.extend(Rec::adpcm(1024, 0).bytes());
        let bank = SfxBank::parse(&fmt, 1024).unwrap();
        assert_eq!((bank.len(), bank.clip_count()), (3, 1));
        assert!(bank.info(2).is_some());
    }

    #[test]
    fn odd_or_wrong_format_data_is_rejected() {
        let reject = |rec: Rec, record_why: &str| {
            let err = SfxBank::parse(&rec.bytes(), 1 << 20).err();
            match err {
                Some(SoundError::BadFormat { record: 0, why }) => assert!(why.contains(record_why), "{why}"),
                other => panic!("expected a format error mentioning {record_why:?}, got {other:?}"),
            }
        };
        let mut odd = Rec::adpcm(1024, 0);
        odd.extra.push(0); // cbSize 33: not 4 + 4 * 7
        reject(odd, "cbSize");
        let mut short = Rec::adpcm(1024, 0);
        short.extra.truncate(31);
        reject(short, "cbSize");
        let mut none = Rec::adpcm(1024, 0);
        none.extra.clear();
        reject(none, "too short");
        let mut zero_coefs = Rec::adpcm(1024, 0);
        zero_coefs.extra = vec![0xF4, 0x07, 0, 0];
        reject(zero_coefs, "cbSize");
        let mut wrong_spb = Rec::adpcm(1024, 0);
        wrong_spb.extra[0] = 0xF5;
        reject(wrong_spb, "samples per block");
        let mut surround = Rec::adpcm(1024, 0);
        surround.channels = 6;
        reject(surround, "mono and stereo");
        let mut silent = Rec::adpcm(1024, 0);
        silent.channels = 0;
        reject(silent, "mono and stereo");
        let mut no_rate = Rec::adpcm(1024, 0);
        no_rate.rate = 0;
        reject(no_rate, "sample rate");
        let mut fast = Rec::adpcm(1024, 0);
        fast.rate = 4_000_000_000;
        reject(fast, "sample rate");
        let mut mp3 = Rec::adpcm(1024, 0);
        mp3.tag = 0x55;
        reject(mp3, "PCM and Microsoft ADPCM");
        let mut bits = Rec::adpcm(1024, 0);
        bits.bits = 8;
        reject(bits, "4 bits");
        let mut tiny_block = Rec::adpcm(1024, 0);
        tiny_block.align = 6;
        reject(tiny_block, "block alignment");
        let mut odd_pcm = Rec::pcm16(100, 0, 1);
        odd_pcm.bits = 12;
        reject(odd_pcm, "8 or 16");
    }

    #[test]
    fn rejects_an_index_with_too_many_records() {
        let slot = empty_slot(0);
        let mut fmt = Vec::with_capacity(slot.len() * (MAX_ENTRIES + 1));
        for _ in 0..MAX_ENTRIES {
            fmt.extend_from_slice(&slot);
        }
        assert_eq!(SfxBank::parse(&fmt, 0).unwrap().len(), MAX_ENTRIES);
        fmt.extend_from_slice(&slot);
        assert_eq!(SfxBank::parse(&fmt, 0).err(), Some(SoundError::TooManyEntries));
    }

    #[test]
    fn loop_info_is_decoded_frames_and_checked() {
        let mut looped = Rec::adpcm(1024, 0);
        looped.flag = 1;
        looped.loop_start = 2 * 100; // 100 frames, in decoded 16-bit bytes
        looped.loop_end = 2 * 2000;
        let mut stereo = Rec::adpcm(2048, 1024);
        stereo.channels = 2;
        stereo.align = 2048;
        stereo.extra = adpcm_extra(2048, 2);
        stereo.flag = 1;
        stereo.loop_start = 0;
        stereo.loop_end = 4 * 2036; // 4 bytes per stereo frame
        let mut not_flagged = Rec::adpcm(1024, 3072);
        not_flagged.loop_start = 2;
        not_flagged.loop_end = 200; // ignored without the flag
        let mut past_end = Rec::adpcm(1024, 4096);
        past_end.flag = 1;
        past_end.loop_end = 2 * 2037; // one frame beyond the clip
        let mut backwards = Rec::adpcm(1024, 5120);
        backwards.flag = 1;
        backwards.loop_start = 300;
        backwards.loop_end = 100;
        let mut unaligned = Rec::adpcm(1024, 6144);
        unaligned.flag = 1;
        unaligned.loop_end = 201; // not a whole 16-bit frame
        let fmt: Vec<u8> = [looped, stereo, not_flagged, past_end, backwards, unaligned].iter().flat_map(Rec::bytes).collect();
        let bank = SfxBank::parse(&fmt, 7168).unwrap();
        assert_eq!(bank.info(0).unwrap().loop_region, Some(LoopRegion { start: 100, end: 2000 }));
        assert_eq!(bank.info(1).unwrap().loop_region, Some(LoopRegion { start: 0, end: 2036 }));
        for index in 2..6 {
            assert_eq!(bank.info(index).unwrap().loop_region, None, "entry {index}");
        }
    }

    // ---------- decoding clips from a stream ----------

    /// A synthetic bank: entry 0 mono ADPCM sine, entry 1 empty, entry 2 stereo ADPCM, entry 3 PCM16.
    fn synthetic_bank() -> (SfxBank, Vec<u8>, Vec<i16>, Vec<i16>) {
        let mono_pcm = sine(2500, 1, 10_000.0);
        let mono = encode(&mono_pcm, 1, 256);
        let stereo_pcm = sine(1500, 2, 8_000.0);
        let stereo = encode(&stereo_pcm, 2, 512);
        let pcm: Vec<u8> = (0..100i16).flat_map(|v| (v * 100).to_le_bytes()).collect();
        let mut dat = Vec::new();
        let mut fmt = Vec::new();
        let mut looped = Rec::adpcm(mono.len() as u32, 0);
        looped.align = 256;
        looped.extra = adpcm_extra(256, 1);
        looped.flag = 1;
        looped.loop_start = 2 * 500;
        looped.loop_end = 2 * 2000;
        fmt.extend(looped.bytes());
        dat.extend(&mono);
        fmt.extend(empty_slot(dat.len() as u32));
        let mut st = Rec::adpcm(stereo.len() as u32, dat.len() as u32);
        st.channels = 2;
        st.align = 512;
        st.extra = adpcm_extra(512, 2);
        fmt.extend(st.bytes());
        dat.extend(&stereo);
        let mut raw = Rec::pcm16(pcm.len() as u32, dat.len() as u32, 1);
        raw.rate = 22_050;
        fmt.extend(raw.bytes());
        dat.extend(&pcm);
        let bank = SfxBank::parse(&fmt, dat.len() as u64).unwrap();
        (bank, dat, mono_pcm, stereo_pcm)
    }

    #[test]
    fn decodes_clips_from_in_memory_data() {
        let (bank, dat, mono_pcm, stereo_pcm) = synthetic_bank();
        assert_eq!((bank.len(), bank.clip_count()), (4, 3));
        let mut cursor = Cursor::new(dat);
        let mono = bank.decode_from(0, &mut cursor).unwrap();
        let info = bank.info(0).unwrap();
        assert_eq!((mono.rate, mono.channels), (44_100, 1));
        assert_eq!(mono.frames() as u64, info.frames);
        assert!(mono.frames() >= mono_pcm.len() && mono.frames() <= mono_pcm.len() + 1);
        assert_eq!(mono.loop_region, Some(LoopRegion { start: 500, end: 2000 }));
        assert_eq!(mono.loop_region, info.loop_region);
        // the decoded clip follows the source (the encoder is lossy, not wrong)
        let worst = mono_pcm.iter().zip(&mono.samples).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
        assert!(worst < 500, "worst error {worst}");

        // clips are read out of order, straight by offset
        let stereo = bank.decode_from(2, &mut cursor).unwrap();
        assert_eq!((stereo.rate, stereo.channels), (44_100, 2));
        assert_eq!(stereo.samples.len(), 2 * stereo.frames());
        assert!(stereo.frames() >= stereo_pcm.len() / 2);
        assert_eq!(stereo.loop_region, None);
        let pcm = bank.decode_from(3, &mut cursor).unwrap();
        assert_eq!((pcm.rate, pcm.channels, pcm.frames()), (22_050, 1, 100));
        assert_eq!(pcm.samples.get(7), Some(&700));
        let again = bank.decode_from(0, &mut cursor).unwrap();
        assert_eq!(again, mono);
    }

    #[test]
    fn decode_errors_are_specific() {
        let (bank, dat, _, _) = synthetic_bank();
        let mut cursor = Cursor::new(dat.clone());
        assert_eq!(bank.decode_from(1, &mut cursor), Err(SoundError::EmptyEntry(1)));
        assert_eq!(bank.decode_from(4, &mut cursor), Err(SoundError::NoSuchEntry(4)));
        assert_eq!(bank.decode_from(usize::MAX, &mut cursor), Err(SoundError::NoSuchEntry(usize::MAX)));
        // a data file shorter than the index claimed
        let mut short = Cursor::new(dat[..dat.len() - 1].to_vec());
        assert!(bank.decode_from(0, &mut short).is_ok());
        assert_eq!(bank.decode_from(3, &mut short), Err(SoundError::ShortData(3)));
        let mut nothing = Cursor::new(Vec::new());
        assert_eq!(bank.decode_from(0, &mut nothing), Err(SoundError::ShortData(0)));
    }

    #[test]
    fn a_clip_over_the_size_limit_is_refused_before_it_is_read() {
        let big = MAX_CLIP_BYTES + 1;
        let fmt = Rec::adpcm(big, 0).bytes();
        let bank = SfxBank::parse(&fmt, u64::from(big)).unwrap();
        assert_eq!(bank.info(0).unwrap().bytes, big);
        let mut nothing = Cursor::new(Vec::new());
        assert_eq!(bank.decode_from(0, &mut nothing), Err(SoundError::ClipTooLarge(0)));
    }

    #[test]
    fn a_corrupt_clip_reports_its_entry() {
        // a clip whose first block names predictor 9: the index is fine, the data is not
        let fmt = Rec::adpcm(1024, 0).bytes();
        let bank = SfxBank::parse(&fmt, 1024).unwrap();
        let mut dat = vec![0u8; 1024];
        dat[0] = 9;
        let err = bank.decode_from(0, &mut Cursor::new(dat)).err();
        assert_eq!(err, Some(SoundError::Adpcm { entry: 0, source: AdpcmError::Predictor }));
        assert!(err.unwrap().to_string().contains("entry 0"));
    }

    #[test]
    fn open_reads_the_index_and_decodes_from_the_file() {
        let (_, dat, mono_pcm, _) = synthetic_bank();
        let dir = std::env::temp_dir().join(format!("fmv-ff7-sound-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (fmt_path, dat_path) = (dir.join("audio.fmt"), dir.join("audio.dat"));
        // rebuild the index bytes the same way synthetic_bank does (the helper only returns the bank)
        let mono = encode(&mono_pcm, 1, 256);
        let mut rec = Rec::adpcm(mono.len() as u32, 0);
        rec.align = 256;
        rec.extra = adpcm_extra(256, 1);
        std::fs::write(&fmt_path, rec.bytes()).unwrap();
        std::fs::write(&dat_path, &dat).unwrap();
        let bank = SfxBank::open(&fmt_path, &dat_path).unwrap();
        assert_eq!(bank.dat_len(), dat.len() as u64);
        let clip = bank.decode(0).unwrap();
        assert_eq!(clip.frames() as u64, bank.info(0).unwrap().frames);
        assert_eq!(bank.decode(1), Err(SoundError::NoSuchEntry(1)));
        // errors name no path
        let missing = SfxBank::open(&dir.join("missing.fmt"), &dat_path).err().unwrap();
        assert_eq!(missing, SoundError::Io(std::io::ErrorKind::NotFound));
        assert!(!missing.to_string().contains(dir.to_str().unwrap()));
        assert_eq!(SfxBank::open(&fmt_path, &dir.join("missing.dat")).err(), Some(SoundError::Io(std::io::ErrorKind::NotFound)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_refuses_an_oversized_index() {
        let dir = std::env::temp_dir().join(format!("fmv-ff7-sound-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (fmt_path, dat_path) = (dir.join("audio.fmt"), dir.join("audio.dat"));
        std::fs::write(&fmt_path, vec![0u8; MAX_FMT_BYTES as usize + 1]).unwrap();
        std::fs::write(&dat_path, b"").unwrap();
        assert_eq!(SfxBank::open(&fmt_path, &dat_path).err(), Some(SoundError::FmtTooLarge));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn random_index_bytes_never_panic() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let valid = two_clip_fmt();
        for round in 0..2000 {
            let mut data = valid.clone();
            // flip a few bytes of a valid index, or use pure noise
            if round % 4 == 0 {
                data = (0..(next() % 400)).map(|_| next() as u8).collect();
            } else {
                for _ in 0..(1 + next() % 4) {
                    let at = (next() as usize) % data.len();
                    data[at] = next() as u8;
                }
            }
            if let Ok(bank) = SfxBank::parse(&data, 1 << 24) {
                for index in 0..bank.len() {
                    let _ = bank.info(index);
                    let _ = bank.decode_from(index, &mut Cursor::new(vec![0xA5u8; 4096]));
                }
            }
        }
    }

    // ---------- ids ----------

    #[test]
    fn sound_ids_are_one_based() {
        assert_eq!(entry_for_sound_id(0), None);
        assert_eq!(entry_for_sound_id(1), Some(0));
        assert_eq!(entry_for_sound_id(2), Some(1));
        assert_eq!(entry_for_sound_id(750), Some(749));
        assert_eq!(entry_for_sound_id(u16::MAX), Some(usize::from(u16::MAX) - 1));
    }

    #[test]
    fn a_sound_is_found_and_decoded_by_its_id_through_the_one_based_entry() {
        // Ids 1, 2 and 3: a clip, an empty slot and a clip. Each clip is one 7-byte block header, which decodes to the
        // two frames it stores (sample 2 first, then sample 1).
        let mut fmt = Rec::adpcm(7, 0).bytes();
        fmt.extend(empty_slot(7));
        fmt.extend(Rec::adpcm(7, 7).bytes());
        let dat: Vec<u8> = [[0u8, 16, 0, 5, 0, 7, 0], [0u8, 16, 0, 9, 0, 11, 0]].concat();
        let dir = std::env::temp_dir().join(format!("fmv-ff7-sound-by-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (fmt_path, dat_path) = (dir.join("audio.fmt"), dir.join("audio.dat"));
        std::fs::write(&fmt_path, &fmt).unwrap();
        std::fs::write(&dat_path, &dat).unwrap();
        let bank = SfxBank::open(&fmt_path, &dat_path).unwrap();

        assert_eq!(bank.info_for_sound_id(1).map(|i| (i.index, i.frames)), Some((0, 2)));
        assert_eq!(bank.info_for_sound_id(2), None, "id 2 is the empty slot");
        assert_eq!(bank.info_for_sound_id(3).map(|i| i.index), Some(2));
        assert_eq!(bank.info_for_sound_id(0), None, "id 0 is no sound");
        assert_eq!(bank.info_for_sound_id(4), None, "past the end");
        // Decoding by id is decoding the entry before it: id 1 is the first clip, id 3 the second.
        assert_eq!(bank.decode_sound_id(1).map(|c| c.samples), Ok(vec![7, 5]));
        assert_eq!(bank.decode_sound_id(3).map(|c| c.samples), Ok(vec![11, 9]));
        // The bad ids are told apart.
        assert_eq!(bank.decode_sound_id(0), Err(SoundError::NoSoundId));
        assert_eq!(bank.decode_sound_id(2), Err(SoundError::EmptyEntry(1)), "an empty slot is silent, not a failure of the file");
        assert_eq!(bank.decode_sound_id(4), Err(SoundError::NoSuchEntry(3)));
        assert!(SoundError::NoSoundId.to_string().contains("1-based"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn menu_ids_follow_the_evidence() {
        // cursor, buzzer and cancel are ids 1, 3 and 4 (entries 0, 2 and 3); the confirm reuses the cursor click
        assert_eq!([MENU_CURSOR, MENU_BUZZER, MENU_CANCEL], [1, 3, 4]);
        assert_eq!(MENU_CONFIRM, MENU_CURSOR);
        assert_eq!(
            [MENU_CURSOR, MENU_BUZZER, MENU_CANCEL].map(entry_for_sound_id),
            [Some(0), Some(2), Some(3)]
        );
    }
}
