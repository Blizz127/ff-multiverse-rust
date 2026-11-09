# FF Multiverse (Rust)

A new Rust client and server that turn single-player Final Fantasy games into a shared
multiplayer world: connected players in the same area, then parties, then shared battles.
The core is game-agnostic and **FF7 is the first game**; adapters for FF8, FF9, FF10 and
FF12 come later. Everything reads **your own, legally owned copy of the game data**. This
project ships no game assets and sends none over the network.

This repository is **proprietary and private** (see `Cargo.toml`, `license`). It is not
affiliated with, endorsed by or sponsored by Square Enix; Final Fantasy names and game
data belong to their owners.

## Legal and data rules

These are design constraints, not guidelines (details in `IMPLEMENTATION-PLAN.md`, section 1).

1. **Bring your own copy.** Only media and files you own and supply are read. Nothing is
   downloaded and nothing is bundled.
2. **Nothing is redistributed.** No asset bytes are committed to this repository, packed
   into a release, or sent between processes. The server never streams assets to clients:
   the server and every client each read their own local copy, and the protocol carries
   only ids, content fingerprints and provenance.
3. **Originals are read-only.** Install directories, disc images and original saves are
   opened read-only. All writable state lives in a separate state directory, `$FMV_STATE_DIR`
   or else `$XDG_DATA_HOME/ff-multiverse/` (`~/.local/share/ff-multiverse/`): the owned-content
   `library/`, the server's accounts in `server/`, a client's saved login tokens in `client/`.
   Account tokens are secrets (their files are owner-only, and the server stores only a hash).
4. **Modded installs are tagged `modded`**, read only and never written to. Compatibility
   with FFNx or 7th Heaven is an open question, not a promise.
5. **The `FIXTURE` area is synthetic and is not FF7 support.** It is a made-up test plaza
   used to exercise networking. It is labelled FIXTURE in the server's READY line, the
   client and the release notes. Only a real field decoded from owned media counts toward
   FF7 support.
6. **Editions are never mixed.** Assets from different editions, regions or revisions do
   not combine into one library set, and clients whose fingerprints differ from the
   server's cannot join that area.

Guards: `.gitignore` blocks `*.lgp`, `*.iso`, `*.cue`, `*.chd`, `*.ff7`, `*.mcr`,
`*.bin.orig` and the library and state directories, and `scripts/package.sh` packages an
explicit allowlist and fails if anything resembling owned data is present.

## Status

**Milestone 1 is done: one real FF7 field with two connected players.** The server hosts the
Sector 1 station platform (`md1stin`) decoded from your own Steam PC data, both clients read
their own copy, compare content fingerprints at the handshake and walk on the real walkmesh
(server-validated); the window draws the decoded background with the walkmesh projected through
the field's own camera. **Milestone 2 is partly in the game:** accounts with persistent avatar
profiles (M2-a) and parties of up to three with story-phase placement (M2-b) work end to end
(protocol v2), and **shared battles (M2-c/d, protocol v3)** run on the deterministic
`fmv-ff7-battle` engine: the server steps a battle for the whole party, each client replays it
from its own `scene.bin` and `kernel.bin`, and **every account keeps its own character progress
(level, experience, AP, gil; protocol v5)**, paid by won battles and saved per account, and
**battles have real commands (protocol v6)**: Attack, Magic, Item and Defend, with an inventory per
account, and **limit breaks and escape (protocol v7)**: a limit gauge that enemy blows fill and a
full one turns Attack into the character's limit break, and any character may try to run. The
synthetic **FIXTURE** plaza still exists
as a labelled test area; it says nothing about FF7 support.

| Milestone | Scope | State |
|---|---|---|
| M1 | One real FF7 field, two connected players, server-side walkmesh validation | done |
| M2-a | Accounts, persistent avatar profile, per-game progression store | done |
| M2-b | Parties (invite, accept, leave, kick, promote), phase-key instance placement | done (single phase) |
| M2-c/d | Shared battle, battle return, per-player rewards | done (battle subset; rewards paid into each account's own character progress and saved) |
| Later | FF8, FF9, FF10 and FF12 adapters on the same core | planned |

**Walking between fields (protocol v4).** The server hosts many fields: the entry field
(`md1stin`) is always loaded and any field behind one of its gateways is loaded the first time a
player walks through the gateway and unloaded a minute after the last player left it. Parties may
span fields, and the fields' NPCs (the characters a field's own data places: Sector 1's guards,
for example) are announced with every snapshot. See "Walking between fields" below.

