# The live field runtime

Owner direction (2026-11-11): finish FF7, Disc 1 first, 1:1 with the original, using this framework. Before this
runtime, an ordinary field was static analysis plus hand-written tables. Only the reviewed first-reactor fields had
story points, and story scenes were registered one at a time. That path cannot reach the end of Disc 1, so the
runtime (`crates/fmv-ff7/src/runtime/`) runs **every field's original scripts**, frame by frame, the way the field
engine does.

## Model

- **Initialisation:** each entity's script 0 runs to its first `RET` on field entry. The bytes after it become the
  entity's main script at priority 7.
- **One frame (30 a second):**
  1. Talk, push and line requests start scripts 1-6 at priority 1.
  2. Every entity, in order, executes up to 8 instructions. It stops early at an instruction that yields: a wait,
     a backward jump, an open window, a movement under way, or a pending command.
  3. The instruction families then update: movement, animation and effects.
- **Requests and priorities** follow the original: `REQ`, `REQSW`, `REQEW` and `RET`, with per-priority saved PCs,
  queued lower-priority requests and synchronous waits.
- **Commands** (map jump, battle, minigame, menu, movie, game over, disc change) are recorded and held until the
  host completes them. A battle, menu or movie then continues the same field; a map jump makes the host load the
  next one.
- **Windows** (`MESSAGE`, `ASK`) are entity-owned. Each holds only its own script until the host closes or answers
  it; every other entity keeps running.

