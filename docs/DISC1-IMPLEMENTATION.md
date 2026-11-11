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
are earlier checkpoints. The cargo-door/passenger-train changes below passed the
interpreter, persistence, TCP and rendering checks at that checkpoint. The fresh-state and
ordinary transaction changes now pass the combined acceptance checks below. These checks do
not establish complete original-engine visual or timing parity.

| Segment/system | Implementation/evidence now | Work still required |
| --- | --- | --- |
| New-game opening | Atomic new-account story/resources; explicit first-control profile; Cloud-only opening roster; authored first-battle policy; fresh-registration/restart TCP acceptance | Opening movie/runtime equivalence and complete startup writers |
| First reactor | Reviewed fields/scenes, exact ordinary CAS, atomic story-battle outcomes and required-route/relogin acceptance | Optional Restore materia, remaining presentation, resource awards and timing fidelity |
| Escape aftermath | Registered md8_4/md8_1 scenes, moment 33 to 36 | Movie parity and ordinary field health/resource persistence |
| Flower encounter | Registered opt-in scene; all five choices, atomic story/gil and earlier TCP/relogin acceptance | Original-engine presentation comparisons and wider mod/body coverage |
| Bridge and cargo arrival | Registered opt-in chain through moment 48; three possible battles, durable recovery and earlier owned/TCP coverage | Movies, remaining field effects and original timing comparisons |
| Cargo door / passenger train | Registered opt-in scenes 48 to 51; owned-data/TCP checks of crossing, durable landing, two windows and final control placement | Dynamic locks and visual/timing limits below |
| Train lesson / Sector 7 station | Unregistered original-script definitions, 14 owned interpreter tests and eight-window ROOTMAP rendering acceptance | Field-local state, remaining map presentation, movie 24 and complete linked route |
| Sector 7 / bar | Flag and choice surveys; isolated later bar proof | Ordered flags, affection, actual route, party/menu effects |
| Rest of Disc 1 | [Static chapter/field/mode manifest](DISC1-ROUTE-MANIFEST.md); no verified continuous route | Execute and implement the recorded branches and mode handoffs through the original Disc 1 ending |
| Battle system | Deterministic subset; bridge fights carry HP/MP, limits and persistent statuses | Remaining enemy behavior, statuses, summons, limits and original progression fidelity |
| Persistent gameplay | Character progress, inventory and durable scene transactions | Ordinary field HP/MP, canon party state, equipment/materia, shops and remaining scripted resources |
| World traversal, minigames, movies | No complete end-to-end subsystem | Required Disc 1 modes and transitions |
| Graphics | Scrolling camera, optional persisted FXAA and 2x scene rendering; authored backgrounds/fades/shake, offsets and palettes; calibrated PC clipping | Original-game comparisons, native scheduling and broader field/effect coverage |

The exact reactor route currently skips the optional Restore materia pickup. Its authored
`SMTRA(53,0)` requires a materia/AP inventory addition and a bank 13:31 result alongside moment
15; unsupported execution refuses all of those changes. The separate boss trigger permits
moment 14 to 27 and writes additional tracked flags. Its success therefore proves the required
route, not every optional resource action. The original pickup disappears after the boss and
cannot be recovered later on this route.

Jessie's reactor-rescue presentation now distinguishes an unknown rescued bit from a
known clear bit. After signed moment 27, unknown state withholds only her NPC and talk
point, preserving her slot and other NPC identities without inventing a trapped pose.
Known clear/set behavior and earlier signed-moment branches are unchanged; inspection
does not initialize the saved flag or enable native-party accounts.

## Durable-state foundations and remaining scope

