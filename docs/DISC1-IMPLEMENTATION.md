# FF7 implementation and Disc 1 completion

Owner objective, 2026-11-10: implement the complete FF7 game using this framework,
prioritize all of Disc 1, preserve original behavior apart from the explicit multiplayer
and port changes, and add optional modern graphics. This is the active implementation
scope. The first-milestone plan is historical; completing its reactor slice does not
complete this objective.

## Evidence and acceptance

Use the original owned field scripts as gameplay authority. Modded dialogue/presentation
must demonstrate equivalent state changes before substitution: the current surveys find
different story moments in the original and Echo-S openings and Sector 7 introduction.
Unknown values remain unknown. A passing synthetic test or a curated scene replay is
not proof of an entire chapter or original-engine parity.

A completed segment must support entry from its predecessor, all authored choices and
required interactions, resource/party changes, battles, exits, save/relogin, and required
presentation. Verify on disposable state with the original assets, and compare against
the original game at matching checkpoints. Record missing or inferred behavior rather
than calling it accurate. Existing owned installs and saves are read-only inputs.

## Current coverage

| Segment/system | Evidence now | Work still required |
| --- | --- | --- |
| New-game opening | Diagnostic original-script execution | Bounded opening, authored initialization, movies, party and battle boundary |
| First reactor | Reviewed fields and registered scenes, whole-route smoke | Remaining presentation, resource awards and timing fidelity |
| Escape aftermath | Registered md8_4/md8_1 scenes, moment 33 to 36 | Movie parity and carried health/resource state |
| Flower encounter | Unregistered md8_2 definition; all five original answer paths evaluated with explicit gil and known-bit writes | Framing, actual linked route, initial character record and route/relogin acceptance |
| Bridge and cargo train | Surveyed exits; unregistered cargo scene 39 to 48 | Linked traversal, reviewed arrivals and complete field effects |
| Train / Sector 7 station | Unregistered scenes 48 to 51 to 63 | Route integration, presentation and skipped effects |
| Sector 7 / bar | Flag and choice surveys; isolated later bar proof | Ordered flags, affection, actual route, party/menu effects |
| Rest of Disc 1 | No verified continuous route | Chapter/field manifest through Disc 1 end and all required modes |
| Battle system | Deterministic implemented subset | Required enemy behavior without fallbacks, remaining statuses, summons, limits and full progression |
| Persistent gameplay | Account character progress and inventory | Canon party state, carried HP/MP, equipment/materia, shops and scripted resources |
| World traversal, minigames, movies | No complete end-to-end subsystem | Required Disc 1 modes and transitions |
| Graphics | Field/model rendering, scrolling camera and optional persisted FXAA before UI | Additional optional quality settings and original-game visual comparisons |

## Work in the current increment

* Version 3 story banks carry known masks and values, including known zeroes, clears and
  numeric state. Byte/word aliases share storage. Legacy v1/v2 blobs remain unchanged
  unless explicitly promoted; newly missing choices are never inferred. Extended writes
  require exact predecessor comparison, not the old monotonic merge.
* The scene VM preserves assigned masks and does not count literal assignment as a read
  of its previous value. A bit assignment leaves neighbouring unknown bits unknown.
  Numeric reads and invalid word boundaries still prevent authoritative completion.
* A recoverable two-record scene transaction and durable receipts carry exact story and
  gil before/after values. The world holds participants until the queued commit acknowledges,
  then updates mirrors and releases/travels them. Duplicate completion, lost acknowledgements,
  reconnect snapshots, evicted destinations and uncertain store failures are checked explicitly.
* Protocol 19 includes the owner's starting gil in `SceneStart`. Every participant and both
  original/replacement script comparisons use the same snapshot. `GOLDu`, `GOLDd` and `CHGLD`
  preserve ordered resource effects; unavailable values refuse authoritative completion.
  Original static PSX and reconstructed PC helper evidence agrees on unsigned saturation:
  subtraction floors at zero, addition caps at `u32::MAX`. The flower's purchase script has
  no affordability branch. This is static source evidence, not execution of the owner's PC binary.
* The owned kernel supplies initial gil through a bounded accessor (240 in the verified
  English data). This increment does not credit existing characters or change new-character
  creation. Original initialization and the complete opening still need to be connected.
* Optional FXAA runs before the UI, with F3 switching modes and settings saved separately
  from game progress. The existing unfiltered path remains the default.

These are foundations, not an enabled flower encounter. Version 3 scene completion uses
acknowledged transactions only with the extended-save option and a resource snapshot.
Legacy field-point progression refuses version 3 until it gains its own exact transaction
path, because the old merge cannot save it. No automatic live conversion or new-game default
initialization is part of this increment.

## Next integration steps

1. Connect fresh-character initialization and exact progression for ordinary field points,
   battles and interactions. Missing character records must be created deliberately before
   transactions need them. Existing blobs and unknown choices must remain unchanged.
   The original md1stin prefix assigns affection bytes 3–6 to 50/30/10/0, bytes 80–83 to
   100 and word 28 to 763; its unresolved movie/battle tail is not safe initialization.
2. Enable and verify all five flower paths, then bridge/train/Sector 7 as continuous
   progression. Flower flag bytes 3/66 and 3/111 are bit-write outputs, so unknown inputs do
   not need invented zeroes for this scene. Extend relogin coverage at every durable boundary.
3. Verify camera/witness/animation behavior on the actual route. Interpretation and save
   tests alone do not establish presentation fidelity.
4. Separate field-resource storage limits from battle reward caps before enabling general
   large-value gil effects: login and ordinary saves currently clamp balances to the battle cap.
   The FF7 host refuses commits whose starting or resulting gil exceeds that cap until
   those paths preserve field values; interpreter arithmetic remains the original unsigned math.
   Invalid selectors and word accesses across byte 255 remain unsupported and refuse saves.
   Trigger placement still evaluates setup without a resource snapshot; resource-dependent
   triggers need the same authoritative input before those scenes can be registered.
5. Expand a Disc 1 chapter manifest, implementing general field execution and missing
   gameplay systems alongside the route instead of treating curated dialogue as the game.

## Relevant verification

* `cargo test -p fmv-ff7 --lib`: legacy and extended state, VM and field rules.
* `cargo test -p fmv-persist`: temporary-store persistence, interruption recovery,
  coherent reads, receipts, backup and delete/restore.
* `cargo test -p fmv-server --lib`: authoritative queue/resource handling.
* `cargo test -p fmv-client --bin fmv-client`: UI, input, settings and client behavior.
* Owned-data `scene_real` tests `extended_opening_retains_authored_defaults_at_the_first_window_including_zero`
  and `extended_flower_paths_retain_each_authored_flag_without_guessing_other_bits`
  exercise the new representation against original scripts. These require `FMV_FF7_DATA`
  and `--ignored`; they do not enable scenes or write player saves.
* `scene_host::tests::owned_flower_choices_produce_exact_durable_resource_results` evaluates
  all five choices at 0, 1 and 600 gil against the owned field, commits to temporary stores,
  reopens them and checks idempotent completion. The scene remains unregistered.
* The ignored `offscreen_fxaa` GPU checks cover actual rendering. Pixel checks do not
  replace original-game comparisons or inspection on the owner's display.

The current `scripts/story-smoke.sh` covers moment 33 to 36, not the flower transaction
or Disc 1. Broader completion requires broader evidence.
