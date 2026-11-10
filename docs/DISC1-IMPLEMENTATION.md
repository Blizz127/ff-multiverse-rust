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

The flower and bridge acceptance results recorded in [POST-REACTOR.md](POST-REACTOR.md)
are earlier checkpoints. The cargo-door/passenger-train changes below have passed the
current interpreter, persistence, TCP and rendering checks listed below. These checks do
not establish complete original-engine visual or timing parity.

| Segment/system | Implementation/evidence now | Work still required |
| --- | --- | --- |
| New-game opening | Original-script initialization survey; new registrations receive kernel gil (240 in owned data) | Faithful first-control story initialization, bounded opening, movies and party/battle boundary |
| First reactor | Reviewed fields and registered scenes, earlier whole-route smoke | Remaining presentation, resource awards and timing fidelity |
| Escape aftermath | Registered md8_4/md8_1 scenes, moment 33 to 36 | Movie parity and ordinary field health/resource persistence |
| Flower encounter | Registered opt-in scene; all five choices, atomic story/gil and earlier TCP/relogin acceptance | Original-engine presentation comparisons and wider mod/body coverage |
| Bridge and cargo arrival | Registered opt-in chain through moment 48; three possible battles, durable recovery and earlier owned/TCP coverage | Movies, remaining field effects and original timing comparisons |
| Cargo door / passenger train | Registered opt-in scenes 48 to 51; owned-data/TCP checks of crossing, durable landing, two windows and final control placement | Dynamic locks and visual/timing limits below |
| Train lesson / Sector 7 station | Authored Jessie/rootmap/departure chain surveyed; station 51 to 63 remains unregistered | Known inputs, field-local state, map presentation, movie 24 and complete linked route |
| Sector 7 / bar | Flag and choice surveys; isolated later bar proof | Ordered flags, affection, actual route, party/menu effects |
| Rest of Disc 1 | No verified continuous route | Chapter/field manifest through Disc 1 end and all required modes |
| Battle system | Deterministic subset; bridge fights carry HP/MP, limits and persistent statuses | Remaining enemy behavior, statuses, summons, limits and original progression fidelity |
| Persistent gameplay | Character progress, inventory and durable scene transactions | Ordinary field HP/MP, canon party state, equipment/materia, shops and remaining scripted resources |
| World traversal, minigames, movies | No complete end-to-end subsystem | Required Disc 1 modes and transitions |
| Graphics | Scrolling camera, optional persisted FXAA and 2x scene rendering; authored background/fade/shake work in current wave | Original-game comparisons and broader field/effect coverage |

## Durable-state foundations and remaining scope

Version 3 stores byte/word aliases, known masks, clears and numeric updates. Literal and bit
assignments establish only the bits they write; unknown numeric inputs still refuse authoritative
completion. Reviewed scene transactions compare exact prior story/resources, retain once-only
receipts and hold the cohort until the queued store acknowledgment. The bridge adds atomic
roster batches and durable battle/landing checkpoints; protocol 20 carries their replay data.
This is not general persistent execution of every field script or automatic save conversion.
Replay contract 2 rejects older contract-1 checkpoints without changing them. Fully known
5,117-byte story banks now fit the three-battle continuation: replay is bounded at 256 KiB,
encoded checkpoints at 2 MiB and character records at 4 MiB. Existing intent limits and
serialization remain unchanged; a separate 256 KiB wire-frame preflight rejects an oversized
initial, accepted or restored replay before it can create an unusable checkpoint.

Field `GOLDd` floors at zero and `GOLDu` saturates at `u32::MAX`, supported by static PSX and
reconstructed PC helper evidence. Ordinary login/resource saves still enforce the battle gil
cap, so the scene host refuses balances outside that cap until those paths preserve field
values. Resource-dependent trigger setup also needs an authoritative resource snapshot before
such scenes can be enabled. Continue expanding ordinary field progression, equipment/materia,
canon-party state and a chapter manifest alongside the curated scenes.