Version 3 stores byte/word aliases, known masks, clears and numeric updates. Literal and bit
assignments establish only the bits they write; unknown numeric inputs still refuse authoritative
completion. Reviewed scene transactions compare exact prior story/resources, retain once-only
receipts and hold the cohort until the queued store acknowledgment. The bridge adds atomic
roster batches and durable battle/landing checkpoints; protocol 21 carries their replay data
and the ordered holds used by ordinary field-story transactions.
This is not general persistent execution of every field script or automatic save conversion.
The current change uses replay contract 3 for model offsets, palette state and corrected
high/low-byte extraction, rejecting older contract-1/2 checkpoints without changing them.
Index compatibility advances to 4 and detached archive manifests to 2 so older readers do
not open records with the new interpretation. The complete persistence suite passes
310 tests (one ignored), including migration fences, crash recovery and exact restore rollback.
Fully known
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

## Native-party and field-visit foundation

The next storage model is separate from route activation. FF7 story version 4 appends
12 bytes to the known-bank representation: three main-party slots and their knowledge
mask, plus independent 16-bit availability and locking values and knowledge masks.
The full image is at most 5,129 bytes. Raw field slots remain in bank 3:9–11; their
order or membership need not match the main party. Known empty slots differ from
unknown slots, and active membership does not imply availability or locking.

Versions 1–3 retain their existing encoding. Explicit promotion to version 4 leaves
the new resources unknown, even when the moment or raw slots look familiar. The
separate fresh native-party constructor is an explicit port reset/opening profile,
not proof of arbitrary PC startup state. It establishes only the opening's known
lock bit; other lock bits remain unknown. Registration still writes version 3.
FF7 login also refuses a prepared version-4 save until gameplay and wire support
are activated together; an older protocol-21 peer must not receive an unfamiliar
story blob and substitute its starting state.

Battle roster lookup distinguishes an unknown party from a known empty one and
preserves the order of known main slots. Malformed, duplicate or oversized rosters
refuse instead of losing members. The checked scene and exact ordinary-point paths
now interpret native party operations under the existing gameplay fences.
Server admission, registration, permissive dialogue, trigger placement and field-sound
execution remain gated. Scripted battles need the party at the exact BATTLE instruction;
the original or final story can contain a different temporary party.

An isolated native-party evaluator now models Add, Remove, Set, availability, locking
and the reviewed `FIELD_Init` return boundary. It retains immutable before/after images
and actual assignment masks, including writes of unchanged empty markers. Duplicate Add
and absent Remove still reconcile the main slots; removing availability clears matching
raw slots without changing main membership. Raw duplicates are not silently normalized:
native operations may repair or retain them, while an unrepresentable final main tuple
refuses atomically. Unknown required inputs also refuse, and applying a retained effect
requires its exact party/raw predecessor. Unrelated banks remain intact; durable commits
still require the complete story CAS. This evaluator follows the pinned PSX implementation;
PC equivalence remains to be checked against the owned executable. Its supported operands
are ordinary characters 0–8; flashback characters 9/10 and PHS-menu selection are deferred.
Its initial 18 tests include 15,028 comparisons against the previous accepted tuple behavior,
plus exact assignment masks, duplicate repair, ordered availability effects and stale
knowledge refusals. At evaluator checkpoint `8793a29`, all 825 FF7 unit tests and 500
server tests passed. The server suite round-trips the maximum 5,129-byte native-party
snapshot through three opaque fixture
battles, 125 windows and repeated store reopening, retaining index compatibility 5.
The four owned-data targets (`scene_real`, `train_lesson_real`, `rootmap_real`,
`train_departure_real`) also pass all 53 tests with ignored tests explicitly included.
All-target Clippy for FF7 and the server passes with the pre-existing standing-animation
fallback warning.

The interpreter layer uses one sealed journal for all persistent assignments and
native party effects, in execution order. A final byte map cannot represent a raw-party
write followed by a native party operation and then restoration of that raw byte: the
main party can still have changed. Each journal retains the complete predecessor and
afterimage; applying it validates every ordered event on a clone before publishing.
Knownness changes and temporary change-then-restore operations remain visible to setup
audits. Direct raw writes do not infer main membership, availability or locks.