Not done yet: field scripts (exits that scripts decide, such as the `MAPJUMP`s of story scenes,
are not followed; only a field's own gateways are), NPC animation and talking, music, and the
window's scrolling camera (it shows the whole background). The increment-by-increment plan is in
`IMPLEMENTATION-PLAN.md`.

## Workspace

Cargo workspace; crates live under `crates/`. The core never imports an adapter, so new
games plug in behind `fmv-adapter-api` without touching it.

| Crate | Role |
|---|---|
| `fmv-core` | Game-agnostic shared types: ids, positions, provenance, phasing, avatars. Never holds asset bytes. |
| `fmv-protocol` | Versioned wire protocol: messages and length-prefixed framing. Never carries asset bytes. |
| `fmv-adapter-api` | The trait boundary every game adapter implements. |
| `fmv-fixture` | FIXTURE adapter: a synthetic test area. **Not FF7 support.** |
| `fmv-server` | Authoritative server binary. |
| `fmv-client` | Client binary: window mode and headless bot mode. |
| `fmv-library` | Owned-content library: provenance manifest and content-addressed index. Never writes to original sources. Also the one resolver (`fmv_library::ff7`) that finds your FF7 field archive for the server and every client. |
| `fmv-import` | Owned-content import CLI: PC install scan now, sequential disc import planned. |
| `fmv-ff7` | FF7 adapter: format decoders (LGP, LZSS, field, background) and the FF7 area runtime. Reads owned data only. |
| `fmv-ff7-battle` *(coming)* | FF7 battle subset for the shared-battle milestone. |
| `fmv-persist` | Accounts, avatar profiles and per-game progression in an atomic, single-writer file store. |
| `fmv-party` | Party membership, leader and story-phase instance selection (pure state machines). |

## Quickstart

**Fastest path to the real thing (FF7, two players on one machine).** Import your own Steam
copy once, then start the server and a client or two. Nothing needs flags beyond these:

```
fmv-import import-pc "<the folder that contains data/>" --language en     # once
fmv-server --game ff7                                                      # terminal 1
fmv-client --game ff7 --name Cloud                                         # terminal 2: a window
fmv-client --game ff7 --name Tifa                                          # terminal 3: a second player
```

(For the Steam edition the folder is the one holding `data/field/flevel.lgp`, for example
`.../steamapps/common/FINAL FANTASY VII Steam Edition/ff7/workingdir`.) The server hosts
`md1stin` from the library `import-pc` wrote under `~/.local/share/ff-multiverse` and keeps its
accounts next to it; each client reads the same library and saves its login token next to it. In
the window, **WASD** runs (hold **Shift** to walk), **P** invites the nearest player, **Y**
accepts an invite, **L** leaves the party. A missing display, GPU or server is reported with one
clear line.

Without `--name` the window opens on the **character menu** instead (see "Window mode"): pick a
saved character, or press **N** to create one: `fmv-client --game ff7`.

Requirements for building: a recent stable Rust toolchain on Linux. Dependencies resolve from
the local cargo cache (`.cargo/config.toml` sets `net.offline = true`; remove that line when
adding new crates). A prebuilt binary needs a glibc at least as new as the machine that built
it (check with `ldd --version`).

**Build**

```
cargo build --release -p fmv-server -p fmv-client -p fmv-import
```

**Run the server.** It prints one READY line and then serves until stopped (SIGTERM or
Ctrl-C). `--bind 127.0.0.1:0` lets the OS pick a free port; the READY line tells you which.

```
target/release/fmv-server --bind 127.0.0.1:0
READY protocol=<v> build=<ver> addr=<addr> area=<id> fixture=<bool>
```

`fixture=true` means it is hosting the synthetic FIXTURE area. To host the real field from
your imported FF7 data (see "Importing your own FF7 data" below), add `--game ff7`:

```
target/release/fmv-server --bind 127.0.0.1:0 --game ff7
READY protocol=<v> build=<ver> addr=<addr> area=Ff7/md1stin fixture=false
```

`--game ff7` hosts `md1stin` unless `--area` names another field. It finds your library under
`$FMV_STATE_DIR` (else `~/.local/share/ff-multiverse`); `--ff7-library <state dir>` names
another one, `--ff7-set <id>` picks a set when the library holds several, and `--ff7-flevel
<file>` reads one `flevel.lgp` directly (read-only; add `--ff7-edition/--ff7-region/
--ff7-revision` so its labels match the other side). The server and every client must hold the
same edition: the area fingerprint binds edition, region and revision.

The server keeps **accounts** in a state directory it holds locked while it runs:
`--state-dir <dir>` (default: `server/` inside `$FMV_STATE_DIR`, else inside
`~/.local/share/ff-multiverse`). `--ephemeral` keeps accounts only until it stops, and
`--no-registration` refuses new accounts.

**Limits.** The protocol is not encrypted and the server is meant for a network you trust, but it
does not let one host hold it hostage either. Every limit has a flag (`--help` lists them; `0`
turns a limit off) and every refusal says why, in words and as a typed code (see below).

| What | Default | Flag |
|---|---|---|
| A connection must send `Hello` within | 3 s | `--hello-timeout-secs` |
| Connections one address may hold at once (handshaking ones count; an IPv6 /64 is one address, players behind one NAT share it) | 8 | `--max-connections-per-ip` |
| Accounts one address may create per 10 minutes | 10 | `--registrations-per-ip` |
| Accounts the whole server creates per minute | 120 | `--registrations-per-minute` |
| Accounts the store holds, then registration closes and logins go on | 100000 | `--max-accounts` |
| A joined client that sends nothing at all is dropped after | 30 s | `--idle-timeout-secs` |
| A joined client that only pings, with no game input, is dropped after | 30 min | `--inactivity-timeout-secs` |

The per-address limits go by the address the TCP connection comes from. A tunnel or proxy that makes
every player arrive from one address (an SSH forward, for example) shares one allowance between them:
raise or lift `--max-connections-per-ip` and `--registrations-per-ip` there.

*Registration limits count accounts that were created.* A registration is checked for its name
and look first, then takes a place in the per-address and server-wide limits, and only an account
that really comes into being uses the place up: a refused name, a name that is taken or a client that
hung up costs nobody a place, so a flood of junk registrations cannot lock real players out.

*Idle sessions.* Two clocks end a joined session. Silence (`--idle-timeout-secs`) is a peer that
vanished. Pinging without playing (`--inactivity-timeout-secs`) is a squatter: an account that only
answers pings would otherwise hold its slot forever, and 64 of them the whole server. "Game input"
is anything but a `Ping` (a move, a party request, a battle command); the reference client pings
every second and moves when its player walks. Whoever steps away for half an hour is told
"closed for inactivity" and logs in again with the saved token.

*Registration and its token.* The token is sent once, in the `Welcome` that answers the
registration. The account is created in one step with its avatar (a name is never taken without a
token having been produced), a client that has already hung up gets no account, and if the `Welcome`
cannot be written the account is released again, so the name is free for the next try, and once the
client answers it (its first message after the `Welcome`, whatever it asks for) the account is recorded
as claimed: `fmv-accounts release-older-than` then only ever frees a registration whose client never
answered, never a player who registered, played and has not logged in again. (A store written by an
earlier server has no such records: until each player logs in once with the saved token they look
unclaimed to `fmv-accounts`, so check `list` and release by id there instead of `release-older-than`.) A look
(`appearance_ref`) is only ever one of the ids the hosted game offers (`ff7/cloud`, ...); anything
else keeps the default look instead of failing the registration.

*Refusals are typed.* `Reject { reason, code }`: the `reason` is a sentence for the screen, the `code`
(`NameTaken`, `InvalidName`, `InvalidToken`, `RegistrationClosed`, `RateLimited`, `AlreadyOnline`,
`ServerFull`, `TooManyConnections`, `ProtocolMismatch`, `ContentMismatch`, `BadRequest`, `Timeout`,
`IdleTimeout`, `ShuttingDown`, `Internal`) is what a client acts on. It is additive within protocol
v3 (an older peer ignores it, a missing or unknown code reads as `Unspecified`).

*Logging.* Junk connections cannot fill the log: past a small budget per address and in total,
connection events are logged at the debug level and one line says so. Nothing is logged while the
world lock is held, so a stalled log reader cannot stall the players.

**Run two headless bots** (use `addr` from the READY line, the same `--game ff7` flags as the
server when it hosts FF7). The first time a bot connects it registers its `--name` as a new
account and saves the token it is given; each bot runs for the given duration and then prints
one SMOKE line:

```
target/release/fmv-client --server <addr> --name A --headless --duration 5
target/release/fmv-client --server <addr> --name B --headless --duration 5
SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n> party=<n> battles=<n> victories=<n> area_changes=<n> level=<n> exp=<n> items=<n>
```

`peers_seen` of at least 1 means that client saw the other player; `party` is the largest
party this client was in during the run (0 if none). Bots can form a party: give one
`--party-invite-first-peer` and the other `--party-accept`, and both lines show `party=2`.
`battles` and `victories` (appended in protocol v3) count the shared battles the client
entered and the server ended in victory; `area_changes` (area travel) counts the times the server
moved the client to another field; `level` and `exp` (appended in protocol v5) are the level and
total experience of the player's character as the server last reported them (`0` and `0` from a
server that reports none, such as the FIXTURE one); `items` (appended in protocol v6) is the total
count of the items the player carries, as the server last told it (`0` from a server with none).
These line formats are a contract: new fields
are only ever appended, and `scripts/smoke.sh` parses them by key (it ignores the fields it does
not know).

