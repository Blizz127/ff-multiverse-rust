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

## The headless solver (`examples/runtime_explore.rs`)

The solver plays the story on the runtime alone, with no server or client involved. From a fresh story it settles each
field, then probes every walkmesh triangle (standing, and with OK), every talkable model (facing it, with OK) and every
line (walking across it, and standing on it with OK). It takes the action that best advances the story or leaves the
field, carrying the story into the next field. Gateway choices are backtracked, and a probe that returns to a story
already seen in a field gets no credit.

**2026-11-11:** it reaches **story moment 51**, the passenger train into Sector 7, the same point the curated scene
route reaches. On the way it plays, from the original scripts alone:
- the opening and battle 300;
- the first reactor (door, lift, Jessie, bomb, Guard Scorpion 324, the countdown);
- the escape (md8_4 at 33, md8_1 at 36, md8_2 at 39);
- the bridge battles, the cargo car (48) and the train (51).

Teleporting the player to a triangle's centre is a test shortcut, not how a player moves. The live server path is
checked separately with `scripts/live-route.py`.