At a native BATTLE boundary the evaluated story supplies the canon roster; only the
final victory step is a durable save result. Legacy actions retain their online leader
roster policy. Scene continuation resources follow character/account identity through
reordering or removal, while newly added or returning canon members refuse until a
durable source for their resources exists. Recovery must bind both roster order and
initial combat resources to the retained boundary and prior fight, rather than merely
checking that their numbers fit engine limits.

Native-journal checkpoint `b483409` passed the full workspace suite: 4,277 tests passed,
none failed, and 379 optional tests were ignored. Separately, 54 owned-data checks
ran successfully: the four route targets above and a new strict reactor-door replay
that verifies the authored Barret join against the complete native journal. Recovery
coverage also verifies that a saved field is loaded before roster validation while
combat still waits for each connected player's area handshake. These checks exercise
the internal native paths; they do not activate version-4 accounts or field visits.
Workspace Clippy with all targets passed after two lint corrections; its three
remaining warnings are the pre-existing audio, standing-animation and avatar-test ones.

Follow-up checkpoint `10946e0` adds the bounded PC movie-camera parser and includes
Jessie's unknown-phase fix (`d084168`). All 891 FF7 unit tests passed, with 10 optional
tests ignored. Explicit owned-data runs passed the three Jessie placement/idle checks
and the movie-camera check of all 197 `mainplr` and 192 `ontrain` records. FF7 Clippy
with all targets passed with only the existing standing-animation warning. Movie
playback and field-visit activation remain outside this checkpoint.

These paths still use the existing scene scheduler and scratch initialization model.
Native-party support alone does not establish a restored field visit. In particular,
absent scratch must not be assumed zero across separate talks or same-field travel;
checked visit injection and cache identity are prerequisites for gameplay activation.
The sound worker now reports unsupported native-party requests explicitly instead of
returning an apparently successful empty field-sound plan.