**Window mode** (needs a display):

```
target/release/fmv-client --server <addr>             # the character menu
target/release/fmv-client --server <addr> --name Me   # straight in as Me (registers it the first time)
```

*Character menu.* A window started without `--name` opens on a menu, in the same window the game
then runs in. It lists the characters saved for that server (name, colour swatch, look):
**Up/Down** choose, **Enter** plays, **N** creates a new one, **Del** forgets a saved login on
this computer (it asks first, and the default answer keeps it: the character stays on the
server, but its token, shown only once at registration, cannot be recovered), **Esc** quits.
With nothing saved yet it opens on the create screen: a **name** (up to 24 characters, cleaned
exactly as the server cleans it), a **look** (FF7: Cloud, Tifa, Barret or Aerith; the ids
`ff7/cloud`, `ff7/tifa`, `ff7/barret`, `ff7/aerith` are stored in the avatar profile and the game
draws the matching field model from your own copy, see "In the game"), and a **tint** (a palette grid plus red, green
and blue sliders; until you pick one it is the colour the command line would derive from the
name). **Tab** or **Up/Down** move between the fields, **Left/Right** change a value, **Enter**
registers. If the server refuses (name taken, registration closed, rate limited) the reason is
shown on screen and you can edit and try again; the first time a registration succeeds its
token is saved before anything else can fail. `--name` skips the menu (the old behaviour) and
`--headless` never shows it. The colour and look the menu shows for a saved login are what this
machine last saw (the server keeps the real profile and sends it on join), so a login that has
not joined from this machine yet says "look unknown".

