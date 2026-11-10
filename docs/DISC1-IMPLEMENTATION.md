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
| Flower encounter | Unregistered md8_2 definition; five surveyed answer paths | Known inputs, gil transaction, durable completion, framing, actual route/relogin |
| Bridge and cargo train | Surveyed exits; unregistered cargo scene 39 to 48 | Linked traversal, reviewed arrivals and complete field effects |
| Train / Sector 7 station | Unregistered scenes 48 to 51 to 63 | Route integration, presentation and skipped effects |
| Sector 7 / bar | Flag and choice surveys; isolated later bar proof | Ordered flags, affection, actual route, party/menu effects |
| Rest of Disc 1 | No verified continuous route | Chapter/field manifest through Disc 1 end and all required modes |
| Battle system | Deterministic implemented subset | Required enemy behavior without fallbacks, remaining statuses, summons, limits and full progression |
| Persistent gameplay | Account character progress and inventory | Canon party state, carried HP/MP, equipment/materia, shops and scripted resources |
| World traversal, minigames, movies | No complete end-to-end subsystem | Required Disc 1 modes and transitions |
| Graphics | Field/model rendering and scrolling camera | Optional quality settings with original presentation retained; visual comparisons |

## Work in the current increment

* Version 3 story banks carry known masks and values, including known zeroes, clears and
  numeric state. Byte/word aliases share storage. Legacy v1/v2 blobs remain unchanged
  unless explicitly promoted; newly missing choices are never inferred. Extended writes
  require exact predecessor comparison, not the old monotonic merge.
* The scene VM preserves assigned masks and does not count literal assignment as a read
  of its previous value. A bit assignment leaves neighbouring unknown bits unknown.
  Numeric reads and invalid word boundaries still prevent authoritative completion.
* The store gains a recoverable two-record scene transaction and durable receipts. A
  queued scene save must finish earlier rewards, verify its prior story and funds, and
  acknowledge durable completion before gameplay may publish its result.
* Optional FXAA runs before the UI, with F3 switching modes and settings saved separately
  from game progress. The existing unfiltered path remains the default.

These are foundations, not an enabled flower encounter. The legacy live scene-completion
path refuses version 3 until it is connected to the acknowledged transaction path. No
automatic live conversion or new-game default initialization is part of this increment.

## Next integration steps

1. Record and verify initialization for every persistent input needed by the next route.
   The original md1stin director explicitly assigns affection bytes 3–6 to 50/30/10/0,
   bytes 80–83 to 100, and word 28 to 763 before its first window. This does not prove
   defaults for the flower or Sector 7 flags. Its later unresolved battle tail must not
   be treated as initialization.
2. Interpret gil reads and writes with authoritative resource input; preserve exact
   ordered operations and insufficient-funds behavior from the original game.
3. Hold scene participants while the durable save runs. Only an acknowledged commit
   updates the in-memory story/gil, broadcasts success and releases/travels the party.
   Cover disconnect, duplicate completion, failed writes and recovery before enabling.
4. Enable and verify all five flower paths, then bridge/train/Sector 7 as a continuous
   progression. Extend relogin coverage at every new durable boundary.
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
* The ignored `offscreen_fxaa` GPU checks cover actual rendering. Pixel checks do not
  replace original-game comparisons or inspection on the owner's display.

The current `scripts/story-smoke.sh` covers moment 33 to 36, not the flower transaction
or Disc 1. Broader completion requires broader evidence.
