# Post-reactor route work

## Current implementation status, 2026-11-10

The two scenes the escape ends in, `ff7/md8_4/post-escape` and
`ff7/md8_1/post-escape-station`, are registered, start by themselves for a player at moment 33,
and take the story to 36 (README, "After the escape"; `scripts/story-smoke.sh`). The street
scene's party change is carried by the party table's row of moment 36, its full restore is a
no-op here (no HP or MP is kept between battles), and both play in Echo-S for a player alone.

**The flower encounter is now registered, with saving opt-in.** Its stable scene ID is
`ff7/md8_2/flower`. With `--story-extended-save`, a player entering `md8_2` at moment 36 is
offered the director scene, whose five answer paths finish at moment 39. Without that option,
the server does not offer the flower encounter. The original script's purchase path executes
`GOLDd 1` with saturating subtraction; a zero balance stays zero, as the instruction specifies.

The flower's stable identity enables preparation of a legacy story as an extended v3
snapshot for interpretation. Preparation is temporary: it preserves unknown bits and does not
write the save. On accepted completion, the server commits the exact story and gil result in
one durable, once-only transaction against the original stored story and balance. Every path
makes `Var[3][66]` bit 0 known set; purchase also makes `Var[3][111]` bit 0 known set. A path
that does not write a bit preserves its previous value and knownness, including unknownness;
it does not infer a clear bit from the story moment. Unrelated scenes retain their prior
preparation behavior and still require their own reviewed inputs, effects and route integration.

`STORY_ENTRIES` now returns a completed moment-39 character to `md8_2` at Cloud's authored
final position `(-3989,21138)`, triangle 19, direction 150; the flower scene is no longer
offered there. New FF7 character registration takes its initial gil from the owned kernel's
initial-state section (240 in the verified owned data) and persists it at registration.
Existing balances, including legacy zero balances, are preserved rather than recredited.

**The exact TCP regression passed.**
`all_flower_choices_walk_the_real_gateway_save_once_and_survive_server_restart` in
`crates/fmv-server/tests/flower_real.rs` passed all five paths at balances 0, 1 and 600,
walking the actual `md8_1`→`md8_2` gateway, repeating completion, restarting the server and
relogging at moment 39. Both saved files remain byte-identical through relogin; unrelated
character fields remain intact. One purchase starts from an empty debug story, verifying that
its debug baseline reaches the FIFO saver before the exact scene transaction.

Five missing, invalid or trailing answer transcripts were refused without changing either
save file. Pressing Back before answering cancels as `Aborted`, without a purchase or storage
error. Durable evaluation requires every authored choice on the selected path; only preview
and survey evaluation retains cursor defaults and answer clamping.

The registered client path rendered all five choices with zero skipped animations. Cloud's
full standing loops support the scene's normal/doubled rates and phase-preserving reset; hidden
cast NPCs no longer leave their old labels behind. The three witness positions are clear during
dialogue and never obscure the cast. Cloud briefly passes in front of two witnesses during his
entrance, a reviewed depth occlusion. The existing route-camera regression also passes with the
flower scene. These are offscreen checks on the original owned models; original PC frame timing
and every mod/body combination are not established.

Validation at the flower checkpoint: 1,387 client tests, 607 FF7 library tests, 504 server tests,
231 persistence tests, all 37 owned-data scene tests, the owned flower host checks and the two
existing owned server preview/gateway regressions passed. All-target clippy completes with two
previously existing warnings. The implementation status here records no deployment or bulk
migration of existing saves.

**Bridge and cargo arrival are now registered behind `--story-extended-save`.** The stable IDs
are `ff7/md8brdg/confrontation` and `ff7/cargoin/train-survey` (the cargo name is retained for
compatibility), appended at registry indices 14 and 15. The bridge starts at its original
entity-4 line, using the player's accepted crossing position and facing. Strict replay checks
every window and all three questions against the script fingerprint; all eight answer paths
select the original subset of formations 328, 329 and 330. The bridge keeps moment 39, then its
scripted jump starts cargo's 26-window arrival and commits moment 48. Cargo's authored arrival
`(-24,16)`, triangle 20, replaces the bridge jump's zero placeholder. Moment-48 relogin uses
that same position with direction 0.

Each battle preparation, settlement and pending landing is durable. The saved replay binds
the source, accepted dialogue, battle identities, exact setup/seed and frozen participant
roster. One atomic batch applies costs, rewards and checkpoint changes for the roster before
play resumes. Guests keep their own story. Recovery handles a prepared fight, a settled
victory, and the bridge-to-cargo landing independently of the moment-based entry table.
Defeat retains a Game Over checkpoint and rolls back that attempt's staged costs; Confirm
retries, while Back disconnects and preserves the checkpoint. Held cohorts cannot mutate
their party until release. New writes now require account-store compatibility version 4
and detached archive manifest version 2; older binaries refuse those formats. Both ends
of the connection require protocol 22. Replay contract 3 includes authored model offsets,
palette operations and corrected byte extraction; older checkpoints are refused unchanged.

Consecutive fights carry HP, MP, limits, knockout and persistent Sadness/Fury. Mode 8 forbids
escape. The actual MP formations and their reachable AI were checked against owned data;
no unsupported AI instructions were needed. Current HP/MP is preserved through a player
level-up and companion levels stay fixed for the chain: these are explicit port policies,
not proven original active-party leveling behavior. Ordinary field health persistence is
still absent.

All eight bridge paths and the cargo arrival pass owned-data interpreter and offscreen
presentation checks, with no skipped actor animations or camera misses. The cargo hides
participant bodies/labels until durable completion, and field geometry is clipped to the
camera range so guards do not appear in side bars. At that bridge acceptance checkpoint,
these checks did not establish movie, fade,
background-opcode or all audio parity. The current wave below adds authored background,
fade and shake support; movie 23 and remaining audiovisual limits are still open.
No live-store migration or deployment was performed.

