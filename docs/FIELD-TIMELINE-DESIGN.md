# Proposed authoritative FF7 field timeline

Status (2026-11-10): investigation and design only. This document does not describe
implemented scheduler support or authorize enabling another story scene. The current
scene interpreter still uses independent thread timestamps and stops the whole scene
at a dialogue window. Current registration, ordinary progression, rendering, and
transaction checks are a separate implementation wave. No source changes, Cargo runs,
deployment, or live-state changes were made for this design.

The next complete implementation boundary is one field-tick timeline shared by entity
scripts, windows, actor movement, and visual effects, with server-accepted inputs that
can be replayed at the same ticks. An eight-opcode quota alone would not provide this:
request priorities, concurrent dialogue, transition ordering, and durable replay must
agree on the same model before it becomes authoritative.

## Evidence and its limits

Primary evidence is the original PSX US engine source in `Xeeynamo/ff7-decomp`, pinned
to revision [`d4db3de3c3aaf61269e048cf3f6a458996290de0`](https://github.com/Xeeynamo/ff7-decomp/tree/d4db3de3c3aaf61269e048cf3f6a458996290de0).
The local checkout used for review was `/tmp/fmv-fade-evidence/ff7-decomp`.
These are static source findings, not original PC runtime measurements. The owner's
PC field scripts establish the instructions used by the reviewed routes, but do not
by themselves prove that the PC scheduler is identical to this PSX implementation.

| Behavior | Pinned source |
| --- | --- |
| Initialization defaults, priority 7, cleared saved PCs/sync/WAIT state | [field_event.c:132–240](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_event.c#L132-L240) |
| Ascending-entity initialization through the first RET | [field_event.c:243–270](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_event.c#L243-L270) |
| Eight handler calls per entity, early yield, and animation update | [field_event.c:381–432](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_event.c#L381-L432) |
| External talk/line requests | [field_event.c:503–578](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_event.c#L503-L578) |
| WAIT and WAIT1 | [field_opcodes.c:1135–1191](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L1135-L1191) |
| REQ/REQSW/REQEW admission and preemption | [field_opcodes.c:1930–2072](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L1930-L2072) |
| RET, RETTO, and backward jumps | [field_opcodes.c:2075–2175](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L2075-L2175) |
| MAPJUMP and BATTLE transition handshakes | [MAPJUMP](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L2197-L2222), [BATTLE](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L2273-L2295) |
| XYZ placement yields; MESSAGE and ASK block only their entity | [field_opcodes.c:3850–3923](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L3850-L3923) |
| Per-window ownership and message lifecycle | [field_dialog.c:381–394](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_dialog.c#L381-L394), [field_dialog.c:889–915](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_dialog.c#L889-L915) |
| FADEW examines the current global fade | [field_opcodes.c:6258–6300](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L6258-L6300) |
| Scripts, scroll/shake, transition checks, movement, then fade; display cadence | [field_loop.c:85–185](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_loop.c#L85-L185) |
| Battle/menu return preserves the interpreter instead of reinitializing it | [field_main.c:207–234](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_main.c#L207-L234) |

The quota counts handler calls, including calls that yield or complete a blocking
instruction. It is shared by every priority executed for an entity during that visit.
A RET can therefore restore a lower-priority PC and continue within the remaining
quota. A request targeting an entity already visited waits until the next field tick;
a later entity can run its new request during the current tick.

Backward JMPB/JMPBL instructions themselves return a yield. Preserve the existing
one-frame backward-jump behavior: it is supported by the original source. The quota
also limits long straight-line instruction runs. Do not add palette-specific WAITs.
The owned rootmap monitor ramps have loops without explicit WAIT, but their backward
jumps already yield; existing measured VM durations are not native timing evidence.

WAIT counters belong to entities, not to saved priority frames. Preemption clears the
counter and cancels scripted movement. WAIT N greater than zero starts at tick F and
continues at F+N if uninterrupted. WAIT 0 advances the PC and yields; the nearby source
comment calling it a NOP disagrees with the executable implementation. Initialization
is different: it repeatedly invokes handlers until the first RET, ignores their return
values, and does not apply the eight-call quota. Our initialization still needs an
instruction bound and must refuse unsupported nonterminating input.

Request admission must preserve these distinctions; replacing them with a FIFO queue
would change authored behavior:

| Target state | REQ | REQSW | REQEW |
| --- | --- | --- | --- |
| Requested priority is active, or its saved slot is occupied | Drop and continue | Retry/yield | Retry/yield |
| A higher priority is active, requested saved slot is empty | Queue and continue | Queue and continue | Queue and wait for completion |
| A lower priority is active, requested sync slot is clear | Preempt and continue | Preempt and continue | Preempt and wait for completion |
| Preemption is blocked by uncleared sync state | Retry/yield | Retry/yield | Retry/yield |

REQSW acceptance is not proof the child has started. REQEW completion remains reserved
until its caller acknowledges it. External talk/line requests use a different helper:
they only preempt lower-priority activity, do not enqueue behind higher activity, and
accept an empty RET script without activating it. Windows belong to an entity rather
than a script priority, so preempting an entity can suspend its window polling too.

## Current gaps that this design addresses

`crates/fmv-ff7/src/script/scene/vm.rs` currently chooses the runnable thread with the
earliest virtual timestamp. It models priority slots as queued independent threads,
runs straight-line operations without an entity quota, and returns globally when a
window opens. Its setup also uses this scheduler rather than the native initialization
mode. Some opcode yield behavior, including placements, differs from the original.

`crates/fmv-client/src/scene/player.rs` pauses actors and script time at an open panel
while advancing only fade/shake effects. The VM's FADEW uses interpreter time, so a fade
can visibly finish during dialogue and still incur another wait after dismissal. The
current replay acknowledgement carries a final answer, but no accepted field tick or
intermediate cursor inputs. It has one pending window rather than concurrent windows.

The original continues other entities, actor movement, camera effects, and fades while
a dialogue is held. ASK writes its current selection variable on every poll. Another
entity can observe an intermediate selection before the final answer. Consequently,
adding only elapsed dialogue time to today's final-answer transcript is insufficient
for unrestricted native behavior.

## Proposed implementation boundary

1. Replace independent thread timestamps with one 30 Hz field timeline and a native
   entity scheduler. State includes the tick, phase, entity cursor, remaining opcode
   budget, active/saved priority PCs, synchronous-request reservations, WAIT counters,
   blocking opcode state, windows, RNG, scratch variables, and pending global event.
   The finite scene controller remains separate from entity execution. A priority-7
   RET is quiescent rather than a destroyed entity, so later requests can still use it.

2. Audit each supported opcode's continue/yield/completion behavior. Blocking operations
   retain their PC until completion; yielding operations must not be advanced a second
   time by a generic delay wrapper. Keep the existing JUMP completion phases and other
   verified movement rules consistent with the new update order. Scene setup gets its
   separate bounded initialization mode. Reviewed scene entry priorities become explicit
   metadata where the present synthetic root steps do not capture native admission.
   Fingerprint any such semantic definition changes.

3. Execute one shared phase order: entity script visits and their animation-state update;
   camera scroll/shake and background update; field transition checks; then, on ordinary
   frames, movement and fade update. Effects are ticked once. A pending transition skips
   the later phases where the original outer loop exits. Rendering consumes the resulting
   state and never supplies authoritative elapsed time.

4. Represent windows as live entity-owned state, with up to four window slots. Opening
   emits an event without halting the scheduler. Cursor changes and completed dismissal
   are separate accepted inputs. Preserve the issuing instruction identity and opening
   ordinal for validation. Concurrent windows may finish in a different order from their
   opening order; event sequence and identity replace the single pending-ack assumption.

5. Feed the VM server-accepted inputs at canonical ticks. A proposed interface, not an
   existing API, is:

   ```rust
   struct AcceptedInput {
       tick: u32,
       sequence: u32,
       window: WindowCheckpoint,
       event: WindowInput,
   }

   enum WindowInput {
       Cursor(u8),
       Complete(Option<u8>),
   }

   // Bounded advancement; returns timed events, current windows, and any transition.
   fn advance_to(tick: u32, inputs: &[AcceptedInput]) -> Result<SceneBatch, SceneError>;
   ```

   The event must name an existing window, obey its choice range, and be legal at its
   accepted tick. Complete carries an answer only for ASK. Never default a missing
   durable answer. Input and pending-window state survive replay and checkpointing.
   Stage inputs for the owning entity's scheduled window-opcode poll; do not directly
   assign ASK's variable at tick start. Earlier entities in that tick observe the old
   selection, and later entities observe the value ASK writes during its visit.

6. Drain transition frames correctly. BATTLE/MAPJUMP record the first accepted global
   event and its issuing instruction; they yield that entity rather than returning from
   the whole VM immediately. Finish remaining entity visits before exposing the durable
   transition checkpoint. Do not run an extra movement tick while draining. On battle
   return, restore the exact scheduler/scratch state and authoritative resource afterimage;
   the original BATTLE instruction consumes its completion handshake on its scheduled
   visit. A map transition initializes the destination field instead. Preserve the current
   victory-only safety boundary until other battle outcomes are independently supported.

7. Keep finite-scene completion and budgets explicit. Ambient work runs during finite
   work but must not make an authored finite tail disappear. Preserve fail-closed unknown
   reads, unresolved operations, and unsatisfied foreground polls. Budget both each
   advancement batch and the total replay. Long dialogue now performs real background
   work, so today's instruction bound cannot be copied unchanged without measuring the
   reviewed scenes. Any safe idle fast-forward must preserve RNG draws, input observations,
   opcode quotas, animation deadlines, and event ordering; otherwise advance each tick.

## PC movie contract: static evidence, runtime integration pending

The owned Steam app 3837340/build 21140793 includes the English PE32 program
`resources/ff7_1.02/ff7_en` (5,997,027 bytes), SHA-256
`26998200234f7a49a700bdc1ae5d0a43fd001cc909871a366cf82bdffaa351c9`.
The addresses below identify instructions in that exact file. Each reviewed range was
checked against its PE mapping and original file bytes. Raw, file-backed static
disassembly is corroborated by retained REA/Ghidra 12.1.4 function dossiers from the
same digest. These are static findings, not runtime measurements; they establish a
PC movie contract without assuming PSX movie behavior. Representative retained evidence:
loader `ev_95c485c9260999fb51f35b3c98ef860474576b35c2d15d2a6f0ea0e355d557ff`,
frame getter `ev_715f530b20cfbaf638fbbc86561f3c8e8da933f1cf1a77848885b8e4da37065d`,
and sample update `ev_b5f976967700424b2d58a54697743fe9a1e5f0eaf71cec1acb006609fcd0ece3`.

| Native path | Verified behavior |
| --- | --- |
| Preparation `0x40ADB0`, member read `0x40AF83`–`0x40AFB7` | Reads the complete CAM member into the record buffer without skipping a header. Disc-1 movie 23 selects `ontrain.cam`/`ontrain.avi`; movie 24 selects `mainplr.cam`/`mainplr.avi`. |
| Media preparation `0x4176D1` | Calls `IMediaSeeking::SetTimeFormat` with `TIME_FORMAT_FRAME`; successful preparation therefore selects video-frame position units. |
| Frame getter `0x418613` | Reads `GetCurrentPosition` and returns its low 32 bits unchanged, with no increment or ±1 adjustment. Stopped/error paths return zero, so zero alone does not establish a valid first frame. |
| Camera selection `0x40AD49` | Selects the buffer at `40 * frame` and copies ten DWORDs. The native guard counts camera reads; it is not a safe bound on the frame index. A new parser must check member length and index independently. |
| `PMVIE` `0x61A224`; `MOVIE` `0x61A321` | Separate prepare and start/completion request handshakes retain the issuing script's PC while pending. This does not establish a pause of other field entities. |
| `MVIEF` `0x61A438`; snapshot write `0x63C29B` | Normal playback reads the frame snapshot written from the getter before script dispatch. It neither increments the movie position nor blocks. An alternate branch selected by global `0xCC0B68` increments a synthetic counter and must not define ordinary playback semantics. |
| Ordinary update `0x417FCA` | Tests multimedia-stream result `0x00040003` (`MS_S_ENDOFSTREAM`); the non-looping path sets the movie-end flag. Completion is separate from frame position and is not derived from CAM record count. |

The interface/constant meanings are independently anchored in the
[Microsoft SDK definitions](https://github.com/microsoft/win32metadata/tree/9cf94833626504b330c2f616ce71a2d3497cdc76/generation/WinSDK/RecompiledIdlHeaders):
`um/strmif.h` (`IMediaSeeking` vtable), `shared/uuids.h` (`TIME_FORMAT_FRAME`),
and `um/mmstream.h` (`MS_S_ENDOFSTREAM`). The position uses the selected frame format;
it is not a software counter advanced once per field tick.

The owned files fully decode: `mainplr.avi` has 197 video frames and `ontrain.avi`
has 192, both at exact rational rate `15/1` fps. Their CAM members contain 197 and
192 records of 40 bytes respectively. In these two members every record's first
38 bytes are zero, including the camera matrix; matching record counts do not make
those zero matrices usable camera poses. Preserve the final word at offset `0x26`
as opaque data. The reviewed 13 direct current-camera-pointer references do not read
it, but that bounded review does not prove its global PC meaning or that it is unused.
The pinned PSX source calls the corresponding word RGB24 mode; that is evidence for a
different platform, not a verified PC interpretation. `MVCAM` and `BGMOVIE` write
separate module control bytes, not this record word.

Still unresolved are the first camera-sampling phase after playback starts, the global
PC meaning of record word `0x26`, per-field/per-movie update cadence, alternate movie
update modes, and synchronization with rendering, audio and concurrent scripts. Startup
requests a media sample before a field callback can observe its position; synchronous
sample updates and the ordinary slow-playback branch also prevent inferring one sample
per field tick. The `15/1` video rate therefore does not prove an exact two-field-ticks
per movie-frame rule. Movie activation and the native timeline remain unimplemented;
these static facts do not establish end-to-end playback or timing parity.

## Multiplayer timing policy

The server owns a monotonic 30 Hz scene clock as an explicit port timing policy. The
original PSX loop normally uses VSync interval 2, but uses interval 3 in one movie mode;
native field updates are therefore not universally 30 Hz. Movie-specific cadence is
outside this first implementation boundary and must not be silently treated as verified.
On receipt the server assigns a legal input to
the next eligible tick; it does not trust a client-supplied elapsed duration or insert
an input retroactively. It broadcasts the accepted tick and sequence so owner and
watchers reproduce the same input stream. Visual interpolation may smooth delivery,
but cannot change script state, resource effects, or accepted timing.

Freeze the scene timeline during combat, a durable-save barrier, and owner disconnection.
Reconnect resumes from the accepted tick without advancing by offline wall time. These
are deliberate multiplayer policies, distinct from the original single-player engine.
Replay must remain independent of render FPS, network delivery batching, or process
restart duration. Inputs that would cross a pending durable transition are refused or
held until its exact acknowledgement, never applied to the next scene accidentally.

The server also defines window completion. If the port retains instant visual closing
initially, a Complete event explicitly means dismissal has completed under that policy;
it is not a claim to reproduce the original opening, text-reveal, or closing animation
duration. Faithful native closing animation can later supply that completion event.
This distinction must not reintroduce a whole-scene dialogue freeze. Cursor inputs must
still be recorded because scripts can observe them before dismissal.

The smallest complete release boundary includes VM scheduling, timed input/protocol
validation, replay persistence, host advancement, and client concurrent-window/timeline
handling together. A quota-only patch remains an experiment, not authoritative support.
Scene activation follows the owned-route tests below; the new engine must not silently
reinterpret scenes that have only been verified under the old ordering.

## Replay and protocol incompatibility

Current VM replay semantics and outer continuation contract are version 3, covering
OFST/palette support and corrected HBYTE/LBYTE semantics. That version does not implement
the native timeline proposed here. Its implementation requires a subsequent replay
version, a matching outer continuation-contract change, and a protocol version change
for timed inputs, concurrent windows, and clock synchronization.

Keep older durable records readable, but refuse to resume them under new semantics.
Their missing reading durations and intermediate choices cannot be inferred. Do not
invent zero-duration input history or silently migrate a pending checkpoint. A prior
compatible engine or a separately reviewed recovery operation is required. New source
fingerprints must bind semantic entry metadata, and a replay's execution version must
be checked before any advancement or durable writes.

Canonical checkpoints or reconstructed state must preserve the frame phase/cursor and
remaining budget as well as priorities, sync reservations, windows, effect state, RNG,
and pending global transition. A battle checkpoint includes all earlier accepted inputs
and same-frame post-request entity work. Story/resource compare-and-swap remains against
the exact predecessor; battle rewards are authoritative afterimages, not replayed from
the scene's old pre-battle gil.

## Verification and sequencing

Implement the pure scheduler/entity state first, then the timed protocol/replay and host
clock, then client lifecycle/timeline handling. Keep activation disabled while these
pieces disagree. Source-backed synthetic tests should independently establish:

- Eight handler calls execute in one visit; the ninth waits for the next tick. Backward
  jumps retain their yield. RET restores a lower priority within the remaining quota.
- Requests to earlier and later entities start on different ticks. Cover busy REQ drop,
  REQSW queued acceptance, REQEW completion reservations, external request differences,
  and preemption clearing the entity WAIT counter.
- Initialization stays at one field time while running through its first RET; normal
  WAIT 0, WAIT1, placements, and interrupted WAIT obey their actual return behavior.
- Other entities, movement, palette changes, and fades continue through held dialogue.
  A peer observes intermediate ASK cursor changes; replay reproduces those observations.
  An entity visited before ASK sees the old value, while one visited afterward sees
  the newly accepted selection in that same tick.
- Concurrent windows accept legal independent inputs and reject stale identity, invalid
  choice, missing answer, duplicate, reordered-sequence, and retroactive-tick inputs.
- FADEW immediately observes a fade completed during reading, observes replacements by
  other entities, and respects the script-before-fade phase boundary without double ticking.
- Transition snapshots include later entities' same-frame writes but no skipped-phase
  movement. Repeated formations retain distinct identities; battle completion is consumed
  once and preserves authoritative earned resources.
- Whole-run and reconstructed state agree across interruption at each frame phase, varied
  input delivery batches, owner disconnect/reconnect, and durable-save acknowledgements.
  Old execution versions refuse before producing effects.

Then rerun every registered owned route, all eight bridge choice combinations, all five
flower paths, cargo/train arrivals, and rootmap's palette/dialogue sequence. Exercise
short and long dialogue holds, including completion near effect/deadline boundaries.
Compare authored outcomes and intermediate stage ordering, not just final moments.
Investigate changed outcomes rather than rewriting old fixtures to match the new output.
GPU checks should confirm actors, camera, palette, fade, and windows agree on one tick
timeline. Durable/TCP tests must include accepted-timing replay after disconnect and
crash boundaries, with no duplicate story or resource changes.

Existing movement-duration estimates, unmodeled opcodes, original UI animation details,
and PC parity remain separate limits. This design establishes the scheduling and
multiplayer timing work needed next; it does not claim those other fidelity gaps are
resolved or that the scheduler is already implemented.

## Field-visit state contract (runtime unimplemented)

The idle storage foundation now implements this record and atomic Enter/Advance updates
with story/resources. It does not implement the running field lifecycle or activate the
train lesson/return. Existing checkpoint operations preserve visits and cannot create or
clear their active-continuation links; login refuses stored visits until the runtime can
restore them. See [the current foundation](DISC1-IMPLEMENTATION.md#native-party-and-field-visit-foundation).
The remaining integration complements the scheduler design: retaining a scratch image
alone does not restore an interrupted field runtime.

The pinned PSX engine has one 256-byte `g_FieldMapVars` buffer. Selectors 5/6 alias that
storage: U8 access uses one byte through either selector; S16 access through selector 5
still uses one unsigned byte, while selector 6 uses two adjacent little-endian bytes.
Reject a selector-6 word at address 255 instead of reproducing native out-of-array access.
The savemap member named `memory_bank_5` is unrelated persistent storage (selectors 7/15).
See the original [byte access](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L727-L738),
[wide reads](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L948-L966),
and [wide writes](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L1084-L1103).

`FieldEventInit` clears all scratch before entity initialization. An actual MAPJUMP,
including one targeting the same field, follows that initialization path; field-name
equality is not a preservation rule. A conversation/scene boundary does not reset it.
Battle/menu return skips field initialization and retains scratch/scheduler state, but
still needs its specific window/model return hooks; this is not byte-identical renderer
restoration. These are PSX source findings,
not independently verified PC runtime behavior. See [scratch reset](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_event.c#L132-L155)
and [map dispatch](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_main.c#L259-L270).
New field entry clears only visit scratch, never the persistent `StoryState` reset image.

Keep the proposed record separate from story progression and scope it to the account:

```text
FieldVisitV1 {
    visit_id: opaque 128-bit identifier,
    revision: u64,
    field: bounded field key,
    source_identity: script/init-profile/execution-version digest,
    entry: accepted arrival and entry-operation identity,
    anchor: last durable control position, facing, and triangle,
    scratch_values: [u8; 256],
    scratch_known_masks: [u8; 256],
    active_continuation: optional exact scene-instance reference,
}
```

An explicitly recorded entry establishes zero scratch, then applies audited authored
initializers. Missing legacy visit data does not prove zero in a resumed visit. Keep
unknown bits zero in the encoding and refuse reads requiring unknown inputs. Bind
SceneStart/replay to the exact visit ID, revision, source identity, and preimage digest.
CAS the visit before/after images atomically with story and resource changes; no union
merge or client-supplied latch is authoritative. Preserve the visit after scene completion.
Run field initialization once per visit, not for every scene; starting another scene must
not discard the retained runtime.
The current `jessie_train_talk_scene(bool)`/`Given` input is a probe convenience, not this
production contract. Expose scratch outcomes separately from persistent story writes.

Owned `tin_1` scripts make the atomicity requirement concrete: Jessie 34:1 checks 5:31
at offset 9089; answer 3 subtracts three affection at 9196; both answers set 5:31 at
9214. Another talk in that visit must take the one-window repeat path. Commit affection
and latch together so a crash cannot preserve the deduction but lose the latch. The
first invitation bypasses this latch, sets persistent 3:223 bit 2, and MAPJUMPs to rootmap.
Returning creates a new train visit, not a copy of the previous train scratch.

Return byte 5:30 is a handshake, not a completion marker. Helper 19:1 polls it at 4421;
Jessie sets it at 9450 after her third window, before more waits/animation, Cloud's
control release at 5480/5482, and the helper's fade tail. Retain/replay the shared active
VM/continuation; never reconstruct completion from `30 == 1`. A ready boundary likewise
needs an exported runtime image or a reviewed equivalent reconstruction profile for its
actor/camera/background state. Field name plus moment 51 is insufficient to choose
incoming versus completed-return placement. The owned definitions/tests are
`scene/data/train_route.rs` and `tests/train_lesson_real.rs`; the timeline/FADEW gap remains.

Reconnect preservation is explicit multiplayer policy: resume the same visit at its last
durable anchor and continuation, without treating offline time as field execution or
rerunning the landing scene. Actual field travel replaces the visit identity even when
the destination key matches. Watchers receive the owner's playback snapshot without
copying its latch into their own account state; their actual travel follows their own
visit-entry operation. A visit identifier must not itself split otherwise compatible
co-op phases. Any phase effect needs a reviewed gameplay dependency.

Acceptance before enabling these paths:

- Byte/word aliases, selector-specific width, overlapping writes, explicit zeroes, unknown
  masks, and rejected out-of-range access have independent fixtures.
- Both Jessie answers, affection saturation at zero, and repeated talk preserve the
  once-per-visit rule; duplicate completion and stale revisions cannot apply it twice.
- Crashes at every story/resource/visit commit boundary recover a coherent pair of
  affection and latch; reconnect before/after acknowledgement resumes the correct state.
- MAPJUMP round trips and same-field MAPJUMP create new visits; a scene boundary or battle
  return preserves the current visit. Unsupported source versions refuse before effects.
- Return interruption before and after 5:30 becomes 1 replays all remaining actor/fade
  work and releases control only at the validated endpoint, including after restart.
- Entry and ready anchors remain distinct; missing legacy state is never inferred from
  moment/field name. Watchers cannot mutate the owner's visit or acquire its latch.