`FieldScratch` provides checked 256-byte values and knowledge masks for future visit
restoration. Selectors 5/6 alias for byte access; wide selector 5 still uses one byte,
while selector 6 uses a little-endian word. Invalid boundary reads/writes refuse
atomically. Its explicit entry clear is distinct from an unknown loaded visit. It is
not yet connected to the running scene VM; see the
[field-visit contract](FIELD-TIMELINE-DESIGN.md#field-visit-state-contract-runtime-unimplemented)
for lifetime, continuation and reconstruction requirements.

`FieldVisitV1` storage keeps entry identity, revision, source identity, an opaque accepted
arrival and reconstruction anchor, scratch values/masks, and a continuation reference in
the private character envelope. Its idle-only Enter/Advance transaction compares the
complete story, resources and visit, then journals both records together. A bounded last
operation receipt acknowledges an identical retry using the current state, preserving
later rewards. Advance retains known scratch bits and increments revision without wrapping;
actual entry replaces the visit identity, including same-field travel. Existing story,
character and battle writes preserve visits rather than defaulting them to absent.

The first visit write requires character envelope 4 and index compatibility 5. Opening or
compacting an older store does not select that format. Explicit compatibility selection is
also available for a future party-v4 writer; storage does not inspect an adapter's opaque
story bytes to infer it. Detached archives from index-5 stores use manifest 3, including
party-only stores with no visit-bearing character. Snapshot copy limits distinguish the
existing 4 MiB character-record bound from the 1 MiB ordinary-record bound.

This is storage support, not a running field-visit lifecycle. Enter/Advance refuses active
scene checkpoints/reservations, and existing scene batches cannot change a visit or orphan
its continuation link. No server path creates these visits yet; login explicitly refuses
visit-bearing snapshots until anchor reconstruction, VM scratch injection, accepted entry
and atomic active-continuation transitions are implemented. The train lesson remains
unregistered, and existing accounts are not converted.

Storage checkpoint `075eb34`: the workspace run plus corrected-target reruns passed 4,180 tests;
378 optional tests were ignored. The two initial failures were stale expectations:
index version 5 is now supported, and an invalid canon member now refuses the entire
battle instead of being dropped. The affected persistence and server targets passed
35 and 499 tests after those corrections. Separately, all 37 ignored `scene_real`
tests ran successfully against the owned field archive. New coverage includes visit
Enter/Advance recovery at every journal boundary, exact retry identity, scratch
knownness, archive rekeys, large snapshots and refusal before unsupported saves can
reach a client. These are foundation checks, not acceptance of the unregistered route.
Workspace Clippy with all targets also completed successfully; its three warnings in
audio tests, standing-animation fallback and avatar test types predate this change.

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
v1/v2 records. Extended-save registration atomically commits the fresh story profile and
owned kernel's initial resources before issuing the account token. Existing accounts,
including empty legacy stories and zero gil balances, never receive those defaults.
`fresh_reset` records the original reset image; `fresh_first_control` is the explicit port
profile after reviewed opening assignments, with unsupported engine-owned values unknown.

The pinned original PSX reset explicitly clears all 1,280 persistent bank bytes, then sets
menu, party and location-buffer exceptions. The owned PC `md1stin` opening assigns affection
bytes 3–6 to 50/30/10/0, bytes 80–83 to 100, word 28 to 763, and finally moment 1. This gives
positive provenance for reset-clear `Var[3][223]` bits 2/4 and first-control affection
`Var[1][4]=30`; absence of an earlier writer or the current moment is not that provenance.
PC source confirms the same bank layout but not its reset routine. One post-reset PSX menu
routine is also absent from the pinned reconstruction. Clocks, location text, counters and
other engine-managed bank bytes need their writers or explicit unknown masks; seeding them
known and invalidating later is not supported by today's monotonic knownness contract.
The profile therefore leaves those inputs unknown from creation. It does not claim the
unrecovered PC startup routine or opening movie has executed.

Cloud starts alone. The door's authored party assignment adds Barret at moment 11;
the station removes him at 36. Actual bank-3 party-ID assignments are journaled separately
from the main party slots. A bounded unlocked-member projection covers the reviewed route
through 63: the door unlocks Barret and removing him does not undo that unlock. General
PHS persistence, later party layouts and unsupported availability queries remain unimplemented.

The opening's literal `PRTYE(0,254,254)` at `md1stin` offset `0x03FD` establishes all three
physical party bytes `[0,255,255]` before first control. This fresh-profile exception is
applied after the general engine-field mask; it never supplies missing bytes to an existing
save. The two original reactor-gate naming menus use an explicit port policy: retain the
canon names already used by dialogue. Their return performs the original party reconciliation,
preserving retained physical slot positions and recording only actual assignments. This
capability is bound to the reviewed original script and its two exact menu sites. The name
editor, renaming persistence and modified/localized versions of those menus are not implemented.

Jessie's first train talk requires bit 2 known clear, sets it, and transfers to `rootmap`.
The lesson requires bit 4 known clear, then returns to a different train placement and a
three-window continuation sharing scratch state. The departure line requires bit 2 known set;
its slot alone does not repeat that guard. Its ten windows and movie 24 precede the station's
one-window 51 to 63 arrival. Optional Jessie dialogue can subtract from affection, so it also
needs the authored initial value and the field-local once-per-visit latch. None of these
unknown inputs may be supplied as zero merely to unlock the route.

## Unregistered next segment: train lesson through station arrival

`script/scene/data/train_route.rs` defines `ff7/tin_1/jessie-train-talk`,
`ff7/rootmap/train-map`, `ff7/tin_1/train-map-return` and
`ff7/tin_1/sector-seven-departure`. The existing station definition now initializes all
23 original entities. None of these definitions is added to the production registry.
They describe original script execution with explicit inputs; they do not enable a route
by assuming unknown flags are clear.

* Jessie's first talk has two windows, sets only bank 3:223 bit 2, and transfers to
  rootmap without changing moment 51 or gil. Later talk retains both authored answers:
  answer 2 leaves affection unchanged; answer 3 subtracts three from bank 1:4 with a
  floor of zero. Both set scratch 5:31. Another talk in that same visit has one window
  and no choice. The runtime must preserve this latch across talks, commit it with the
  affection effect, and clear it only on actual field entry. Supplying `false` to every
  new scene would incorrectly repeat the affection deduction.
* Rootmap's independently known bit 4 selects either the eight-window lesson and return
  to tin_1 `(39,-325)`, triangle 49, direction 28, or movie 24 and the station. Both paths
  preserve story and resources. The Cloud/Jessie close-up rigs are `clgd`/`cmde`; their
  original type-0 `OFST` and palette programs are required presentation. Their authored
  base `(-18,-459,-56)` is on ROOTMAP triangle 4. The script's triangle 53 is outside its
  five-triangle mesh; the definition keeps the authored coordinates with a verified floor
  hint instead of the incoming MAPJUMP's zero placeholder.
* Return runs Cloud's main and the director/Jessie work in one new field VM. Three
  windows and scratch 5:30 synchronize completion. Final Cloud position is `(72,-319,-53)`,
  triangle 47, turned toward Jessie by `TURA`, not the initial direction 64. Biggs's
  independent bank 3:223 bit 0 is also an input to the native carriage setup. Re-entering
  the field resets scratch; separate talks within that visit must not do so. This return
  reaches `FADEW` after dialogue, exposing the VM/client elapsed-dialogue-time gap described
  above; renderer support alone does not resolve its scheduling.
* Departure requires bit 2 explicitly known set at the original border2 line
  `(-55,-210,-53)` to `(52,-220,-53)`. Slot 27:2 relies on the main's gate and does not
  repeat the flag test. It preserves the accepted Cloud crossing, plays ten windows and
  requests movie 24 before station MAPJUMP 146 at `(-2891,3392)`, triangle 26. A delayed
  concurrent helper performs audio/fade work during that movie; immediately skipping the
  movie also truncates this helper. The verified movie basename is `mainplr`. The owner's
  Alienware Steam app 3837340/build 21140793 supplies it at
  `ff7/workingdir/data/movies/mainplr.avi`, alongside `ontrain.avi` for movie 23 and
  `data/cd/moviecam.lgp`. Private, hash-verified copies of only those three files are now
  available to local tests (7,979,036 bytes total; no game media is stored in this repository).
  Both `.avi` files actually contain Matroska VP8 video at 1280×896 and 15 fps with stereo
  Vorbis audio at 22,050 Hz; format detection must inspect the contents. Full-file video/audio
  decoding passed without diagnostics. `mainplr` has 197 decoded frames (13.133 s container
  duration), and `ontrain` has 192 (12.800 s). The archive contains matching `.cam` entries
  of 7,880 and 7,680 bytes. Static inspection of the owned PC executable now verifies
  headerless 40-byte records selected by the media frame position, without a plus/minus-one
  adjustment. Playback completion uses the stream's end signal, not the camera record count.
  The first observed frame and concurrent field scheduling remain unverified; see the
  [PC movie evidence and limits](FIELD-TIMELINE-DESIGN.md). The first 38 bytes of every
  record in these two camera tracks are zero, so they cannot supply a projection matrix.
  The final word's PC meaning remains unresolved; it must not be treated as a camera-valid bit.
  The exact video durations are 197/15 s and 64/5 s. Decoded audio contains 286,848 and
  282,240 samples per channel respectively (about 13.009 s and 12.800 s); stream duration
  fields are absent and audio timestamps begin at 0.011 s. Audio sample counts alone
  therefore do not define the movie's final frame or completion boundary.
  The same install also supplies the English PE32 program at
  `ff7/resources/ff7_1.02/ff7_en` (no `.exe` suffix, 5,997,027 bytes). Its private copy is
  hash-verified for static PC comparison; copying it does not establish runtime behavior.
* Station arrival has one window and writes moment 63 without inherited train scratch.
  Its final Cloud position is `(-2738,3017,101)`, triangle 9, direction 16. Original
  `UC 0` and the moment write precede Barret's window and remaining actor work. A durable
  tail hold therefore differs from original control timing, and a checkpoint can already
  contain moment 63 while that tail is unfinished. Required channel-3 door calls, volume
  commands and the dynamic triangle-64 lock are separate integration requirements.

The channel-3 audio gap is more than a missing play opcode. Pinned native PSX
[`akao.c`](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/akao.c#L1677)
maps commands 162/166 to that channel's persistent gain and gain slide. Command 166 takes
duration first, target second, and treats duration zero as one audio-envelope tick; these
ticks are not field frames. Gain survives replacement sounds. Scene-end cleanup must not
restart the train hum while a stopped channel is still held for transfer. Command 202 in
the movie helper instead ramps CD/movie audio. PC source corroborates channel scope, but
the original PC dispatch arithmetic and zero-duration edge remain independently unverified.

The new `train_lesson_real`, `rootmap_real` and `train_departure_real` targets cover these
branches, partial masks, unknown reads, accepted origins, final poses and script ordering.
All 14 tests passed on the owned data in the fresh combined build. The ROOTMAP GPU test
also reached all eight windows and its terminal transfer; matched frames show that the
authored offsets bring Cloud and Jessie into the foreground. Its monitor remains black
without movie playback, and original scale and palette timing during dialogue remain
unverified. These isolated checks do not enable the segment. Production additionally needs
retained visit state, durable held landings
through both field transfers and same-moment return, destination handshakes/relogin, movie
playback with concurrent script scheduling, palette/offset rendering and complete authored
audio. No live-save conversion or deployment accompanies this evidence.

## Verification coverage for this wave

The earlier cargo/train checkpoint passed 1,456 client tests, 561 server tests and 257
persistence tests. The complete FF7 library run passed 661 tests; after the final
checked-access lint fixes, all 139
scene units and all 39 owned scene tests passed again. Owned train and bridge TCP runs and
the cargo/train host placement check passed. The renderer checks passed eight owned scene
tests, the background lifecycle test, the fade compositor test, four supersampling tests and
the range-clipping regression. Workspace all-target clippy passes with three pre-existing
warnings. These runs used disposable state; there was no deployment or live-save conversion.

The subsequent workspace suite passes 4,098 tests with zero failures and 378 ignored tests.
It includes 1,499 client unit tests, 494 server library tests, 16 server binary tests, 769
FF7 library tests, 94 protocol tests and the complete 310-test persistence suite. The final
palette checked-access cleanup additionally passes its seven focused tests and the owned
ROOTMAP color-ramp test. Workspace all-target clippy passes with three pre-existing warnings.

Owned checks separately pass all 39 scene tests, six story tests, 14 train-lesson/ROOTMAP/
departure tests, six server host tests, 16 server battle tests, and bridge/train/opening TCP
tests. ROOTMAP reaches all eight windows in the renderer; the actual team-talk door models
reach their closed, halfway and open positions through all 12 windows. All seven battle
smoke modes pass; they explicitly seed moment 11 and do not prove fresh progression.

`opening_real` exercises genuine v3 registration, battle 300 and item use, the accepted
team-talk origin and exact transfer, saved landing recovery, stale ordered-acknowledgment
fencing and restart at moment 8. The extended full-route smoke completes 17 owner transfers
to `md8_1`/moment 36 and 15 follower transfers to `nrthmk` while preserving the follower's
personal moment 1. Both shared victories replay exactly. A 40-second same-store restart
retains both accounts' exact story and character records, with no battle, scene, transaction
or reward replay. The full-route run uses a level-30 battle modifier and skips the unsupported
optional Restore pickup; it is required-route acceptance, not native difficulty or all-choice
parity. This follower was not selected for the finale, so it does not prove a second station
arrival. Verifier expectations were corrected from independent original-script evidence;
the retained-run restart used the same binaries, store and tokens after those checks were
corrected. Its verifier also rejects 16 deliberately corrupted evidence cases.

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