*In the game.* WASD or the arrow keys move (up goes up the screen, whatever the camera looks
like); in an FF7 field you run, and walk while **Shift** is held (at the field's run and walk
speeds: hypotheses used for pacing only, the server's own cap is unchanged). **P** invites the
nearest player to your party, **Y** accepts the pending invite, **L** leaves the party, Esc
quits, **F1** shows or hides the walkmesh over the picture (hidden by default). The window shows
player names (over the head of a model, or the marker), your party (leader starred) in the top
right, the area name (`FF7 md1stin`), the connection status (players, round-trip time) and a
pending invite; a FIXTURE area wears the label `FIXTURE (synthetic, not FF7)`. The window title
still shows your party size and a pending invite.

*Characters.* In an FF7 field every player, you included, is drawn as the field model of the
look they chose (`ff7/cloud`, `ff7/tifa`, ... `ff7/vincent`), standing on the walkmesh at their
position and facing the way they face, with the idle, walk or run animation of what they are
doing. The models are read from **your own copy** of the game's `char.lgp` (found in the same
library set, or next to a `--ff7-flevel` file) and are never sent over the network: the wire
carries only the look's id. A look this table does not know is drawn as Cloud; every player gets
a ring on the ground in their tint (yours has a white outline, a party member's a gold ring
around it). Without a `char.lgp`, in a FIXTURE area, or for a character that fails to load, the
plain markers are drawn as before. The picture of an FF7 field is drawn **tile by tile with a depth
per tile**, in the same depth-tested pass as the models, as the game does it: a layer-2 tile is
`4 * id` units from the camera (the rule is derived from the game's own stored values and checked
on all 702 owned fields in `fmv-ff7`), layer 4 is in front of everything, layers 1 and 3 behind it,
and a character is hidden by exactly the tiles that are nearer than it is. In `md1stin` Cloud
stands fully visible in front of the far left wall piece that the old flat front layer covered him
with (39% of him at the test spot), and is hidden behind it where he is farther away; the glows,
shadows and glass (blended tiles) are drawn over the characters, which the flat split could not do.
It is checked against software composites to within two pixels at each spot. The depth of an
object is one value for the whole tile, so an object that reaches far in depth under one id is an
approximation; see `crates/fmv-client/src/avatar/mod.rs` for the measured numbers and the remaining
limits. A field with nothing in front of the characters, or a window with no models, shows the
flat picture as before.

*In a battle.* **B** asks the server for a debug battle (refused unless it runs with
`--debug-encounters`; random encounters start battles by themselves in a field that has an
encounter table). The window then shows the battle as a 3D scene: the formation's own camera
(from your `scene.bin`), the enemies where the formation puts them, your party opposite them
facing them, on the location's own model: all of them are the real battle models of your own
`battle.lgp` (stand-ins when it has none). They play the game's own clips: a party member runs up,
swings, and hops back (attack), gathers a spell with its cast clips (a glow, then a burst in the
colour of the spell's element: fire, ice, bolt, cure), holds its guard, flinches when it is hit and
falls when it is beaten, and rejoices when the battle is won; an enemy runs up and plays the clip
`scene.bin` names for the attack it uses, flinches with its hurt clip and, when beaten, fades away.
The presentation is only that: **nothing waits on an animation.** The bars, the menu and the
command that is applied follow the battle at once; the numbers float up when the blow lands (never
more than 0.6 s after the server said it), the clips are sped up (or their start skipped) so that
the blow's impact frame lands then, and a beaten enemy leaves the enemy list as it starts to fade.
An actor without real clips moves procedurally (a lunge, a blink, a collapse). A gold ring under a
character means it can act, a white ring marks yours. The
battle's own overlay is drawn over the scene (the enemy list, the party's HP, MP and ATB, the
command menu on your turn, the floating numbers and the pointer over the models' heads, and a
band across the top that is green while your client's replay agrees with the server and red if it
ever diverges); the arrow keys (or WASD) move in the menu, **Space** or **Enter** confirm and
**Backspace** goes back. The menu has Attack (which says **Limit** while your limit gauge is full),
Magic, Item, Defend and **Escape** (greyed out in a battle that cannot be run from). A machine without battle data to replay the battle (or one started with
`FMV_BATTLE_FLAT=1`) shows the flat view instead: one token per fighter with a thin HP bar. (The
battle scene has been checked by unit tests and offscreen renders only: this box has no display,
so the window itself was not run.)

**One command for the basics:** `scripts/smoke.sh` starts the server, runs two headless bots
concurrently, checks both SMOKE lines and stops the server. It uses `target/release` by
default; `--debug`, `--build` and `--bin-dir <dir>` are available, and `--help` lists the
rest. It runs the processes with throw-away XDG directories, so it never touches your real
library or state.

## Walking between fields

A field's gateways (up to twelve line segments in its trigger section, each naming a target
field and an arrival point) are its **exits**. The **server** watches every move it applies:
a move that crosses an exit line (from strictly one side of the line to the other or onto it)
takes the player out of the field. It is then *in transit*: nobody sees it, its moves are
ignored and it cannot start a battle. The server loads the target field if it must (from the
same owned archive; a target that cannot be loaded closes the exit), settles the arrival on
the target's walkable surface and offers it to the client (`AreaChange`: area, fingerprint,
path-free provenance, arrival position and heading). The client checks the offer against **its
own** copy of the field exactly as it checks the first field, loads it, answers `JoinArea` (or
`CannotJoin`, in which case the server sends it back where it stood) and the server answers
with the new field's `AreaSnapshot` (players and NPCs) and `InstancePlacement`. The players of
the old field hear `PlayerLeft`, those of the new one `PlayerJoined`; every broadcast reaches
only the field it is about. Unanswered offers time out like the first join. Nothing but ids,
fingerprints, positions and NPC names crosses the wire: the client draws the new field from
its own files.

