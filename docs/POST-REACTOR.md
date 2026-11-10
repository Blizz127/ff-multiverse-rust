# Post-reactor route work

This is an implementation checkpoint, not a playable route. The current player build stops at
the first post-escape field. The scene definitions in this branch are not registered for play.

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
writing moment 39. A presentation-only replay must not alter currency; a server-owned, once-only
transaction and its persistence need to be implemented before serving that branch.
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

A read guard needs more than checking whether a byte was written in the VM. A bit operation
can preserve unknown bits, arithmetic can propagate an assumed value into another byte, and a
bit-test may depend on only one known bit of a partially tracked byte. Track knownness through
the operations that produce values and reject a real run whenever an unknown persistent input
can affect a branch, a persistent effect, or a scene result. Treat `Given` as preview context,
not authoritative saved data; retain the separate preview path. Exercise partial-byte tests,
word reads, overwrites versus bit operations, and propagation through scratch variables before
using this as a safety check. This is an implementation requirement, not a proposed change to
the live story format or authorization to migrate player data.
The existing story merge unions tracked bits and is not a numeric affection merge. The correct
initial affection and the behavior of later increases/decreases must be established from owned
game data before proposing a durable numeric representation; treating the observed `+5` as a
complete value or simply OR-ing bytes would be incorrect.
An owner whose client cannot build or finish the interpreter run aborts without saving; a timed-out
run also leaves progress untouched rather than silently choosing any remaining answers.
The server also checks the setup interpreter's unknown instructions, unresolved positions,
dropped requests and ignored stateful operations before accepting an outcome. Setup itself can
still stop at its budget and discard running threads without recording a completion reason, so
these checks do not establish that every setup finished correctly.
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