Bridge acceptance now includes the real TCP route in `tests/bridge_real.rs`: both the no-fight
path `[3,1,1]` and all-three-fight path `[2,0,0]` pass, with one actual Potion consumed on the
latter. It checks carried resources through 328/329/330, the pincer opening, escape refusal,
restarts before combat, after a victory and at the cargo landing, then all 26 cargo windows
and byte-identical final relogin without repeated rewards. Separate lifecycle tests cover
co-op reservations, defeat, acknowledgment races and dropped sessions; persistence recovery
passes 42 injected crash cases. Final suites pass 552 server tests, 254 persistence tests,
1,417 client tests and 89 protocol tests (including the owned battle chain). All workspace
targets compile; clippy retains only the previously existing FF7/client warnings.

### Current wave: cargo door and passenger-train arrival, 48 to 51

**Implemented and checked through moment 51, with the fidelity limits below.** `ff7/cargoin/passenger-door` and
`ff7/tin_1/sector-seven-train` append at registry indices 16 and 17 and require
`--story-extended-save`. This wave passes 1,456 client, 561 server and 257 persistence tests;
the full FF7 library run passes 661, followed by 139 scene-unit and all 39 owned-scene checks
after the final lint-only edits. The owned train TCP, host placement and bridge TCP regressions
pass, as do the new scene/effect/background and existing graphics regressions. Workspace
all-target clippy passes with three existing warnings. No live saves or deployment changed.

Cargo's border1 entity 10 slot 2 starts at line `(-15,-104,0)` to `(53,-104,0)` after the
moment-48 arrival. The scene binds the accepted crossing position and follows Cloud 11/8
and the original door request. After `WAIT 15`, Cloud jumps to `(9,-247)`, triangle 0,
then hides. `MAPJUMP 139` transfers to `tin_1` at `(-2,608)`, triangle 29, direction 252.
There are no windows, questions, battles, story writes or gil effects in this connecting
sequence. Its durable landing retains `tin_1` across restart even though the unchanged
moment 48 normally returns a character to cargo.

The train definition initializes environment entities 0–24 and its nine cast members,
including the woman's original initializer. The employee's placement comes from his main.
Reviewed ambient mains 9–12, 16 and 20 run beside the foreground employee/director work;
producer 16 selects the active background mode, while main 20 supplies `SCR2D(18,2)` and
shake. These use the ordinary interpreter and safety audit. Unrelated timer/lesson mains
are not started. Foreground actor requests and their tails must finish before completion;
a moment-51 write alone is not the completion boundary.

The two windows precede Cloud's placement `(2,537,-53)`, triangle 28, direction 0, and
moment 51. The same-field durable completion installs that pose before releasing control;
it must not leave players at the incoming `(-2,608)` landing. Guest story/resources remain
their own, connected guests wait for destination handshakes, and reconnect refreshes their
retained participant identities. Participant bodies/labels stay hidden through cargo arrival,
door crossing and train introduction; unrelated players remain visible.