Behaviour comes from the field-script documentation (FFRTT and the Qhimm wiki, Makou Reactor's opcode table). It also
comes from reading the PSX decompilation (`~/dev/ff7-references/ff7-decomp`, AGPL-3.0) **for behaviour only**: no code
is copied, because this runtime runs in the server. The decompilation is the PS1 build, so PC differences found by
static inspection of the owned executable win. One example is in [FIELD-TIMELINE-DESIGN.md](FIELD-TIMELINE-DESIGN.md):
the byte/word selector widths and the signed word arithmetic.

## Coverage is the roadmap

`cargo run -p fmv-ff7 --release --example runtime_coverage -- --list <disc1 field list>` counts every Disc 1
instruction as implemented or not, and then runs each field. Unimplemented instructions are skipped and counted:
the script goes on without them and never stops. The first core (2026-11-11) implemented **77.3%** of the 231,733
static instructions in the 441 Disc 1 fields. The rest is split into families, one file each under `runtime/ops/`:

- `motion`: movement, turning and walkmesh
- `anim`: animation
- `stage`: background, palettes, camera, fades and shake
- `windows`: window variants and contents
- `party`: PHS, HP/MP, items and materia, battle modes

## Multiplayer decisions (deliberate differences from the original)

- **The player character.** Line, contact and talk triggers fire on the position of the **party leader's avatar**.
  While the player has control (`UC 0`), the leader's avatar *is* the field's player model (`PC` of party slot 0, or
  `CC`): the host feeds its position in through `PlayerInput`. When a script locks control and moves that model,
  the runtime moves it, and the host moves the avatar to match. The canon character (Cloud) keeps his story role
  (owner's choice: "Cloud keeps his role"). The canon cast's models are the field's own entities. Players' original
  characters are shown alongside them by the client; scripts never see those characters.
- **Whose story.** One runtime runs per area instance. The instance's governor (the party leader) supplies the
  persistent banks, and the governor's save receives their changes. Party members in the instance see the same
  scene.
- **Windows** are answered by the leader. Other party members see them read-only.
- **Talk.** The original's talk cone is approximated: the model in front of the player within its talk range.

## Persistence

The runtime's persistent memory is the 0x500-byte image of banks 1/2, 3/4, 11/12, 13/14 and 15/7. It is kept in a
sidecar record next to the MMO location record. It is not kept in the fenced story blob, so older builds ignore it
and rollback stays a plain cold swap. A fresh account starts from the new-game image. An existing account whose story
has unknown bits is never filled with defaults in place (no player progress resets); only fully known stories
convert.

## Running it

```
fmv-server --field-runtime ...        # live fields; new accounts start from the complete reset story
python3 -I scripts/live-bot.py HOST:PORT --seconds 45 --press-every 1.5 --approach   # protocol-23 route bot
FMV_FF7_DATA=<data> cargo run -p fmv-ff7 --release --example runtime_trace -- md1stin --at 3875,29195   # one field, headless
```

**Status, 2026-11-11:** over TCP, md1stin's opening plays from its own scripts:
- the opening movie request (53);
- the party set to Cloud;
- the guards' and passengers' placements;
- Barret's arrival;
- the director's first window;
- control handed back at story moment 1, saved as an exact story transaction.

Scripted movement, animation, background, camera and fades are being implemented, one instruction family per file.
The curated scene route stays the default until the live route reaches further.

## The headless solver (`examples/runtime_explore.rs`) and the route bot

The solver plays the story on the runtime alone, with no server or client. A state is a running field plus where
the player stands. The search always expands the state with the furthest story moment. Expanding a state tries what a
player could do from there:

- stand on a triangle, with and without OK;
- talk to a model;
- walk at or across a line, or press OK at it;
- walk through a gateway.

A candidate is found by standing somewhere. It is then carried out by walking there along the walkmesh, one stride a
frame, with the field running, so what happens on the way replaces the action. A walk never enters a triangle a script
locked (`IDLCK`), never passes a solid model, and takes a gateway only while map jumps are allowed. Game over is not
progress. `--route-out` writes the chain of actions as `scripts/live-route.py` entries, with the exact corners walked.

The solver and `runtime_trace` read animation lengths from the owned `data/field/char.lgp`, as the live server does
(missing lengths retain the runtime's fallback). Entry scenes are allowed up to 3,600 frames to settle; a field still
holding control after that budget is reported as busy, without trying player actions. The cargo-car scene lasts
longer than the old 900-frame entry budget: expanding it early incorrectly attributed its automatic progress to a
walk that the player never took.

The TCP bot's stall clock counts dialogue windows as progress. A scene can hold control for more than a minute while
its dialogue advances; repeated cast or lock snapshots alone do not reset the watchdog. `--verbose` prints window
IDs and control changes without game text. `--token-file PATH` saves a newly registered **test** account's token with
mode 0600, or resumes it when the file already exists. Use the remaining route with that account on its throwaway
server state; no route run should use the owner's saves.
An entry with an explicit `gate` resumes its remaining walk after a scene in the same field (the station's farewell
conversation is one example). Other entries still stop when a script takes over, since a ladder or jump may be their
intended result. A failed run prints the field it expected and the control, window and pending-step state.
Field transitions complete an already-started entry immediately, even while control stays locked: an automatic
flashback can leave the bar and return without ever accepting player input. An unstarted entry whose field is
visited and left is reported as an obsolete route, with its actions still unplayed; the bot never skips it as passed.

For timed routes, `idle S` means S seconds with free control: its budget pauses during scenes, open dialogue,
movies awaiting acknowledgement and battles. A trailing idle survives a same-field script cancelling unfinished
movement, and a field change discards the old budget. Intentional free waiting counts as watchdog progress, while
a stuck lock still times out and the overall run budget always applies. `wait S` retains its wall-clock meaning.
The asset-free checks run with `python3 -m unittest discover -s scripts -p test_live_route.py`.
The solver uses 200 field units per second, matching the bot's default, and appends coalesced voluntary waits to
its honest action playback. It rejects branches whose internal wait/movement order cannot be represented by an
idle suffix, including interrupted walks that ordinary dialogue would not cancel in the bot. Entry delays,
network sampling and dialogue response timing still differ; a generated route needs a TCP replay.

For an authored dialogue branch, repeat `--choice FIELD:DIALOG=INDEX` on the solver. Answers count from zero relative
to the question's first choice; an unspecified question still uses zero. The exported route starts with
`@choice FIELD DIALOG INDEX` headers, and the TCP bot applies that same policy even in an automatic entry scene,
before walking begins. Each override applies to every occurrence of that field/dialogue pair. Duplicate or malformed
headers fail before connecting; a configured answer outside the encountered question's choices stops the run before
answering. This is an explicit replay policy, not automatic exploration of all dialogue branches. Busy solver states
also report script PCs/waits and scratch counters, so a bounded settle failure need not be mistaken for a closed path.

For timing diagnosis, the TCP bot accepts `--answer-delay SECONDS` (default0.2). A window closing or being reused
cancels its old queued answer. The single-field trace accepts `--choice DIALOG=INDEX` and
`--answer-delay-frames N` (default0), and continues ticking every entity while an answer waits. Its
`--walk-tri` now defaults to200 units/sec (`--speed` changes it); fractional positions accumulate between ticks and
are rounded for the runtime's integer input. `--then-tri` remains a diagnostic placement, not a gameplay route.
These response delays are test policies, not native window emulation; `runtime_explore` still answers immediately.

**Window lifecycle (first increment, 2026-11-12):** the owning `MESSAGE`/`ASK` polls now grow the window using its
integer quarter dimensions, pass through a separate Text poll, then emit `Event::Window` when input is ready.
Early, duplicate, prohibited and wrong-kind answers are rejected. Accepted input starts shrinking; the owning
instruction resumes only when both dimensions have closed. Other entities keep running, and a preempted owner
cannot advance its window until it resumes. ASK holds and non-passive window waiting remain through closing.
This restores structural timing without changing protocol24: the client receives the existing window message at
readiness and keeps it until final closure. `LiveWindow.answerable` still expresses governor permission.

Scripted closing keeps distinct opcode behavior: `WCLSE` requests closing after opening and continues without
waiting for shrink; `WCLS` waits for vacant ownership; `WSIZW` waits for a foreign owner or closes its own window
before resizing. Foreign callers cannot double-step the owner's geometry. A tiny/zero window completes once
without reopening; this is a bounded port behavior for a degenerate original-state-machine case.

Text remains instantaneous in the distinct Text poll. Native message speed, held-OK acceleration, inline pauses,
row/page controls and visible growth/shrink are **not implemented by this increment**. The GUI still shows full
page text. The original timing reference is pinned PSX source, not a measured original-PC run. Before this change,
the runtime accepted an answer immediately: paired temporary TCP checks at the tunnel_2 duct stalled with0.2s
answers but reached sbwy4_1 with1s answers; traces showed the fast path dropping a request while Tifa's prior script
was active. Preserve native REQ drop semantics. The diagnostic1s delay is not a game policy or a native text model.
On r649, the same isolated checkpoint passes the default0.2s TCP replay through the duct to sbwy4_1, saved at
moment117; the zero-delay trace also reaches MAPJUMP164 without unsupported instructions. A fresh-state packaged
startup/client smoke passes. These are focused headless checks, not a full new-game playthrough or visual receipt.

For a focused destination rather than the highest story counter, the solver accepts `--until FIELD`. It exports a
route only if it reaches that field with control returned and a valid standing triangle, and appends `FIELD: wait 0`
so the TCP replay checks arrival even when the entire trip is automatic. Failure to reach the target exits nonzero
and writes no target route. This matters when several fields share one story moment, as on the next train journey.

**Earlier numbers were wrong.** The first two versions of the solver (2026-11-11) stood on any triangle and took any
gateway. They "reached" moments 78 and 197 by leaving Sector 7 through a gate a man stands in, and moment 33 by letting
the reactor's bomb go off. Nothing the solver reports counts until `scripts/live-route.py` has replayed its route
against a real `--field-runtime` server.

**What the original engine does that the scripts rely on** (each was found by the solver or the bot getting stuck):

- **The first allocated model is selected by default before initialization.** Explicit PC/CC instructions can
  override it; a field with no model remains unbound. Fields such as sbwy4_4 omit PC/CC but read the selected
  model's arrival triangle to start an entry ladder. The host's locked-scene player_model anchor remains separate.
- **Lines, pushes, talks and gateways** fire only for a player walking under its own control. A character the scripts
  hold or move triggers nothing.
- **A plain message** does not hold the player. Only a question does, or a script's own `UC`.
- **A window marked "cannot be closed"** (the countdown clock) is passive.
- **A finished movie** stands at its last frame for `MVIEF`.
- **Ladders** wait for direction keys in the original. A multiplayer host sends positions, so they climb by
  themselves here.
- **Background triggers** (doors that open as the player nears) are engine data, not scripts.
- **Solid models** stop the player, and not at the centre. The original tests three points a solid range (34 units
  at scale 512) ahead of where a step ends, straight on and an eighth of a turn to each side, against every solid
  model at half the two solid ranges. On contact the player does not move, and whoever was touched gets its contact
  script. So two guards 77 units apart close a gate, stepping away from someone is always free, and nobody slides
  round a character: the player steers round. A side point off the walkmesh counts as the wall pushing the player
  over (`FieldRuntime::player_step_blocked`, `player_bumped`). The server refuses such a step; the solver plans a
  corner beside whoever stands on its line when there is room, and the bot steps aside when it gets no nearer.
- **Lines are checked against where the player was a frame ago**, a few units back. A network player arrives in
  strides of a dozen units or more, so the runtime sweeps a stride in four-unit steps; otherwise a line (the second
  reactor ladder) is stepped over unnoticed.
- **Control handed to someone else while the player is held** (`CC` under `UC`) does not make the player that
  character. In the bar a child is lifted onto a shoulder and made the "controlled" model until the scene ends. The
  player's own character is the model that was controlled when the player last had control
  (`FieldRuntime::player_model`); hosts place and follow that one, and the cast leaves only that one out.

**Rollback (2026-11-11):** the format check (r560 as the old build, r601 as the new one) passes with a live-field
account. r560 reads the live account's v3 story, `index.log` stays at schema 4 (the same as the device), and the saved
spots resume after the round trip.