Parties, accounts and battles belong to the world, not to a field: a party can be spread over
several fields (each member is placed in its own field with the leader's phase), and a battle
takes the party members that stand in the requester's field. A player in a battle cannot
travel. The window rebuilds its view (camera, background, walkmesh) on arrival; the HUD names
the field. **NPCs** are the characters whose placement is unconditional in the field's init
script (not player-bound, not hidden at start). The window draws each as the field model its data
names (the skeleton and the standing clip the field's model loader lists for it: the guards of
`md1stin`, the AVALANCHE members and the doors of `md1_1`) with the same renderer as the players'
models: standing still where the data puts it, facing the way it says, a shadow under it and its
name over its head, from **your own** `char.lgp` (only the two names cross the wire, and they are
only ever looked up as entries of that archive). A character whose model is not to be had here (no
`char.lgp`, a FIXTURE area, a model that does not load, or an area naming more than 24 different
models) is a marker with its name over it instead, never a stand-in character. `npc_appearance` in
`crates/fmv-client/src/npc.rs` is the one place that decides which. The offscreen test
`cargo test -p fmv-client -- --ignored offscreen_real_ff7_npcs_in_two_fields` (see
`crates/fmv-client/src/avatar/shots.rs` for the variables) draws `md1stin` and `md1_1` this way.

Headless bots: `--travel md1_1,md1stin` walks through the exits to those fields in order
(`area_changes=` at the end of the SMOKE line counts the moves; `peers_seen` counts the peers of
the field the bot ended in), a bot that has a party to form waits for it first, and bots
without `--travel` never walk across an exit by accident. `scripts/travel-smoke.sh` runs two
bots in a party from `md1stin` through its gateway to `md1_1` against your own library
(read-only, throw-away state) and back; `scripts/travel-smoke.sh --fixture` does the same on the
synthetic FIXTURE rooms (`fmv-server --game fixture-rooms`, `fmv-client --game fixture-rooms
--name X`: a hall and a yard joined by an exit each way, with one NPC in the yard; **not FF7**) and
needs no owned data.

Story phasing: every account has one story progression per game (the opaque `Progression` blob; an
account that never advanced has none, which is a new account) and the world keeps one instance of a
field per story phase, so players only see the players of their own instance. Solo, a player is in
the instance of its own phase; in a party, in its leader's, keeping its own progression. For FF7 the
progression is the main story counter `Var[2][0]` (`crates/fmv-ff7/src/story.rs`): each field's
phases are the intervals of the values its own scripts compare it against, every new account is the
empty phase of every field, and the story advances where the field's own script says it does and the
server can see it: in `md1stin` a player's character stepping onto walkmesh triangle 12 while the counter
is 1 sets it to 6 (the server logs `story advanced account=<n> entity=<n> area=Ff7/md1stin point=0
moment=1->6`; the script's battle after it is not run). A headless bot does it with `fmv-client
--headless --story-advance`. `scripts/phase-smoke.sh` runs it against your own library (read-only,
throw-away state): A advances and B does not, so they stop seeing each other; B joins A's party and is
placed in A's instance; the server restarts, both log in again, and the phases persisted.

Area travel is what **protocol v4** adds to v3: a v3 client does not know `AreaChange`, so the
version was bumped and a server refuses a v3 client at the handshake with a clear
`ProtocolMismatch` reject (instead of losing it at the first exit it crosses). Both sides must be
built from this version or later.

Not done: exits that scripts decide (FF7 `MAPJUMP`s and line triggers; they need the script VM
and the game's story variables), conditional NPCs, NPC animation and talking, and per-phase
instances (an area is one instance whatever the story phases of the players in it).

## Accounts, avatars and parties

**Accounts.** A client connects with a token or registers a display name. Registering creates
the account and its avatar profile on the server and hands the client its token **once**; the
client saves it under its state directory (`--state-dir`, default `client/` next to the
library) in `accounts/<server>.token`, owner-only, and uses it from then on. After
registration the server never trusts a client's name, tint or appearance: they come from the
stored profile, and `--name` is only needed to register. Names are unique (case-insensitive,
invisible formatting characters are stripped). `--register` forces a new account for `--name`.
A second account on the same server and machine gets its own file (the first file records
which name it belongs to). A token that stops working (the server's accounts were reset) is
reported with the file to delete. One session per account at a time.

**Parties.** Up to three players; the leader invites (solo players may invite too), anyone
invited can accept or decline within a minute, the leader can kick or promote, anyone can
leave, and a player who disconnects leaves. The server decides everything and tells each client
exactly the changes that concern it; a client mirror replays them. The party chooses where the
members are placed: the instance follows the leader's story phase (`InstancePlacement`), and a
member whose own phase differs keeps their progression but is told they may not advance the
story there. Today every player is in the single free-roam phase.

**Shared battles (protocol v3).** A battle starts for the whole party (or a solo player): from a
random encounter while walking in a field that has an encounter table (`md1stin` has one), or
when a player asks for one and the server runs with `--debug-encounters` (off by default). The
players in it are frozen in the field. The server owns the battle (`fmv-ff7-battle`'s seeded,
deterministic engine over its own `scene.bin` and `kernel.bin`), steps it, checks that every
command comes from the character's own player with a full gauge, and announces every step;
`EncounterStart` carries only the formation id, a seed, who plays which character at which level
and a fingerprint of the server's battle data, so each client rebuilds the very same battle
from its own files and replays it, and shouts `BATTLE DIVERGENCE` if its engine ever disagrees
with the server's. When it ends everyone is back in the field and `BattleEnd` carries the
rewards (experience and gil shared among the living, AP to each), which the server pays into each
member's own character progress (see below). A server without
battle data (the FIXTURE one, or `--ff7-flevel`) refuses every battle request cleanly.
`scripts/battle-smoke.sh` runs two bots through one real battle on `md1stin` and checks that both
members acted (the server logs the accepted commands per actor) and were told their inventory, and that each bot replayed the
battle from its own game files and matched (`verified by replay`, not merely "no divergence"). Its `--commands`
mode fires every command over real processes (see "Every command over real processes", below).

**Character progress (protocol v5).** Every account has its own progress in the game it plays: the
character its look names (Cloud unless it chose another look) and that character's level, experience,
AP and gil. A battle's members fight as their own character at their own level (the old rule that
gave the `i`-th member of a battle the `i`-th character of the game is gone), a victory pays each
member that fought on, and the server adds the payment to that member's progress with FF7's own
level-up rule, tells the player (`CharacterProgress`, which it also gets when it joins) and saves it:
a new account starts at its character's starting level and experience from the kernel (Cloud: level 6
with 610 experience, six short of level 7), nothing is written until its first win (or, since protocol v7,
the first battle that ends with a limit gauge worth keeping), and it is kept in
`<state>/server/progression/<account>/<Game>.character.json`, a record of its own beside, and
never mixed with, the story blob that selects the account's story phase (so a battle can never move a
player to another instance). The window shows it under the area name (`Cloud Lv 7  EXP 626/949
Gil 20`) and the headless bot's SMOKE line ends with `level=`, `exp=` and `items=`. The experience needed for
each level comes from the character's EXP curve in `kernel.bin` (`needed = modifier * level^2 / 10`
per level, the rule the community documents; checked against Cloud's known 6, 33, 94, 202 EXP for
levels 2 to 5 and against the shipped starting record). The store is game-agnostic: the game's caps
(level 99, 99,999,999 experience, 9,999,999 gil) are applied by the server. A payment for a player
who left mid-battle (its character fought on) is saved too; it is told nothing. Stopping the server
saves everything still queued first. `scripts/battle-smoke.sh --progress` runs the whole chain on
your real data: two bots fight, reconnect with their saved tokens, fight again at the level the
first win left them at, and reconnect once more to see what was kept (the progress and, since
protocol v6, the inventory: the starting stock once, plus what the wins dropped; since protocol v7, the
limit gauge: what the first battle ended with is what the second starts with, and every queued gauge
is written when the server stops).

**Battle commands and items (protocol v6).** A party member whose gauge is full gives Attack, Magic
(a spell it knows, for MP), Item (one the account owns) or Defend (physical damage halved until its
next turn). The engine (`fmv-ff7-battle`) plays them with FF7's own formulas decoded from the
owner's `kernel.bin` (spell, Cure, Life/Phoenix Down, potion and Grenade formulas; elemental
weakness, resistance, nullify, absorb and full-heal from `scene.bin`), and the enemies use their own
`scene.bin` attacks (a uniform pick among the damaging ones they have the MP for; the original's AI
scripts are not decoded). Spells: the magic materia a character's starting record equips, plus a
starter grimoire every character is given (Restore, Fire, Ice and Lightning at level 1: Cure, Fire,
Ice, Bolt), because on the Steam copy only Cloud and Red XIII start with magic materia. **The engine
knows no inventory**: the server checks the account owns the item, submits the command and takes the
item only when the engine accepted it, so every client still replays every battle from its own files
without knowing anyone's items. A new account (and an existing one that never had an inventory)
starts with the kernel's own starting stock (Potion x3, Ether x1, Phoenix Down x1) exactly once; the
inventory is saved inside the account's character record (older records without it still load and
are not rewritten until an item is used or found), and the server tells the player what it carries
when it joins, after every item used and after drops. A won battle drops items rolled by the server
from the enemies' own drop rates (`scene.bin`), dealt to the living members, logged and not part of
the replay. Known gaps: no statuses, no spells or items that hit a whole side, no summons, party
elemental resistances are not modelled (limit breaks and escape are in protocol v7, below).

**Limit breaks and escape (protocol v7).** Every character has a **limit gauge** (0 to 255) that enemy
blows fill (`damage * 76800 / (max HP * HP divisor)`; the divisor is the character's own, read from
`kernel.bin`: Cloud's is 140, so he fills at about 46% of his max HP lost; the constant is calibrated on
the community's documented figures and labelled an approximation in `crates/fmv-ff7-battle/src/limit.rs`),
a knockout empties and a limit break spends. With it full, the first command of the menu says **Limit**
where it said Attack, and the party panel draws a limit bar under each member's ATB bar. The limit of
each character is its first (limit level 1: Braver, Big Shot, Beat Rush, Healing Wind, Sled Fang, Greased
Lightning, Galian Beast, Boost Jump); its id, name and gauge divisor are the owner's own data, while the
**damage of a limit is not in the owner's files** (the kernel's attack data stops before the limit ids),
so it is the community's multiplier (Braver 3x a normal attack, Big Shot and Boost Jump 3.25x, ...), and
Healing Wind heals one chosen ally for half of its max HP (FF7: the whole party). Not modelled: limit
levels above 1 and the kills and uses that earn them, Cait Sith's Dice, Tifa's slot reels, Vincent's
transformation, whole-side limits. **Escape** is the last row of the menu (FF7 runs by holding L1+R1,
which this window cannot do cleanly): FF7's escape does not depend on levels but on the formation's *Run
Difficulty* (`scene.bin`'s escape counter, 1 for most fights) and a Run Check that succeeds one time in
four (always, after a preemptive strike or on a side attack); the actor spends its turn on four checks,
the difficulty that is left is kept, and a formation whose flag bit `0x0004` is clear (every counter of 9
or more in the owner's file: bosses and story fights) cannot be escaped at all. A successful escape ends
the battle for the whole party with `Escaped`: nothing is paid. The **gauge is saved at the end of every
battle**, whatever the outcome (escapes included), in the account's character record (a record of its own
is made for an account that has never won, so "nothing is written until its first win" now reads "until
its first win or the first battle that ends with a gauge worth keeping"); records written before v7 load
with an empty gauge. The headless bot gives its limit when the gauge is full (a healing one only to a
wounded ally) and never runs, unless a `--battle-script` tells it to (next paragraph).

**Every command over real processes (`--battle-script`, `--debug-limit-full`).** The headless bots only attack
(and heal below 30% HP, which the first enemies never bring about) and a limit gauge fills by only a few points
of 255 in a battle against them, so Magic, Item, Defend, Limit and Escape were verified by in-process tests
only. Two debug aids close that gap. `fmv-client --headless --battle-auto --battle-script <list>` makes a bot
give the commands of the list first, in order, one per full ATB gauge, and only then play as before. The
entries, separated by commas: `attack`, `defend`, `escape`, `limit`, `magic:<spell id>` and `item:<item id>`
(the game's own ids: Cure is spell 0, Potion is item 0, Phoenix Down is item 7; they were checked against the
owner's data and the client's name book), for example `--battle-script defend,item:0,magic:0,limit`. An
offensive spell, item or limit, and `attack`, aim at the first living enemy; a healing spell, item or limit at the
party member with the least HP left (the bot itself on a tie); a reviving one (Life, Phoenix Down) at a fallen
member. An entry the character cannot give at that moment (not enough MP, none of the item left, the limit
gauge not full, a battle that cannot be escaped, nobody fallen) is logged (`battle: script: ... skipped:
<why>`), counted and skipped for good, and the next entry is tried in the same turn. An entry is used up when
the server applies (or refuses) it, not when it is sent, so a slow answer cannot make the script lose one; the
script goes on across battles, and an escape can fail, so list it more than once. The closing
`headless: battle script: entries=<n> applied=<n> skipped=<n> refused=<n> lost=<n> unused=<n>` line says how it
went, and the `headless: battles:` line counts what the server applied for the bot's character by kind:
`(<n> attack, <n> magic, <n> item, <n> defend, <n> limit, <n> escape)`.
`fmv-server --debug-encounters --debug-limit-full` (the second flag is refused unless the first is given, and is
logged as a warning when the server starts) makes every member of every battle start with a **full limit
gauge**, whatever its account's is. It changes exactly two things: the gauge a battle STARTS with (the `battle started ...
limits=[255, 255]` line and every replay carry it), and the end-of-battle gauge save, so no limit gauge is ever stored: a gauge that
started from a faked full one is not the account's own, so the end-of-battle save is skipped (logged as
`limit not kept ... reason=debug-limit-full`) and every account keeps the gauge it really has, even when a
member spent its limit or was knocked out. A smoke run can never overwrite a real gauge. The server also logs
every Magic, Defend and Item it applies (`magic used`, `defend used`, `item used`) next to `limit used` and
`escape tried`.
`scripts/battle-smoke.sh --commands --build` uses all of it on the real field `md1stin` and owned data: one
server (with both debug flags) stays up through two runs of the same two bots; in the first they follow
`defend,item:0,magic:0,limit` and `magic:0,magic:9999,item:0,defend,limit` (spell 9999 does not exist: that entry
must be skipped and counted) and win, in the second both try `escape` until the battle ends in Escaped (formation
301 has run difficulty 1; one attempt gets away 68% of the time). It FAILS unless the server applied each of
magic, item, defend, limit and escape at least once (the bots' breakdowns and the server's own log lines must
agree), every battle was replayed by each bot from its own battle data with 0 divergences, both battles started
with `limits=[255, 255]` and the debug gauge was never stored. `FMV_SMOKE_SCRIPT_A`, `FMV_SMOKE_SCRIPT_B` and
`FMV_SMOKE_ESCAPE_SCRIPT` override the scripts (to try its failure paths).

`md1stin`'s own encounter table (section 6 of the field file, Steam PC build): the first of its
two 24-byte tables is the enabled one, with rate 128 (a roll succeeds with probability 128/1024)
and three formations whose weights add up to 64: 301 (weight 24, two enemies), 302 (22, one enemy)
and 303 (18, two enemies); the second table is disabled. The server reads every field's table the
same way (checked over all 702 fields of that build: every enabled table's weights add up to 64
and every formation exists in `scene.bin`). A starting party wins 301 and 303 every time (200 of
200 seeded runs each, both members attacking), which is why the smoke script uses 301.

**Protocol v7** (`fmv-protocol`) carries all of the above; its module docs describe the flow,
and golden JSON tests pin the wire format (the engine's own event, command and outcome types
included). A client or server of another protocol version is refused with a clear message:
v5 added `CharacterProgress`, a message a v4 peer cannot decode (the server sends it right after
every join), so the version was bumped like v4 was for `AreaChange`, and a v4 client is refused
at the handshake instead of being cut off at its first join; v6 added `Inventory` (also sent
right after every join), the new battle commands and events and the items in `BattleReward`, so a
v5 client is refused the same way; v7 added the `Limit` and `Escape` commands, the `LimitGauge` and
`EscapeFailed` events, `Outcome::Escaped`, three refusals and the starting limit gauge of every
battle member (`BattleMember::limit`, a required field: a v6 `EncounterStart` has none), so a v6
client is refused at the handshake. Both sides must be built from this version or later. **Rolling
back:** a server rolled back to a v6 build ignores the `limit` key of a record written by this
version and drops it the next time it saves that account (the character comes back with an empty
gauge, nothing else lost; a record whose gauge is empty is byte for byte what v6 writes); a v7 server
reads every v6 record (no `limit` key reads as an empty gauge). The inventory caveat of v6 still
applies: a server rolled back to a v5 build ignores (and drops) the inventory of a record and
refuses a record with more than about 70 stacks as too large, so roll back past v6 only from a
snapshot taken before any inventory was stored.

## Importing your own FF7 data

Point the importer at an installed PC copy of FF7 that you own. Your install is only ever
read; nothing in it is modified.

```
target/release/fmv-import scan-pc <dir>      # read-only: identify the edition and what is there
target/release/fmv-import import-pc <dir>    # record provenance and index it into your library
```

`<dir>` is the game's install directory. The result lives in the state directory (rule 3
above), never next to your originals and never in this repository. The server and each
client need their own local copy of the same edition: they compare content fingerprints
at the handshake and exchange no asset bytes. The Steam edition keeps its data under
`data/lang-<xx>/`; `import-pc` takes one language per library set (`--language en`).

## Packaging and smoke test

```
scripts/package.sh
```

Builds `fmv-server`, `fmv-client` and `fmv-import` in release mode, stages **only** the
three binaries, the launcher entry point `ff-multiverse.sh`, this README, a generated
`LICENSE-NOTICE.txt` and a `manifest.json` into
`dist/ff-multiverse-<version>-<gitshort>-linux-x86_64/`, and writes the `.tar.gz`, a
sidecar `.manifest.json` and `dist/SHA256SUMS` next to it. The manifest records the version,
the revision and launcher build id (`ff-multiverse-r<revision>-<gitshort>`), git commit
(`-dirty` if the tree is dirty), rustc version, target triple, build time and the sha256 of
each binary and file; the sidecar adds the tarball's sha256. The script prints the tarball
path, its sha256 and the build id, honours `CARGO_TARGET_DIR`, and can be re-run safely. It
fails if any staged file is off the allowlist or matches an owned-data pattern. It needs
`jq`. The package is also a private launcher port, see `docs/LAUNCHER.md`.

A name ending in `-dirty` came from uncommitted changes and one ending in `-prebuilt`
came from `--skip-build`, which packages existing binaries without compiling (meant for
testing the guard cheaply). Neither is a clean release.

To smoke-test the artifact that would ship, unpack the tarball and run
`scripts/smoke.sh --bin-dir <unpacked directory>`.

## Deployment

`deploy/` holds a systemd **user** unit template and the install, upgrade and rollback
procedure. The deployment host and port are **not assigned yet** and appear there as
UNKNOWN placeholders; see `deploy/README.md`.

## State directory layout

```
<state>/library/     owned-content library (fmv-import): blobs and one manifest per edition
<state>/server/      the server's accounts, avatar profiles, progression and character progress
                     (LOCK = single writer)
<state>/client/      a client's saved login tokens: accounts/<server>.token (mode 0600), and next to
                     each one a display-only note of how that character looked when last seen
                     (accounts/<server>.<name>.look: tint and look, no secret; safe to delete)
```

`<state>` is `$FMV_STATE_DIR`, else `~/.local/share/ff-multiverse`. Back up `server/` by
stopping the server and copying it (a hot snapshot API exists in `fmv-persist`, not yet wired
to a command). Never commit or share any of it.

## Repository layout

```
crates/                  the Cargo workspace (see the table above)
scripts/package.sh       build and package a release tarball (and dist/SHA256SUMS)
scripts/ff-multiverse.sh the launcher entry point shipped in the package (docs/LAUNCHER.md)
scripts/smoke.sh         two-bot local smoke test
scripts/battle-smoke.sh  shared-battle smoke test (real data; modes --fixture --random --progress --commands)
scripts/travel-smoke.sh  walking between fields: two bots, md1stin -> md1_1 and back (real data)
scripts/phase-smoke.sh   story phasing: A advances md1stin's story and B does not, party, restart, persist (real data)
deploy/                  systemd user unit template and deployment procedure
docs/LAUNCHER.md         the package as an owner-only launcher port: layout, entry point, data dir
IMPLEMENTATION-PLAN.md   first-milestone plan and design constraints
```