**Explicit timing difference:** original `UC 0` releases Cloud when he appears, before the
remaining actor work ends. The port holds the cohort for that tail (about 200 more field
frames in the port's trace of the owned scripts) and the durable acknowledgment. Actors and script
windows also retain the port's scene-wide dialogue pause. This is not original input-release
or full frame-timing parity. Dynamic `IDLCK` triangle locks are not yet part of authoritative
field movement. The surveyed final triangle 28 connects to departure triangle 43 even with
the observed locks, but floor connectivity does not establish that a story-gated exit is open.

The cargo jump's authored path approaches and crosses the camera; its apex mesh spans camera
depths about 15.9 to 65.0. The current projection therefore produces large foreground legs.
The train employee at the second window stays in front of the camera, occupying about 213.5
of 240 screen units. These measurements explain the rendered geometry but do not prove native
near clipping or the existing base model-scale hypothesis. Those frames remain a visual-fidelity
gap rather than being accepted solely because their automated assertions pass.

### Background, fade, shake and jump presentation in this wave

Literal `SOUND` and `AKAO`/`AKAO2` operations 40/41 now retain channels 1/2 through the
scene player and mixer. Cargo's door sound 233 uses channel 2; the train's authored sequence
59, 167, 427, 167 uses channel 1. Each channel has independent cancellation and pending
decode state and replaces only its own field loop. Train ambient loop 154 remains on field
channel 3; higher scene channels, other commands and banked operands are not enabled here.

Authored `BGON`, `BGOFF`, `BGROL`, `BGROL2` and `BGCLR` update the backdrop's group masks.
Quiet setup, replayed prefixes and timed commands retain their order, and same-field scene
teardown preserves the resulting background. Reviewed entry initialization for `cargoin` and
`tin_1` reads literal original initializer commands and refuses an unexpected script layout.
Cargo's main door opens with `BGOFF(2,0)`; the passenger crossing later enables group 8 frame 2,
which changes box occlusion. Train group 3 follows the authored closed/open/closed sequence.
These are distinct effects; the box change must not be presented as the door-opening operation.

Supported `FADE`/`NFADE` modes and `FADEW` use the pinned original fixed-point state machine.
The GPU composites in display RGB after scene filters and before dialogue/HUD, including sRGB
and unorm targets. System-transition modes 3/13 and unsupported durations remain explicit.
The visible client advances fade/shake during dialogue, but the VM lacks user-dialogue elapsed
time, so a future `FADEW` after a window can wait too long. Current registered waits occur
before dialogue. Quiet setup does not yet export fade snapshots; current registered setup
paths contain no reachable fade. A terminal map-jump fade survives scene teardown until the
actual field change; an abort clears it.

`SHAKE` moves the backdrop and models together after camera clamping/rounding, leaving dialogue
stable. It preserves each axis's phase and private random-table index across supported scenes
and field changes; all 256 easing values have original field-memory evidence. It does not
reconstruct unexecuted ambient history or elapsed dialogue time after reconnect. The train's
scripted camera therefore stays centered on the clamped view equivalent of `(-18,2)`, with
shake added separately, rather than following a witness or Cloud.

`JUMP` now interprets the signed operand as movement steps, retains the current animation,
and follows the original Q12 trajectory and landing-triangle plane. Cargo's 12 movement
steps give a 70.3125-unit midpoint rise on level ground; initialization/completion make the
script wait 14 frames. Integer endpoint remainders are retained rather than snapped away.
This behavior is proved by the pinned PSX implementation and corroborated by FFNx's frame-rate
hooks, not by executing the unmodified PC routine. Invalid/unresolved jumps are refused rather
than replaced with a guessed linear motion. Bridge movie 23 and later departure movie 24
remain unplayed; these effects do not prove every sound, mod/body or original timing path.

Coverage written for this wave, **with final combined results pending**:

* `scene_real::cargo_door_uses_the_actual_crossing_and_transfers_without_changing_story_or_gil`
  and `train_director_finishes_after_its_authored_actor_work`: accepted origin, exact transfer,
  setup, two windows, full tail, camera and final state/pose.
* `train_real::cargo_door_and_train_arrival_keep_their_landing_until_control_is_durably_returned`:
  real TCP crossing, restart at the pending landing, unchanged resources, durable moment 51,
  correct live placement and duplicate/relogin safety.
* Continuation tests `a_door_guest_waits_for_shared_arrival_then_returns_to_control_without_a_story_write`
  and `an_owner_rejoining_after_the_arrival_commit_refreshes_the_retained_guests_participants`:
  co-op handshakes, holds and reconnect identities.
* `scene::shots::train::offscreen_real_ff7_cargo_door_crossing` and
  `offscreen_real_ff7_passenger_train_arrival`: original models, sampled jump arc/animation,
  authored background frames, camera/shake, full tail and post-ack body handoff.
* `scene::fade::tests::offscreen_authored_fade_composites_display_rgb_after_scene_filters_and_before_ui`
  and the background/fade/shake/jump unit tests: GPU ordering and arithmetic, setup/replay
  retention, unsupported operands and late-event timing.

### Next route gap and initialization provenance

The next complete segment is Jessie's train-map lesson, its return conversation, and the
scripted departure to the Sector 7 station; the station then advances 51 to 63. Pillar,
Sector 7 intro and early bar continuations also remain unregistered. The detailed audit below
records their authored triggers, not permission to skip them.

Jessie's first talk needs `Var[3][223]` bit 2 known clear, sets it, and jumps to `rootmap`.
The lesson path needs bit 4 known clear. On return, the train's director and Jessie share
scratch state for three windows and control release. The departure line's main enables it
only with bit 2 set; calling its slot directly bypasses that guard. The station jump is
preceded by ten windows and movie 24. Later Jessie choices can subtract from `Var[1][4]`
and rely on a field-local latch, so neither affection nor repeat-talk behavior is supplied
by the moment alone.

A new-game provenance audit establishes a positive reset baseline: the pinned original PSX
routine clears all 1,280 persistent bytes, then applies menu, party and location-buffer
exceptions. Its new-game routine sets disc 1. The owned PC opening explicitly assigns
`Var[1][3..6] = 50,30,10,0`, bytes 80–83 to 100, word 28 to 763 and then moment 1. All 12
owned `md1stin` initializers add no other persistent script assignment. Thus reset-clear
`Var[3][223]` bits 2/4 and first-control affection 30 have evidence independent of the current
moment or an absence of writers. See the [pinned reset implementation](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/cd.c#L39).

This is **not yet a fresh-story registration implementation**. PC source corroborates the
same bank layout, but the PC reset itself is unverified; a post-reset PSX menu routine is
also missing from the pinned reconstruction. Engine-managed clocks, location text, counters,
party mirrors and other derived bytes need authoritative writers or explicit unknown masks.
Current exact story replacement cannot remove knownness later. No existing/legacy unknown
record may receive reset defaults, and no migration or deployment belongs to this wave.

## Historical survey notes

The following notes preserve earlier checkpoints and their evidence limits. Statements that
the flower, bridge, cargo arrival, cargo door or passenger-train arrival was unregistered,
their continuations were missing, or the story model could not carry these bits describe
those checkpoints, not the current implementation above. Older notes on
`md8_4`/`md8_1` likewise predate their registration. The later scenes' unresolved work remains
open unless explicitly updated in the current status or the dated continuous-route audit.

On the owned English Steam data, the escape ends at story moment 33. The `md8_4` director's
main script runs four windows and jumps to field 133 (`md8_1`), at (1629, -5357), triangle 15,
direction 192. The `md8_1` director runs four windows and writes moment 36. These are pinned by
the ignored real-data tests in `crates/fmv-ff7/tests/scene_real.rs`. The interpreter still skips
the station movie, sound and party operations; those tests do not prove presentation parity.

The next authored gateways go `md8_1` → `md8_2` → `md8_3` → `md8brdg`; the director in
`md8_2` writes moment 39, and `md8brdg` contains a script jump to `cargoin`. These are survey
findings, not a tested route. `cargoin` contains a write to moment 48. The bar scene proof is at
moment 72 in `mds7pb_1`; it is not reachable from the escape in this branch yet.

The `md8_2` director has branching questions and one branch removes one gil (`GOLDd`) before
writing moment 39. At this survey checkpoint, the server-owned, once-only transaction and its
persistence were still required before serving that branch; a presentation-only replay must
not alter currency.
An isolated survey of the director's sampled choice paths found every path sets
`Var[3][66]` bit 0; purchase paths also set `Var[3][111]` bit 0 and execute `GOLDd 1`.
Neither flag was carried at that checkpoint. These effects belong in the same validated durable
result as the gil and moment changes; see `STATION-TRANSACTION-PROPOSAL.md`.
The then-unregistered `flower_station_scene()` and owned-data check exercised the short path and
all four combinations of the later two choices. A local original/Echo-S comparison aligned answers
by position: sampled paths agree on the `GOLDd` occurrence, moment 39, and persistent flag values,
but Echo-S has more windows and different choice line numbers. These observations do not prove
choice meaning, voice playback, safe persistence, or multiplayer synchronization.
The sampled Echo-S section's `GOLDd` instruction also has the literal amount 1; the scene VM
did not execute that debit at this checkpoint.
An offscreen short-path exploration tested several possible witness spots. Some were hidden by
background or scene actors despite large walkmesh separation. At `(-4055, 21419, triangle 6)`
the witness was visible in the inspected samples, and 100 sampled positions kept it at least
212.4 units from visible cast actors (ring threshold 44). The run ended unskipped, but other
choice paths, every rendered frame, full screen occlusion and other models remain unchecked;
the spot is not accepted for live use.
The longer sampled purchase path also ended unskipped with a 212.4-unit minimum across 104
position samples, but an inspected frame showed the witness partly covered by scene actors.
That candidate therefore remains unsuitable even though its walkmesh separation was ample.
Another candidate `(-3785, 20901, triangle 31)` was visible in inspected frames of the short
and one purchase path. The short path and all four later-choice combinations ended unskipped;
the closest visible actor was 203.3 units away in 100–105 position samples per run. Full visual
occlusion, all model/path combinations and the actual route are still unverified, so this is not an accepted
live spot. Two other placeholder bodies were also rendered on one purchase path and were
visible in the inspected samples; this was not an all-frame or Ninostyle-model check.
The `md8_1` director also runs `PRTYM` for character 1 (Barret) and `HMPMAX3`; the current
story party table otherwise continues to offer Cloud and Barret at later moments. Those
operations and the correct later party must be modeled before this scene advances real progress.

The branch has an arrival-scene trigger and a scene jump through the ordinary area handshake.
Scene jumps only resolve to reviewed fields, so `md8_1` remains closed until its transition and
story are ready. The server refuses to apply incomplete interpretations or ignored party, resource,
inventory, clock and menu operations, and preflights a destination before saving a scene's story change. Debug previews never
save or perform a scene jump. An automatic scene is judged and saved against the entering player's
own story, even when its field uses the party leader's phase. A member whose story has not reached
the scene's moment is not advanced or teleported; how such a follower catches up remains unfinished.
Both bar answers change additional untracked bits of `Var[3][212]`; the long answer also changes
Tifa's affection (`Var[1][4]`). The current story model carries neither effect. A real scene must
refuse those writes instead of silently discarding them; debug
preview can still show it on made-up state. Safely carrying affection needs its own reviewed
persistence design and owner decision under the existing data gate.
There is a separate read-side gap: `SceneVars` supplies `Given` values or zero for bytes
the saved story does not carry. The VM can branch on those assumed values, yet the server's
current completeness check records unknown opcodes and unresolved positions, not unknown
persistent variable reads. A run with no discarded writes can therefore still choose the wrong
branch. In particular, the bar preview assumes bits in `Var[3][211]` and `Var[3][212]`;
those assumptions are useful for a preview but are not evidence of an individual player's
state. Keep the post-reactor scenes unregistered until their decision inputs and effects are
modeled and verified against saved state.

The initial read guard is deliberately conservative: it records every baseline read of a
persistent byte unless the entire byte is carried by the story, and refuses a real outcome
if any such read occurred. It also sees inputs copied into scratch variables and reads from
setup. `Given` remains preview context, not authoritative saved data. The guard may reject
safe runs: even a known bit of a partially tracked byte, and an overwrite whose old value was
consulted internally, can be flagged. Bit-level knownness and propagation can later reduce
those false refusals; they must not weaken the conservative guard. This does not establish that
party state, unimplemented effects or every other interpretation gap is correct, and changes
neither the live story format nor authorization to migrate player data.
The existing story merge unions tracked bits and is not a numeric affection merge. The correct
initial affection and the behavior of later increases/decreases must be established from owned
game data before proposing a durable numeric representation; treating the observed `+5` as a
complete value or simply OR-ing bytes would be incorrect.
An owner whose client cannot build or finish the interpreter run aborts without saving; a timed-out
run also leaves progress untouched rather than silently choosing any remaining answers.
The server also checks the setup interpreter's unknown instructions, unresolved positions,
dropped requests and ignored stateful operations before accepting an outcome. Setup reports
when it stops at a budget or an unexpected scene end rather than silently accepting partial state.
In owned bar data, asking the host to replay a preview against a story with the bar mark already
set runs an actor setup loop until that budget and is refused; normal offering already excludes
the marked story. A fresh preview remains available.
The read guard above remains conservative, and these checks do not establish correct party state.
Echo-S uses a
rewritten field script with different entity and window numbering, so scene presentation and
watcher synchronization must be reconciled before claiming that all scene dialogue and voices use it.
`SceneDef::rebind_entities` maps uniquely named entities into a replacement section, refusing
missing or ambiguous names; entity identity alone does not prove
the replacement's slots, choices, windows or timing are compatible.

The verified Echo-S 1.24 archive was compared locally against the owned English field data with
`fmv-mods`' `echo_scene_compare` example. At the same story moments, `md8_4` produces four windows
in both layers, while `md8_1` produces four original windows and five Echo-S windows. At the bar,
the original's two answer paths produce six and eleven windows; Echo-S produces seven and twelve.
The original bar question uses absolute lines 1–2, while Echo-S uses 0–1. Comparing answer ordinal
(first with first, second with second) gives the same tracked story bits on the sampled paths; this
does not establish semantic equivalence of every choice or a safe watcher protocol. The bar script
also capitalizes actor names differently, so identity matching must be case-insensitive and reject
case-insensitive duplicates. Echo-S's added windows have voice files in these samples. These
measurements do not claim that sound, animation or all story effects are done.

The proof bar scene can now use Echo-S for a single owner with no watchers, including voice cues.
Before selecting it, the client runs both answer positions in both sections and requires the
measured question ranges, complete interpretation and matching persistent writes. It maps Echo-S
answer lines 0–1 back to original lines 1–2 for the server and for headless bot inputs. If the
text layer is off, a section is missing or validation fails, it uses the original. Scenes with a
watcher always use the original until mixed-layer window synchronization is implemented. This is
a preview milestone, not proof that the Reactor-to-bar route or all Echo-S scenes are playable;
the real bar write remains guarded until its persistent effects are carried safely.
Echo-S's bar windows in the measured archive do not identify speakers with the original's
first-line convention. For the exact measured replacement-section fingerprint, the preview uses
the speaker mapping established from the original windows and the two scripts' actor-action
traces. Other replacement sections leave unmatched speaker anchors unset rather than guessing.

The owned-data exit graph also gives the unreviewed continuation `cargoin` → `tin_1` →
`mds7st3` → `mds7plr1` → `mds7` → `mds7pb_1`. `tin_1` → `mds7st3` is a
script-driven line with a moment guard, and `mds7` → `mds7pb_1` is a script-driven
one-shot line, not a plain gateway. The authored scripts contain a write to moment 51 in
`tin_1` Cloud slot 3, moment 63 in `mds7st3` Cloud slot 3, and moment 69 and 72 in
`mds7pb_1` Tifa slot 5 and Barret slot 5 respectively. These are static observations, not
proof those slots are reached, that their skipped side effects are safe, or that the route
can be played. In particular, `tin_1`'s director starts Barret's slot 3 at moment 48, and
its concurrent actor requests must run before concluding the scene.
In `mds7`, the early director checks and sets `Var[3][209]` bit 0; the script-driven
entrance to the bar requests Cloud's slot 11, which sets bit 1 before jumping to `mds7pb_1`.
Neither bit is carried by the current story model. These are static findings, not evidence that
either sequence can be skipped or that a moment alone reconstructs the player's flags.
The unregistered `cargo_train_scene()` surveys `cargoin` at moment 39: staging the five
field actors through their own initial scripts resolves a facing target that was absent in a
bare director run. The sampled original director finishes at moment 48 with 26 windows, no
unknown persistent reads, untracked writes, unresolved actions or off-cast actors. Setup still
reports unimplemented `CHAR` and `PC`; scene playback skips background, sound and control
operations, and the witness and incoming transition are not validated. It must not be enabled
for live progress on this evidence. The `md8brdg` field jump supplies the survey's arrival
coordinates (0, 0, triangle 0), but that is not a verified witness location.
A sampled Echo-S replacement reaches moment 48 with 27 windows and no untracked writes;
three of those window/dialog lookups did not match a voice file, and voice playback and
semantic equivalence have not been checked. This comparison uses local owned data only.

The intermediate `mds7plr1` producer also tests `Var[3][208]` bits 0, 1 and 3 and can turn
bits 0 and 3 on; a stage script can turn bit 1 off. These field-entry paths must be evaluated
against actual player state, and the current monotonic union cannot represent the clear. Static
references do not establish which branch an existing player took.
An unregistered pillar-entry survey at moment 63 with absent flags assumed clear finishes
without a window, starts five measured field actors and sets `Var[3][208]` bit 0. It reads
that uncarried byte, so it cannot safely advance a real player; other flag combinations and
their clears still need to be modeled. Its arrival spot and presentation are not validated.
The original and Echo-S sections produced the same sampled write and no windows in this run;
that does not establish behavior for other flag values or enable either layer. Its offline
floor plan drew 16 walks and no witness.
The unregistered `sector7_intro_scene()` surveys the field producer at moment 63 using zero for
unavailable flags. That sampled run has no dialogue windows, stages ten actors and turns on bits
0, 2, 3, 5 and 7 of `Var[3][209]`; it also reads uncarried bytes 209, 210 and 213. The outcome
would be refused for real progression. Sound and background/control instructions are skipped,
and the witness, actual starting flags and full route remain unverified.
An offline floor plan drew 102 actor walks with no witness. The Echo-S replacement changes
several actor names in `mds7`; the standard identity rebind rejects this intro, so the
original-only survey is not an Echo-S playback check.
An isolated raw-VM comparison with manually identified producer indices did run both layers at
moment 63 with unavailable flags assumed clear. The original left the moment at 63 and wrote
`Var[3][209]=173`; Echo-S wrote moment 64 and that byte as 45. The replacement's producer has
an explicit moment-64 write at its moment-63 branch. Both runs used uncarried flags, so these
results do not establish behavior for every player; they do establish that substituting the
replacement without reconciling effects would be unsafe. The comparison example requires the
owner's local archives and does not enable the scene.
In the original `mds7` script, bit 7 is also read by the `obasan` talk script to select a
dialogue branch; it is not merely a value that can be discarded without investigating later
behavior. The inspected Echo-S field has no direct left-operand bit-7 comparison or `BITON`
for this byte, which does not establish what other rewritten logic or fields require.

Further static inspection of `mds7pb_1` connects the early bar sequence: director entity 7
requests Cloud slot 5 and Marlene slot 4 before starting Tifa slot 4. Tifa slot 4 requests
Marlene slot 5, which requests Tifa slot 5. That slot asks a two-position question; its first
answer turns on `Var[3][211]` bit 0, then the script runs `MMBud` and `MENU` before setting
moment 69. Later Tifa slot 10 requests Barret slot 5, which sets moment 72 and requests other
actors. These links are static script references, not proof of complete scheduling, reachability,
effects, or dialogue equivalence. The untracked flag and party/menu operations must be handled
before this sequence can safely advance a player's saved story.
An exploratory headless run starting director slot 1 at moment 63, with no stage setup and the
proof scene's limited cast, returned six windows and moment 69 for each answer. The first path
set both `Var[3][211]` bits 0 and 2; the second set bit 2 only. Each run also skipped `MMBud`
and `MENU`, had one unresolved position read and dropped 28 or 30 off-cast stage events. This
helps identify the missing work; it does not establish safe playback, cast, state, or completion
in the real engine.
An unregistered `early_bar_scene()` adds the observed six missing actors and runs the field's
stage director as setup. With the same two answers in owned data, it reaches six windows and
moment 69 without unresolved positions or off-cast events. Its test verifies model/idle stems
against the field loader. It still skips party/menu effects and assumes unsaved state; neither
its witness nor the real route has been validated. A private floor-plan survey drew 23 actor
walks across the bar with no witness. A candidate `(218, 292, triangle 37)` was visible and
clear of the cast in the inspected offscreen samples. Subsequent isolated client runs on both
answers each emitted an unskipped scene end and sampled 74 positions: the closest visible cast
actor was 280.2 walkmesh units away, versus a 44-unit ring threshold. This does not cover every
rendered frame, screen occlusion throughout, or all model/path combinations. No safe player position
has yet been established. Two other placeholder bodies were rendered on one answer path and
were visible in inspected samples; Ninostyle models and all-frame occlusion remain untested.
An isolated interpreter comparison used the original director/setup slots and the corresponding
manually surveyed Echo-S slots (the replacement renames several entities, so automatic rebind
rejects this definition). At moment 63 each original answer produced six windows and each Echo-S
answer seven. The choice ranges were 1–2 and 0–1 respectively; aligned by ordinal, both layers
produced moment 69 and the same `Var[3][211]` values. On each sampled Echo-S path the local voice
lookup found clips for six of the seven windows; the choice window had no match from that lookup.
The original skipped `MENU` and `MMBud`, while Echo-S skipped `SPECIAL` and `MMBud`; both read
uncarried persistent bytes. This comparison does not establish equivalent effects, multiplayer
synchronization, speaker anchors, or audible playback, and it does not enable Echo-S here.
A separate temporary `ScenePlay` experiment bound the sampled Echo-S section directly to its
own surveyed director and cast indices. Both choices again finished with seven windows, no
unresolved position reads and no off-cast events. This is preliminary staging evidence only:
the binding is not in the client, the actor identity mappings and presentation have not been
validated end to end, and party/state effects still prevent real progression.

An exploratory real-data run of `tin_1` director main at moment 48 produced two windows and a
moment-51 write, but ended at the VM frame budget with one unresolved read. It also ran looping
background animations and requests. It is not safe to commit the partial write or enable the
scene: identify the authored completion boundary before trying again. The unresolved read was a
turn of entity 40 toward entity 31; running entity 40's initialization as setup supplies its
authored position and removes that unresolved read. With that setup, the remaining live loop at
the frame limit is entity 35's slot 6 animation loop; entities 40 and 41 are parked. A narrow VM
rule recognizes that exact five-instruction animation-only cycle only when every other thread has
finished or is parked at an unconditional self-jump; it refuses to end while any thread is runnable
or waiting. A real-data regression exercises this director, and a synthetic check covers a later
window and a script waiter. This does not prove audiovisual parity or make the scene playable.
Stopping at Cloud's slot-3 return is not a valid shortcut: the trace still has other actors running
at later virtual times, including Jessie, Biggs and Wedge.
The unregistered `train_scene()` now names the measured nine-actor cast and their field-loader
idle clips; the real-data check verifies that this run drops no off-cast stage events. The witness
spot, rendering, skipped sounds and background effects, and the preceding route remain unverified.
`scene_map` can survey the unregistered train, station, and post-escape definitions without
registering them; its pictures remain outside the repository. A first train floor-plan run drew
46 actor walks and no witness, so it does not establish a safe witness position.
An isolated offscreen exploration later exercised one candidate each in the train and station,
and the existing post-escape witness spot. All three scene drivers became inactive in that
harness, but it did not assert the outcome, so this is not evidence of successful completion.
This was a rendering exploration, not a route or all-frame clearance test. The train candidates
`(169, 571, triangle 69)` and `(-172, 548, triangle 71)` were partly hidden in sampled frames;
an oversized foreground actor seen in both is part of the scene, not the witness. The original
post-escape witness was partly hidden behind scenery, and candidate `(-332, 150, triangle 16)`
was fully hidden in the sampled frames. The
station candidate `(1041, -5165, triangle 28)` appeared clear in the inspected samples, but
neither temporal clearance nor the complete route has been checked. None of these spots is
accepted for live use on this evidence. The exploratory harness's field label is hard-coded to
the bar and is not evidence of which field was rendered.
The original and Echo-S train director each yielded two windows, the same moment-51 write and no
unresolved reads on the measured run; both Echo-S windows had voice files. This comparison does not
enable the scene or prove that the arrival is reachable.

At `mds7st3`, the entry field's producer starts a chain through the guard, child, a passenger,
Wedge, Biggs, Jessie and Barret before Cloud's script writes moment 63. The unregistered
`sector7_station_scene()` surveys the producer at moment 51 with the ten observed actors; the
owned-data check verifies the field-loader model/idle stems and no dropped cast events. The
headless run reaches one window and moment 63 without unknown instructions, unresolved positions
or unknown persistent-byte reads, but it skips sound and field-control/background operations.
The arriving player's location, audiovisual presentation, party state, and actual route from the
train remain unverified. The authored train jump names arrival `(-2891, 3392, triangle 26)`;
that is an arrival, not a validated witness position. An offline floor plan drew 100 actor walks
and no witness; it does not establish a safe place to stand.
The original and Echo-S replacement scripts both produced that single window and the same
moment-63 and scratch-variable writes in the measured run, and the Echo-S window had a local
voice-file match. This is not a listening check or proof of live scene synchronization.

## Continuous route after the flower scene: historical owned-data audit, 2026-11-10

The current implementation above supersedes this audit's earlier registration and missing-engine
status. Its authored trigger/operand evidence remains useful; later lesson/station work remains open.

This audit begins with a successfully completed flower scene at moment 39. It re-read the
original English Steam `field/flevel.lgp` through the current field parser and scene VM;
it did not change saves, enable scenes, or test a continuous client/server journey. The older
isolated cargo, train and station surveys above remain useful, but leave out two connecting
sequences: the bridge confrontation and Jessie's train-map lesson. All coordinates below are
authored field coordinates. An arrival is not an accepted player/witness location.

### Forward connections and the work each connection performs

| From | Authored trigger | Destination and placement | Required work before accepting the transition |
| --- | --- | --- | --- |
| `md8_2` (134), moment 39 | Gateway 1, line `(-3983,20513,258)` to `(-3851,20541,258)` | `md8_3` (135), `(-3580,19968)`, triangle 3 | Complete the flower transaction first; the gateway itself does not perform it. |
| `md8_3` (135) | Gateway 2, line `(-4127,17274,397)` to `(-3947,17274,397)` | `md8brdg` (137), `(-422,1387)`, triangle 3 | Account for the street's one-time NPC flag and optional pickup described below. |
| `md8brdg` (137), moment 39 | Entity 4 `ev`, line slot 5; line `(-614,835,518)` to `(-238,835,518)`; requests entity 0 `dic` slot 3 | Script `MAPJUMP` at offset 1991 to `cargoin` (138), `(0,0)`, triangle 0, direction 0 | Three authored choices, possible battles 328/329/330, movie 23, then the jump. There is no plain forward gateway. The zero placement is marked as a placeholder by the script-exit reader. |
| `cargoin` (138), after its 39→48 arrival | Entity 10 `border1`, line slot 2; line `(-15,-104,0)` to `(53,-104,0)` | `tin_1` (139), `(-2,608)`, triangle 29, direction 252; `MAPJUMP` offset 1666 | The line disables control/menu and awaits Cloud entity 11 slot 8: door request, turn, wait, sound, jump and hide. It is not just a placement transfer. |
| `tin_1` (139), after its 48→51 arrival | First talk to Jessie, entity 34 slot 1, with `Var[3][223]` bit 2 clear | `rootmap` (143), `(0,0)`, triangle 0, direction 28 | Two windows; sets bit 2 before jumping. The train's departure line is disabled while this bit is clear. |
| `rootmap` (143), lesson path | Producer entity 4 main and director entity 5 main, with `Var[3][223]` bit 4 clear | `tin_1` (139), `(39,-325)`, triangle 49, direction 28; `MAPJUMP` offset 3240 | Eight windows and the animated map lesson; no persistent write on this measured path. |
| `tin_1` (139), returning from the lesson | Entity 17 director main, moment 51 and `Var[3][223]` bit 2 set | Remains in the train | Three additional windows through entity 19 slot 1; writes scratch `Var[5][30]=1`, with no durable change. |
| `tin_1` (139), moment 51 after the lesson | Entity 27 `border2`, line slot 2; line `(-55,-210,-53)` to `(52,-220,-53)` | `mds7st3` (146), `(-2891,3392)`, triangle 26, direction 0; `MAPJUMP` offset 4229 | Ten windows through Cloud 30/7, Barret 31/10 and entity 17/1, then movie 24 and departure. Moment remains 51. |
| `mds7st3` (146), moment 51 | Existing `sector7_station_scene()` producer/actor chain | Remains in the station; Cloud writes moment 63 | One window, authored disembarkation and cast movement; no additional persistent input was needed by the measured scene roots. |

The bridge's sole ordinary gateway goes **back** to `md8_3`, at `(-4027,17357)`, triangle
140. A generic bridge→cargo link would skip the confrontation. Likewise the train's static
`ScriptExit` reports the departure's `moment < 108` guard but does not include the separate
main-script `LINON` decision based on `Var[3][223]` bit 2. Static exit extraction alone is
insufficient evidence that either forward exit is currently usable.

### The bridge confrontation at the earlier survey checkpoint

The bridge line's initializer tests moment 39 at offset 2222 and runs `LINON 0` at offset
2230 for other moments. Its slot 5 disables control/menu, requests the director's slot 3 and
turns its own line off. The director's three question sites are entity 6 `mes2` slots 1, 2
and 3. Their authored answer ranges are 2–3, 0–1 and 0–1, all stored in scratch `Var[5][4]`.

| Answers supplied, in order | Current interpreter result | Authored battle instruction |
| --- | --- | --- |
| `[3,1,1]` | Eight windows, three questions, `MapJump(138)`; moment stays 39 | None on this path |
| `[2]` | Two windows, one question, `End::Battle` | Formation 328 at offset 1562 |
| `[3,0]` | Four windows, two questions, `End::Battle` | Formation 329 at offset 1679 |
| `[3,1,0]` | Five windows, three questions, `End::Battle` | Formation 330 at offset 1784 |

These four structural runs used all entity initializers and an exploratory cast, with extended
moment-39 state and known gil 240. They had no unknown persistent reads, unknown instructions,
unresolved positions or dropped requests; none changed gil or persistent bytes. The non-battle
path reaches `PMVIE 23`/`MOVIE`, fade and music before the cargo jump. Movie playback is skipped.
`WCLSE` and `WSIZW` are also skipped and the scene player pauses the entire stage for a window;
these counts are not a proof of original window timing, rendering, cast identity or witness safety.
Combinations that fight, resume and later choose another fight were not executed by this probe.

There is a concrete server/VM boundary to implement before registering this scene:

1. `Ff7SceneHost`'s `complete_outcome` accepts `Finished` or a consistent `MapJump`, and rejects
   `End::Battle`. A bridge scene must start the selected authoritative battle and retain enough
   scene state to continue afterwards, instead of treating a battle boundary as completion.
2. `Vm::seek_after_battle` reconstructs the existing presentation-only battle tail by accepting
   non-choice windows until a matching formation. It returns false on `ASK`. It cannot reproduce
   any bridge battle path, because each battle follows at least one question. Retain/replay the
   actual accepted answer history and the ordered battle boundaries; do not substitute default
   answers or turn the existing Guard Scorpion presentation continuation into a second reward path.
3. Verify victory, defeat, cancellation, reconnect and repeated completion at each boundary, with
   battle rewards applied once. Verify all eight combinations of the three binary choices through
   the final cargo jump, including consecutive battles. The four probes above stop at the first
   battle and do not satisfy that check.
4. Bind the measured cast, validate the line/arrival and a witness through the real window camera,
   then connect the cargo arrival scene. Neither `(0,0,triangle 0)` nor the exploratory cast is
   a safe live definition. Movies and authored window/field-control effects remain presentation
   work even after the state/battle path is correct.

The next implementation handoff is therefore the moment-39 bridge line and its choice/battle
continuations, reached through `md8_3`. Registering only `cargo_train_scene()` would leave the
actual incoming story sequence unimplemented.

### Street and train state that the director-only surveys miss

In `md8_3`, entities 6–9 test `Var[3][66]` **bit 1**, separately from the flower scene's bit 0.
Entity 7 `panpi4` main runs its one-time walk when bit 1 is clear, then sets bit 1 at offset
1324. It subsequently enters a repeating random/background walk rather than returning. A scene
must identify the authored one-time completion boundary; treating the entire ambient main as a
finite scene is not justified. The gateway geometry has no equivalent bit guard. An extended save
that knows only the flower's written bit 0 does not thereby know bit 1, and an unknown bit must
not be silently treated as clear.

The optional `md8_3` pickup is entity 11 `p`. Its initialization tests `Var[1][48]` bit 0;
talk slot 1 sets that bit at offset 1692 and executes `STITM` item 0, quantity 1 at offset
1696. The inventory award and consumed-pickup bit need one durable transaction. A flag-only
implementation would lose the item. This is additional street coverage, not a condition found
on the forward gateway.

The existing cargo arrival definition was rerun at moment 39 using both legacy and extended
state. Each run finished with 26 windows, no questions, moment 48, no unknown persistent reads
or unresolved positions, and no further persistent/resource effects. It still skips background,
control/collision and some audio effects. The exit's Cloud slot 8 and the actual bridge→cargo→train
movement are separate from this arrival test. Another cargo Cloud script contains a question
and `MINUS! Var[1][4],3`; it was not reached by the tested arrival and is not asserted here to
belong to this mandatory first-arrival path.

The existing train arrival definition was rerun in legacy and extended state: two windows,
48→51, no unknown reads/unresolved positions/off-cast events, and the same narrowly recognized
quiescent animation tail as the existing regression. That success does not enable the exit.
At moment 51, the train's field mains read `Var[3][223]` bit 2 to place Cloud and control the
departure line; other train NPC paths also read bits 0 and 1. Those field-main inputs must be
known independently of the arrival scene's successful roots.

On the first Jessie talk, a structural run with bit 2 explicitly known clear produced two
windows, a masked bit-2 assignment and the `rootmap` jump. With bit 4 explicitly known clear,
the map lesson produced eight windows and returned to the authored train placement, with no
durable changes. These are test inputs, not evidence that existing saves' missing bits are zero.
The field contains a separate bit-4 route from the map producer to the station, so a blanket
ROOTMAP→train transition would also be wrong: with bit 4 explicitly set, producer 4 requests
director 5 slot 1, plays movie 24 and jumps directly to `mds7st3` at the same placement,
without any window or durable write. No bit-4 writer was found in the scanned `tin_1` scripts;
its initial value cannot be inferred from moment 51.

The mandatory introduction, Jessie invitation, map lesson, return, departure and station
branches have no questions. Jessie's optional talk after the lesson does have a question
(absolute answers 2/3): answer 3 executes saturating `MINUS! Var[1][4],3`, with scratch
`Var[5][31]` tracking the repeat. That path needs the affection byte known and preserved.
Biggs/Wedge's optional talks use and write `Var[3][223]` bits 0/1. They are separate coverage
from the mandatory first lesson and cannot be replaced by clearing the whole flag byte.

ROOTMAP requires its own visuals: the owned field loader names `rootmapmain_cloudup.char`
(`clgd`/`cmda`) and `rootmapmidgal_highrez.char` (`cmde`/`cmhb`), rather than the train's
Cloud/Jessie bodies. The measured lesson skips `OFST`, palette copies/loads/multiplication,
background changes and fades. Those operations build the map presentation; eight dialogue
windows with a copied train cast do not establish a working lesson. The first isolated Jessie
and departure probes had unresolved actor-position reads (one and four respectively), because
their initializer setup did not reconstruct the field's live actor positions. Repeating them
with positions captured from the completed train introduction removed those unresolved reads;
the measured required branches then had no unknown instructions, unknown persistent reads,
unresolved positions or dropped requests. Production scene setup still needs to establish
those positions from the actual field state. These are structural tests, not accepted visual
definitions or witness checks.

The station arrival also skips `IDLCK` triangle 64 and two `LINON` operations, besides
background/audio controls. Those navigation changes remain to be integrated with the field.
After the station, gateway 0 runs from `(-3996,2032,0)` to `(-3595,1357,0)` and lands in
`mds7plr1` (156) at `(378,-264)`, triangle 65. That field's gateway 0 runs from
`(-495,-192,0)` to `(-423,-667,0)` and lands in `mds7` (151) at `(1641,-328)`, triangle 109.
These gateway observations extend the route inventory; they do not establish the pillar or
Sector 7 scenes' successful completion.

### Reproduction and limits

The committed isolated regressions are in `crates/fmv-ff7/tests/scene_real.rs`. With
`FMV_FF7_DATA` pointing at an owned installation's data directory, run each exact filter with
`cargo test -p fmv-ff7 --test scene_real <filter> -- --ignored --exact --nocapture`:

- `cargo_train_survey_runs_director_and_field_staging`
- `train_director_finishes_after_its_authored_actor_work`
- `sector7_station_survey_runs_the_authored_arrival_chain`

The audit additionally used temporary read-only Rust probes linked to the current `fmv_ff7`
library. Their structured listings and outcomes were retained locally under
`/tmp/ffmv-disc1-audit/`: `route_after_flower.txt`, `bridge_scene.txt`,
`train-sector7-evidence.txt`, `train-sector7-detail.txt`, `train-rootmap-evidence.txt` and
`train-extra-placed-evidence.txt`, with the adjacent probe sources. They contain instruction operands,
state transitions and counts, not copied dialogue or image/audio assets. Scratch files are not
durable test infrastructure. No accepted witness, complete field-main execution, connected
multiplayer route, post-battle bridge continuation or movie presentation was established here.

## Next implementation boundary

The bridge needs a durable scene continuation separate from the reactor's presentation-only
battle continuation. Resume the paused VM after a validated battle boundary; reconstruct it
from the original snapshot, seed and accepted transcript after restart. Commit the scene
checkpoint before combat, then rewards and the consumed battle ordinal together before
resuming. The current asynchronous reward save cannot by itself prevent repeated fights or
rewards after a crash. Missing choices must never select a battle by default.

Persist the pending final destination too: the bridge leaves moment 39 unchanged, so moment-
only relogin placement otherwise sends the account back to the flower field before cargo reaches
48. Implement the actual line trigger and all eight choice combinations, including consecutive
battles, defeat/escape behavior, reconnect and injected save failures, before registering it.
The all-run branch alone is insufficient. Cargo registration and the Jessie/ROOTMAP/return/
departure sequence follow this boundary; their isolated survey results are not continuous play.