## Current wave: cargo door to passenger-train control

`ff7/cargoin/passenger-door` and `ff7/tin_1/sector-seven-train` append at registry
indices 16 and 17, behind `--story-extended-save`. The door begins at the authored border1
line and uses the accepted crossing position. Cloud's slot 8 requests the door, turns,
waits, plays sound, jumps and hides before `MAPJUMP` to `tin_1` at `(-2,608)`, triangle 29,
direction 252. This zero-window transition leaves moment 48 and resources unchanged. Its
durable landing must survive restart independently of moment 48's ordinary cargo entry.

The train initializes its reviewed environment and nine actors. Ambient mains 9–12, 16 and
20 supply the background mode, camera and shake while foreground employee/director work runs.
Completion waits for foreground requests and their actor tails; ambient work retains ordinary
VM limits and state audits. Two windows precede the authored moment-51 write and Cloud's
control position `(2,537,-53)`, triangle 28, direction 0. Same-field completion must install
that placement before releasing the hold. Connected guests wait for the destination handshake;
their own story and resources remain unchanged.

The original `UC 0` enables control when Cloud appears, while other actors still have work.
The port deliberately holds participants through that remaining tail (about 200 additional
field frames in the port's trace of the owned scripts) and the durable acknowledgment. Participant
bodies and labels stay hidden through the carriage scenes, then return at the committed
control placement. This is a multiplayer/transaction timing difference, not original control
release timing. Dynamic `IDLCK` walkmesh changes and later `LINON` departure gating are not
made complete by this arrival implementation.

### Authored presentation and its limits

* Background commands update group/frame masks in authored order, including quiet setup,
  replayed prefixes and the timed tail. Reviewed cargo/train entry initializers establish the
  baseline; same-field scene completion retains mutations. The cargo door and train door
  frames are reconstructed from original commands, not inferred from a final screenshot.
  This is bounded initialization for these fields, not a general field-runtime replacement.
* `FADE`/`NFADE` and `FADEW` have explicit supported modes and original fixed-point arithmetic.
  The compositor runs in display RGB after scene filtering and before dialogue/HUD. Unsupported
  system-transition modes and invalid durations remain explicit. Visible fades/shake advance
  during dialogue, but the VM does not know the user's elapsed dialogue time: a future `FADEW`
  after a window can wait too long. Current registered waits precede dialogue. Quiet setup fade
  snapshots are not yet exported; current registered initializers have no reachable fade.
* `SHAKE` preserves per-axis phase and random-table indices across supported scenes/field
  changes; all 256 easing values are verified against original field memory. It moves the
  background and models together after camera clamping, keeping dialogue stable. Unexecuted
  ambient history and elapsed dialogue time during reconnect cannot be reconstructed.
* `JUMP` uses the original signed movement-step operand and Q12 trajectory, including landing
  plane and integer remainders. Cargo's operand 12 gives 12 movement advances, a 70.3125-unit
  midpoint arc on level ground, and 14 blocked script frames. The current animation continues
  rather than setting jump duration from clip length. This is pinned PSX implementation
  evidence corroborated by FFNx hooks, not a direct trace of the unmodified PC routine.
* Literal `SOUND` and `AKAO`/`AKAO2` operations 40/41 preserve effect channels 1/2 with
  independent replacement, stop and pending-decode cancellation. This covers cargo sound 233
  and train sounds 59, 167, 427 and 167 in authored order. Higher scene channels remain with
  the existing field-audio path, so train ambient loop 154 is not duplicated. Other sound
  commands and banked operands still need separate support.
* Bridge movie 23 and later train movie 24 remain unplayed. Background/fade/shake support
  does not establish movie, full audio, every body/mod combination or original frame timing parity.
* Cargo's door jump approaches and crosses the camera plane; its large foreground legs follow
  the current projection. The train employee likewise occupies about 214 of 240 field-screen
  units at the second window. Owned camera/model inputs explain these images, but the existing
  base model-scale hypothesis and original PC near-clipping behavior remain unverified. The
  screenshot assertions are not acceptance of those frames as faithful to the original game.

## Fresh-state provenance and the next route dependency

Version 3 records known masks, including known zero, without inventing values for existing
v1/v2 records. Registration now credits only new FF7 characters with the owned kernel's
initial gil; existing zero balances remain zero. Fresh story-bank initialization is still
research, not an implemented migration or registration change in this wave.

The pinned original PSX reset explicitly clears all 1,280 persistent bank bytes, then sets
menu, party and location-buffer exceptions. The owned PC `md1stin` opening assigns affection
bytes 3–6 to 50/30/10/0, bytes 80–83 to 100, word 28 to 763, and finally moment 1. This gives
positive provenance for reset-clear `Var[3][223]` bits 2/4 and first-control affection
`Var[1][4]=30`; absence of an earlier writer or the current moment is not that provenance.
PC source confirms the same bank layout but not its reset routine. One post-reset PSX menu
routine is also absent from the pinned reconstruction. Clocks, location text, counters and
other engine-managed bank bytes need their writers or explicit unknown masks; seeding them
known and invalidating later is not supported by today's monotonic knownness contract.

Jessie's first train talk requires bit 2 known clear, sets it, and transfers to `rootmap`.
The lesson requires bit 4 known clear, then returns to a different train placement and a
three-window continuation sharing scratch state. The departure line requires bit 2 known set;
its slot alone does not repeat that guard. Its ten windows and movie 24 precede the station's
one-window 51 to 63 arrival. Optional Jessie dialogue can subtract from affection, so it also
needs the authored initial value and the field-local once-per-visit latch. None of these
unknown inputs may be supplied as zero merely to unlock the route.

## Verification coverage for this wave

Current results: 1,456 client tests, 561 server tests and 257 persistence tests passed. The
complete FF7 library run passed 661 tests; after the final checked-access lint fixes, all 139
scene units and all 39 owned scene tests passed again. Owned train and bridge TCP runs and
the cargo/train host placement check passed. The renderer checks passed eight owned scene
tests, the background lifecycle test, the fade compositor test, four supersampling tests and
the range-clipping regression. Workspace all-target clippy passes with three pre-existing
warnings. These runs used disposable state; there was no deployment or live-save conversion.

* `scene_real::cargo_door_uses_the_actual_crossing_and_transfers_without_changing_story_or_gil`
  covers the accepted origin, unchanged state, zero windows and exact transfer.
* `scene_real::train_director_finishes_after_its_authored_actor_work` covers setup, two windows,
  the foreground tail, fixed camera and final moment/control pose.
* `train_real::cargo_door_and_train_arrival_keep_their_landing_until_control_is_durably_returned`
  covers the TCP crossing, landing restart, final placement, resources and duplicate/relogin safety.
* Continuation co-op tests cover guest destination handshakes and owner reconnect while the
  landing remains durable; guests retain their own story.
* `scene::shots::train::offscreen_real_ff7_cargo_door_crossing` and
  `offscreen_real_ff7_passenger_train_arrival` cover original models, jump samples, camera/shake,
  background frames, full actor tail and body handoff.
* `scene::fade::tests::offscreen_authored_fade_composites_display_rgb_after_scene_filters_and_before_ui`
  covers GPU formats, filtering order, resize and clearing. Background, fade, shake and jump
  unit tests cover arithmetic, ordering, replay and unsupported inputs.

Owned-data tests use `FMV_FF7_DATA` and `--ignored`; offscreen tests also require a GPU or
software driver. Temporary-store tests do not authorize changes to owned saves. The existing
`scripts/story-smoke.sh` covers moment 33 to 36, not the new chain or complete Disc 1.
