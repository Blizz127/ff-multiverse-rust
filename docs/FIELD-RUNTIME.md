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
