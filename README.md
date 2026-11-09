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
full one turns Attack into the character's limit break, and any character may try to run, and
**every player has one original character of their own (protocol v8)**: a saved identity, a
per-game appearance binding (a generic placeholder body in FF7), a fighting style (the stat curve of a
canon party member, separate from the canon story roles) and pointers to its progress, and
**the story's own party fights beside the players (protocol v9)**: the canon Cloud and Barret of the first
reactor mission are in every battle as companions the server commands, and each player's character is ADDED
after them, in its own body, replacing nobody, and **a spell, an item or a limit can hit a whole side (protocol
v10)**: an ALL place for the target pointer, FF7's All materia rule and the 2/3 split, and **battles have status
effects (protocol v12)**: Poison, Regen, Sleep, Stop, Paralysis, Silence, Haste, Slow, Barrier, MBarrier,
Sadness, Fury and Darkness, inflicted and cured by the game's own spells, items and enemy attacks. The
synthetic **FIXTURE** plaza still exists
as a labelled test area; it says nothing about FF7 support.

| Milestone | Scope | State |
|---|---|---|
| M1 | One real FF7 field, two connected players, server-side walkmesh validation | done |
| M2-a | Accounts, persistent avatar profile, per-game progression store | done |
| M2-b | Parties (invite, accept, leave, kick, promote), phase-key instance placement | done (single phase) |
| M2-c/d | Shared battle, battle return, per-player rewards | done (battle subset; rewards paid into each account's own character progress and saved) |
| M2-e | Player characters: identity, per-game binding, fighting style (a stat curve) separate from canon roles, pointers to progress (protocol v8) | done for the slice (FF7: generic placeholder bodies, fixed styles; `scripts/slice-smoke.sh`) |
| M2-f | The story's canon party fights beside the players as server-controlled companions; players are added after them (protocol v9) | done for the first reactor mission (FF7: Cloud and Barret; `scripts/battle-smoke.sh`, `scripts/slice-smoke.sh`) |
| M2-g | Whole-side targeting: spells, items and limits can hit one target or a whole side, the party side being the canon companions and the players (protocol v10) | done for spells, items and limits (FF7: the All materia rule, the 2/3 split; the enemies' own AI still picks one party member; `scripts/battle-smoke.sh --commands`) |
| M2-h | Status effects: the statuses of the game's own spells, items and enemy attacks, their clocks, what they do to turns, damage and hit rate, cures, panels and body cues (protocol v12) | done for 13 statuses (the engine's `status` module lists what is left out: Confusion, Berserk, Reflect, Petrify, Frog, ...; `scripts/battle-smoke.sh --status`) |
| Later | FF8, FF9, FF10 and FF12 adapters on the same core, in that order (the owner's roadmap; FF13 maybe later) | planned |

**Walking between fields (protocol v4).** The server hosts many fields: the entry field
(`md1stin`) is always loaded and any field behind one of its gateways is loaded the first time a
player walks through the gateway and unloaded a minute after the last player left it. Parties may
span fields, and the fields' NPCs (the characters a field's own data places: Sector 1's guards,
for example) are announced with every snapshot. See "Walking between fields" below.

Not done yet: field scripts as a whole (what is carried out is the exits a line's walk-on script decides, the
ladders and jumps of the lines, and the story steps the scripts make where the server can see them: a walk, a press of
OK, a talk, a scripted battle, see "Walking between fields"; no cutscenes), NPC animation and conversations beyond the
story steps, the field sounds that no script names (footsteps, and the sounds of cutscenes and of talks; the music, a battle's
sounds and what the fields' scripts play by themselves and for a ladder, a jump or the lift are done, see "Sound"), and the
window's scrolling camera (it shows the whole background). The increment-by-increment
plan is in `IMPLEMENTATION-PLAN.md`.

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
| `fmv-import` | Owned-content import CLI: PC install scan (the field and battle data, and the music and sound files when the install has them) now, sequential disc import planned. |
| `fmv-ff7` | FF7 adapter: format decoders (LGP, LZSS, field, background), the FF7 area runtime, the music track list and the field scripts' music commands, and the sound-effect files. Reads owned data only. |
| `fmv-ff7-battle` *(coming)* | FF7 battle subset for the shared-battle milestone. |
| `fmv-persist` | Accounts, avatar profiles, player characters (identity, per-game binding and slot, pointers to progress) and per-game progression in an atomic, single-writer file store. |
| `fmv-party` | Party membership, leader and story-phase instance selection (pure state machines). |
| `fmv-audio` | Audio engine: a music and effects mixer (crossfades, loop points, volumes, mute), a null sink for machines without a sound card, the cpal output device, and decoding of Ogg Vorbis, WAV and ADPCM music with its loop tags. Game-agnostic; the client plays it (see "Sound"); it holds no audio data. |

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
accepts an invite, **L** leaves the party, **M** mutes the sound (**-** and **=** change the volume). A
missing display, GPU or server is reported with one clear line.

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
unclaimed to `fmv-accounts`, so check `list` and release by id there instead of `release-older-than`.) A body
(`Hello::character.body`) is only ever one of the ids the hosted game offers; anything else is the default
body instead of failing the registration, while a fighting style (`Hello::character.slot`, the id of a stat curve) the
game does not offer to a new character (FF7: Cloud's curve, or none that exists) is refused, because it would fix how
the character fights for good.

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
**Up/Down** choose, **Enter** plays, **N** creates a new one, **Del** deletes the character (it asks first, and
the default answer keeps it: **Y** deletes it ON THE SERVER, with its own login token, which frees its name and
removes it from the server; the server keeps a recoverable copy that only its operator can restore, see "Deleting a
character"; **F** only forgets the saved login on this computer, for a server that is gone; **N** or **Esc** keeps
it); the saved login is removed only after the server says it deleted the character or no longer knew it, and a
character that is in the game right now is refused), **Esc** quits.
With nothing saved yet it opens on the create screen: a **name** (up to 24 characters, cleaned
exactly as the server cleans it), a **body** (a generic placeholder model of your own `char.lgp`,
the field look of your character, see "In the game"), a **tint** (a palette grid plus red, green
and blue sliders; until you pick one it is the colour the command line would derive from the
name) and a **fighting style** (FF7: Gunner, Martial artist, Mage, Guardian, Ninja, Gambler, Marksman or Lancer:
how you fight, meaning the stats, spells and level curve, fixed once the character is created; the screen names
no canon character, and the Swordsman's, Cloud's own curve, is not offered; see "Your own character"). **Tab** or
**Up/Down** move between the fields, **Left/Right** change a value, **Enter**
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

*Talking.* In an FF7 field, **Enter** or **Space** (the controller's **A**) next to a character, facing
it, opens that character's own dialogue in a blue window at the bottom of the screen. The text is the
game's, read by your client from your own copy of the field's script and shown exactly as written (the
names in it are the game's default names: whoever you play is never written into a line about Cloud
or the rest of the cast; nothing about dialogue goes over the network). **Enter**, **Space** or **A**
turns the page, goes on to the next window and answers a question (**Up/Down** or **W/S** move the
pointer), **Backspace**, **B** or **Esc** closes the window, and you stand still while it is open. A
character's talk script is run only as far as the messages it shows (`fmv_ff7::script::dialogue`): what
it would do besides (give an item, start a battle, move you to another field) is not done, and until a
story source is plugged in its branches are read as at the start of a new game. A character with
nothing to say there (hidden, silenced, no talk script) does nothing when you talk to it.

*Characters.* In an FF7 field every player, you included, is drawn as the binding the server holds for
them: a generic placeholder body (`ff7/body/lean`, `ff7/body/sturdy`, `ff7/body/slight`) tinted with their
colour, never the model of a canon character, whatever stat curve they fight with (an account that predates
protocol v8 keeps the canon look it chose: `ff7/cloud`, `ff7/tifa`, ... `ff7/vincent`), standing on the walkmesh at their
position and facing the way they face, with the idle, walk or run animation of what they are
doing. The models are read from **your own copy** of the game's `char.lgp` (found in the same
library set, or next to a `--ff7-flevel` file) and are never sent over the network: the wire
carries only the binding's id. A binding the tables do not know is drawn as Cloud; every player gets
a ring on the ground in their tint (yours has a white outline, a party member's a gold ring
around it). Without a `char.lgp`, in a FIXTURE area, or for a character that fails to load, the
plain markers are drawn as before.

*The player's colour on a model (field and battle).* The colour is not multiplied into the model any more (that
left a dark blue cloak under an orange player a darker, greyer blue: `[57,66,109]` came out `[55,47,58]`, and
skin was pulled to brown). It is carried per DRAW (an `Identity`, a second instance-rate vec4 next to the tint:
a model is never copied per player, because the client's GPU cache holds 32) and the fragment shader repaints
the **cloth** in the colour's hue and saturation at the cloth's own value, lifted (`0.2 + 0.8 * v^0.6`) so that
dark cloth shows it while the shading, belts and shoes stay told apart, and bright cloth stays a paler pastel;
**skin-hued texels** (a smooth hue, saturation and value mask), the **face decals** and the **hair** keep their own
colours under a 5% tint (hair is dark brown, very saturated and very dark, which the skin mask's gates exclude, so a
blue player's `lean` used to have blue hair: the mesh now flags the head bone's vertices of a skin or hair hue, and its
dark greys, as the model's own colours, `avatar::identity::head_colour_is_own`; the same colours on the shoes or the
suit are still cloth, and a cap's white or a hood's blue is cloth too); white, grey and black colours fall back to the old multiplicative look, and a model without an identity
is drawn bit for bit as it always was (enemies, canon models, non-player characters, effects and fades are
untouched). A CPU mirror of the look is the test oracle and the shader agrees with it on 99.6% of 110 116 body
pixels (the rest are face decals); an sRGB target and a plain one show one picture (99.89% of pixels within 4
levels). With a blue player: the dark cloak `[57,66,109]` becomes `[54,91,164]`, dark suit grey `[48,48,52]`
`[46,71,120]`; with an orange one `[157,86,43]` and `[118,68,34]`; skin `[230,180,150]` moves by 5 levels at most
(the old tint moved it by 28 to 57). At 1x two players' bodies are told apart at a glance (mean colour distance
of two bodies 25 of 441 before, 119 after). **In battle** the PLAYERS' bodies (not the canon companions Cloud and
Barret, who have no player and keep their own colours; not any sword, not the enemies, not the location) wear the
colour of the player each member is, the same look: a blue player's placeholder body is blue clothes with its own
skin, an orange player's is orange (a colour that is not known yet, for instance before that player's snapshot
arrived, leaves the model as it was and is picked up as soon as it is). Limits: pure black cloth (all channels 0) only gets a very dark shade of the colour (near-black cloth,
which is what the models contain, is lifted plainly); an orange or peach colour sits near skin hue, so those
players' pastel clothes stay told apart from their skin by saturation and brightness only; and a head's cap, hood or
helmet that is grey, white or a cool colour is repainted with the clothes (the `sturdy` body's hood is; its visor and
red marks are on the head bone, in warm and dark colours, and keep their own). The pictures come from the real-data tests (`cargo test -p fmv-client --
--ignored offscreen_field_identity --test-threads=1`, `offscreen_battle_identity_before_after`, with
`FMV_FF7_LIBRARY` and `FMV_SHOT_DIR`): each body in four colours as untinted | old | new from one frame, a 1920x1080
overview, and the battle before and after from one scene at one instant (54 339 pixels differ, all of them on the
two players' bodies: the canon pair, the enemies and every weapon are bit for bit as they were).

*Faces and scale of the field models (measured 2026-11-09).* Every character of `char.lgp`, the cast and the
three placeholder bodies alike, gets its face from three decals on the head bone: two eye quads (the same eye
twice) and a mouth. The data shifts a decal's texture coordinates by whole numbers (the right eye's `u` runs 1.00 to
1.97, a mouth's `v` 5.00 to 5.94), which the game's wrap-around addressing ignores; this client's sampler
clamps (so a decal at the edge of its texture does not blend with the opposite edge), and a clamped coordinate past
1 reads the transparent edge, so **the right eye and the mouth of every model were not drawn: one eye, Cloud's included**. The
mesh merge (`avatar::mesh`) now takes the whole number off a textured part whose coordinates lie in one cell (a part that
really tiles is left as it was): every model has both eyes and its brows, in the field and in battle (the extras'
mouth textures are blank in the data, as `lean`'s and `slight`'s are; Cloud's has a mouth and has one now). The
`sturdy` body's face is covered by a visor by design. The bodies are the cast's size: the idle pose is 28.7
(`lean`), 30.4 (`sturdy`) and 29.2 (`slight`) model units against Cloud's 33.2 (86%, 91%, 88%; Tifa is 86%, Yuffie 80%)
and Jessie's 30.8, and every model is drawn at the field's one model scale, so at one place the bodies are 82 to 86% of
Cloud and 92 to 97% of Jessie on the screen (in battle 0.86, 0.88 and 0.91 of Cloud's battle model for `lean`, `slight` and
`sturdy`); a player who looks a
quarter taller than a character stands nearer to the camera. `slight`'s orange skin (255,131,80) is the model's own
colour (a tanned beach extra), kept by the colour look; it is not the tint. The pictures are
`offscreen_field_lineup_bodies_beside_cloud_and_an_npc` (Cloud, the field's character and the three bodies at one
spot, front and three-quarter, their own colours over the player look, orange and blue) and the numbers
`real_looks_survey_heights_parts_and_faces`; `fmv_ff7::playable` has the table.

The picture of an FF7 field is drawn **tile by tile with a depth
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
(from your `scene.bin`), the enemies where the formation puts them, the party opposite them
facing them (the story's canon party first, then each player: up to five), on the location's own model: the enemies
and the canon companions are the real battle models of your own `battle.lgp` (stand-ins when it has none) and each
player is its OWN body, the placeholder body and colour it has on the field, moving procedurally (a lunge, a hop back,
a flinch, a fall; a canon battle model on a player would be a bug). The real models play the game's own clips: a
party member runs up, swings, and hops back (attack), gathers a spell with its cast clips (a glow that grows at its hands, then the spell's effect breaks over each of
its targets: see "Battle effects" below), holds its guard, flinches when it is hit and
falls when it is beaten, and rejoices when the battle is won; an enemy runs up and plays the clip
`scene.bin` names for the attack it uses, flinches with its hurt clip and, when beaten, fades away.
The presentation is only that: **nothing waits on an animation.** The gauges, the menu and the
command that is applied follow the battle at once; the numbers float up when the blow lands (never
more than 0.6 s after the server said it) and **so do the HP and MP in the party panel, the HP of the enemies' bars and
tokens and the names of the fallen** (each value changes at the instant of the performance that changes it, so a counter's
damage is on the panel when the counter lands, not when the message that carried it arrived; **so are the status tags** (a
Sleep's tag comes up when its spell breaks, a cure takes its tag off when it lands, a status that runs out goes with the short
beat of the numbers, a knockout takes the tags with the collapse) **and the 3D bodies' colours, droop and held pose take their
statuses from those very tags**; the gauges still follow the engine at once), and so does the blow's sound (see "Sound"); the clips are sped up (or their start skipped) so that
the blow's impact frame lands then, and a beaten enemy leaves the enemy list as it starts to fade.
An actor without real clips moves procedurally (a lunge, a blink, a collapse). A gold ring under a
character means it can act, a white ring marks yours. The
battle's own overlay is drawn over the scene (the enemy list, the party's HP, MP and ATB, the
command menu on your turn, the floating numbers and the pointer over the models' heads, and a
band across the top that is green while your client's replay agrees with the server and red if it
ever diverges); the arrow keys (or WASD) move in the menu, **Space** or **Enter** confirm and
**Backspace** goes back. The menu has Attack (which says **Limit** while your limit gauge is full),
Magic, Item, Defend and **Escape** (greyed out in a battle that cannot be run from). A spell, item or
limit that can hit a whole side has one more place for the target pointer, **ALL** (one step past the
last target, with the same arrow keys: there is no extra key; Healing Wind and Megalixir start on it).
A machine without battle data to replay the battle (or one started with
`FMV_BATTLE_FLAT=1`) shows the flat view instead: one token per fighter with a thin HP bar. (The
battle scene has been checked by unit tests and offscreen renders only: this box has no display,
so the window itself was not run.)

**Battle effects.** Every performance has a picture in the 3D scene, made of procedural glows that our renderer draws (seven small
additive meshes shared by all of them; none of the game's own spell animations is used or recreated and no game texture is). What
it is comes from the action's own data in the replayed engine. A blow's attack record decides how it strikes (`CUT` slashes, `HIT`
thumps, `PUNCH` bursts; a critical hit is bigger and gold; a miss is a small whiff beside the target). A record with the `SHOOT`
bit (a gun, a laser, a thrown rock: the MPs' Machine Gun, the Guard Scorpion's Tail Laser) flies a bolt from the shooter to each
target it was aimed at and leaves a line that fades (a party member whose weapon is long-range shoots too). A spell breaks by its
element mask: fire, ice, bolt, earth, wind, water, poison, gravity, holy and none each have their own colours and shapes; a Cure,
an Ether and a Phoenix Down have theirs. Every status the rules play has a sign when it is inflicted (Zs for Sleep, bubbles for
Poison, a clock for Slow, a shell for Barrier and another for MBarrier, speed lines for Haste, a spiral for Regen, ...) and a
wipe when it is cured or runs out. An item is tossed to its target on an arc and takes effect there; a limit break gathers
longer, breaks bigger and flashes over the whole picture; a guard shimmers; a spell or an item that found nobody to land on
fizzles at its performer. A spell or an item on a whole side shows on every target. Each effect begins with its performance (a
counter's with the counter, never with the batch), peaks at the landing, the very instant the number is born, and fades out over
about a second. Each particle is a pure function of the clock and a seed, so nothing is stored or allocated per particle; an
effect is at most 28 pieces and a frame at most 240 (the newest effects win), which costs about 0.3 ms of CPU with nothing on the
screen and 0.8 ms with the cap drawn (`a_frame_with_every_effect_at_its_peak...`, measured on a busy machine). Known gaps: a ring
is drawn standing up, not on the floor; a bolt leaves the middle of its shooter's chest, not the muzzle; effects are hidden by
bodies nearer the camera but not by the location's walls; the gold ring of a party member that has just been put to sleep drops at
once while its droop waits for the landing (the gauges and the menu follow the engine); every party weapon's blow slashes (the
weapons' own elements are not modelled); an enemy that shoots (the `SHOOT` bit) no longer runs up to its target when it has no
clips to say how it moves (it stands and fires). The pictures: `cargo test -p fmv-client --bin fmv-client -- --ignored
offscreen_battle_effects --test-threads=1` draws every effect on a stage with no game data (`fx-blows`, `fx-elements`,
`fx-restoring`, `fx-statuses-curses`, `fx-statuses-buffs`, `fx-staging`), and with `FMV_FF7_LIBRARY` and `FMV_SHOT_DIR` it plays the
real fights and writes `fx-fire-landing`, `fx-ice-landing`, `fx-fire-all-enemies`, `fx-cure-party`, `fx-limit-gathering`,
`fx-limit-break`, `fx-enemy-laser-flight`, `fx-enemy-laser-hit`, `fx-status-before-landing`, `fx-status-landing` and
`fx-statuses-haste-and-fury`, each at 1100x680 and 1920x1080.

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
needs no owned data. `scripts/travel-smoke.sh --chain` walks the whole first reactor mission, from the first field
to the reactor's core and the Guard Scorpion (`md1stin`, `md1_1`, `md1_2`, `nrthmk`, `nmkin_1`, `elevtr1`, `nmkin_2`,
`nmkin_3`, `nmkin_4`, `nmkin_5`), with the real story: see below. `scripts/travel-smoke.sh --escape` goes on past the
boss: the party turns round and climbs back out of the reactor with the bomb's countdown running, rides the lift back up
and takes the finale in `nrthmk` (see "After the Guard Scorpion").

The bots find their way with `crates/fmv-client/src/nav.rs`: a grid over the field, A* over its cells, string pulling
into a few straight legs that the server's own `validate_move` accepts, and never across the line of an exit they do
not want. The nodes keep the **height** a walker has at them, so a surface with several levels over one spot is
planned level by level (`nmkin_2`, the way down from the lift, is a stair of four levels joined by ramps about 50
units wide: a coarse grid first, finer ones where the surface is narrow). The bots plan on what `validate_move`
says and nothing else, so what they can walk is exactly what the server lets them walk.

**Exits that scripts decide, and the story along the way.** Besides its gateways a field has the exits its
scripts run when a player walks onto a line (a `MAPJUMP` in a line entity's trigger script, with the conditions
it sits under); the adapter reports them (in the reviewed fields, see below) after the gateways (`ExitKind::Script`) and answers, for an account's
story, which of them is open (`AreaRuntime::exit_state`). Several can share one line: the lift of the first
reactor (`elevtr1`) has no gateway and four exits on one line, picked by a bit of `Var[1][225]` and by
`Var[2][0] < 33`, so the way down to `nmkin_2` is open only after the ride and the later story's `smkin_*` stay
closed because their conditions say so. **A script exit of a served field that leads to a field that is not reviewed is
closed whatever the story says** (from moment 33 the lift's data says `smkin_1` / `smkin_2` are open; no server offers
them until those fields are reviewed: `Ff7Adapter::load_field` closes them, the survey's loader shows what the data
holds). Gateways keep their behaviour (`nrthmk` to `md8_4` is how a party reaches the end of the reviewed slice on
purpose). The server judges them against **the story of the account that governs
the crossing player's instance** (its own, solo; the leader's, for a party member), takes the first exit that
is open, and walks a player over a line whose exits are all closed. An exit whose condition the model cannot judge
(a variable the progression does not carry, a key to press, the party) stays closed and the server says so once per
area (`exit closed: its conditions cannot be judged ...`). Rules and numbers: `crates/fmv-ff7/src/exit_gate.rs`.
**A member follows its leader's way out.** A member is judged on its leader's story, and that moves while the member is
still on its way: the leader's finale in `nrthmk` takes the story from 27 to 33 and a member that comes to the lift a few
seconds later would be judged on moment 33 (found by `travel-smoke.sh --escape` with the member 18 s behind: it was sent
to `smkin_2`). So when a party's leader leaves an area through an exit its story decided, the world remembers which
(`crates/fmv-server/src/world/follow.rs`: per leader, the last 8 areas), and a member of that party that crosses the same
line in the same area takes THAT exit, once, whatever the story now says (the server logs `player follows its leader's
way out ... story_says=<where the story would have sent it>` when that differs). It is not a story step and changes
nobody's progression. The way is forgotten when the leader comes to stand in that area again, when its next way out of the
area replaces it, when the party's leader changes or the party is disbanded, and with the leader leaving the world; it
never applies to a player outside the party it was made for.

**The OK button, ladders and jumps (protocol v14, `ClientMsg::Interact`).** The server sees where a player walks, and
since `Interact` it sees the OK button too (Enter or Space in the window, one message per press, with the character the
client addressed, if any). What a press does is the world's call from where the server holds the player
(`crates/fmv-server/src/world/press.rs`): next to the character it names, a talk (below); standing in a press triangle
(the lift's panel), a ride; touching a ladder's line, a climb. **Links** are the ladders and jumps of a field: lines whose
scripts put the player's character somewhere else in the SAME field (`crates/fmv-ff7/src/links.rs` walks each line's
trigger scripts, once as walking up to the line and once as a press of OK, and what they do to the player's entity is the
link; a script that reads the story, reads back a place it cannot know or does not end normally is dropped, not guessed).
A walk-on link fires when the player walks up to its line, a link that waits for OK when the player presses OK touching it.
The server puts the player at the destination (on the destination's own floor) and tells the client with `MoveRejected {
corrected }`, the message that already puts a client where the server says it is: no wire change for it. Two rules keep
that honest. A link is **armed** only while the player is farther than 30 units from its line, and a player placed
anywhere starts with every line within reach disarmed, so a landing next to a line does not send the player straight back
(`fmv_adapter_api::LinkArming`; the first jump of `nmkin_3` lands 23 units from the second, the stair of `nmkin_4` within a
unit of the ladder back up). And the player is **fenced** after a link: until one of its intents agrees with the new place
(within 60 units, on its level) or two seconds pass, every intent is answered with the destination and moves nothing, and a
press does nothing, so the intents already on their way from the old place cannot walk the player on over a change of level.

The story advances along the way where the scripts advance it and the server can see it: a walkmesh triangle, a line the
player walks across, the player coming to stand in a field whose own scripts move the story by themselves, a press of
OK on the lift's panel, a talk, or a scripted battle won (`crates/fmv-ff7/src/story.rs`). The first mission's steps: 1->6
(`md1stin`), 6->7 (`md1_1`), 7->8 (`md1_2`), 8->10 (`nrthmk`), 10->11 (`nmkin_1`, a line), 11->12 together with the lift
bit (`elevtr1`: press OK on the panel's triangle), 12->13 (`nmkin_2`), 13->14 (`nmkin_3`: talk to the woman), 14->15
(`nmkin_5`: talk to the materia on the floor), 15->27 (`nmkin_5`: the core's triangle starts the Guard Scorpion, formation
324, and the step is paid when the party WINS); and the way out, after it: the second talk to the woman of `nmkin_3` (a tracked bit turned on, the moment stays 27), the
lift's ride back up (`elevtr1`, a tracked bit turned off, the moment stays 27) and 27->33 (`nrthmk`: the finale, see "After
the Guard Scorpion"). A **talk** point is a character whose own talk script (or one it requests)
writes the story: what the talk does is not read off the text, the character's script is run (the dialogue walker) for the
story the player has when it presses OK, after the character's own set-up has decided whether it can be talked to at all
(the materia switches itself off once the story has moved to 15). The client's conversation shows the lines for the story
as it was BEFORE the step; the server applies the step on the press and the mirrored story moves on after. A **scripted
battle** is a `BATTLE` the block starts before its write (the director's loop in `nmkin_5` asks the characters' scripts to run
the cutscene, which fights): the server starts it for the party, a battle point fires only by coming to it from outside, a
lost fight leaves the story where it was, and the step waits for the win. The progression blob is `[1, lo, hi]` until a
step sets a variable beyond the moment, then `[2, lo, hi, n, (bank, address, value)*n]` (an old blob loads exactly as
before; a build that predates version 2 reads a version 2 blob as unreadable and leaves it alone).

**Only ten fields are reviewed: the allowlist.** The script exits, story points and links above are **active in the ten
fields of the first mission only**: `md1stin`, `md1_1`, `md1_2`, `nrthmk`, `nmkin_1`, `elevtr1`, `nmkin_2`, `nmkin_3`,
`nmkin_4` and `nmkin_5` (`fmv_ff7::REVIEWED_FIELDS`, in `crates/fmv-ff7/src/area.rs`), each read against its own script
case by case. Every other field offers what it always did, its gateways (data, always open, so a party can still walk out
of the mission) and its phases, and leaves its script exits, story points and links off: a point raises a story for good, a
story only moves forward, a link teleports a player, and nobody has read the others (the data holds 917 script exits, 151
points and 190 links; the ten fields offer 4, 15 and 11, and the other 310 fields that have any leave 913, 136 and 179 off.
The points were 133, 14 and 119 in 299 fields until the writes of a script that a main loop requests were read, which
found 18 more entry points: `nrthmk`'s finale and seventeen in fields that are left off).
The server says what a field left off once per area when it loads it (`story left off (said once for this area)
area=Ff7/md8_1 why=unreviewed field script_exits=0 story_points=1 ... links=0 link_list=[])`). A field joins the list by review, not by a casual
edit: `cargo run -p fmv-ff7 --example world_survey -- <field>` shows what it would offer (the survey loads fields with
everything on), each point, exit and link is checked against the field's script, the result is pinned in
`crates/fmv-ff7/tests/chain_real.rs`, and only then is the name added.

**The lift goes down and back up, one ride per press.** The panel is two presses of one triangle: the ride down (the lift
bit on, and the story to 12 the first time) and the ride back, which the panel's own script makes when the lift is down
(the panel's script is run for the player's story and the tracked bit it turned off is turned off). They exclude each other,
a press is one ride and nothing flaps; the lift's exits follow the bit (`nmkin_1` up, `nmkin_2` down) and the moment stays.
The model only carries a story FORWARD (merging two stories is a join), so the saved story keeps the lift down while the
story the player holds in the world has it up until the player rides down again.

**After the Guard Scorpion: the bomb, the way out and the countdown.** What the game does after moment 27, read from
the owned field scripts (`--example script_dump` and `--example world_survey`; ids, offsets and moments only), and what a
party can do of it. Everything below is pinned against the real data in `crates/fmv-ff7/tests/chain_real.rs` and
`crates/fmv-client/src/route/real_tests.rs`.

* **The bomb.** There is no separate "set the bomb" step after 27, and it is not Barret's: the cutscene that comes before the
  fight does it (`nmkin_5`: Barret's talk script runs Cloud's scripts, the last of which walks Cloud to the reactor, plays
  the placing animation and the machine sounds, and starts battle 324), and the block of the director's loop that writes
  moment 27 when that is over also **starts a timer of ten minutes** (`STTIM 0:10:00`, just before the write) and flips bits of
  the reactor byte `Var[1][225]` that are not carried (bit 6 on, bits 3 and 4 off: the bomb and the doors). The other points of `nmkin_5` are for moments 120 to 127, a much later visit nobody on the first mission reaches.
  **There is no menu:** none of the ten fields has an `ASK`, so the countdown is not a choice. It is 10:00, always.
* **The countdown.** The `time` entity of every reactor field (`nmkin_1` to `nmkin_5`, `elevtr1`) opens the game's timer window
  only at moment 27, and the `timeo` entity of each sets bit 7 of the reactor byte and jumps to `nrthmk` when the timer reads
  zero at moment 27; `nrthmk`'s finale then ends in a game over for that case. The server runs the timer: the step that wins the
  fight carries its length (`StoryStep::countdown`, read from the `STTIM` of the step's own block: 600 s), the world keeps one
  countdown per account whose story it is (the leader's, in a party) and tells the account and the members of its party
  (`ServerMsg::Countdown`: `Running { remaining_ms, total_ms }` at the start, to a member that joins the party and to one that
  returns, `Over { Cleared }` when the story moves past 27 or the account or the party is gone, `Over { TimeUp }` when it
  runs out), and the window draws it at the top centre (`crates/fmv-client/src/ui/countdown.rs`: `M:SS`, the last minute in the
  warning colour, "TIME IS UP" for eight seconds; the headless client says it in `countdown:` lines).
  **A countdown that runs out does nothing:** the server logs one line (`countdown over account=<n> reason=time-up`), sends
  `Over { TimeUp }` and goes on. No story step, nothing saved, wiped or reset, nobody moved; the finale below still applies to
  a party that was late (the game's own late party gets a game over; a persistent multiplayer account never loses what it
  saved to a clock). A countdown is not stored: a server that restarts, and an account that logs in again at 27, have none.
* **The way out.** Every gateway stays open both ways, the ladders and jumps of `nmkin_3` and `nmkin_4` have their way up
  (the same lines, other links: the bots take 3 in `nmkin_3` and 1 in `nmkin_4`), and the lift is ridden back up by pressing OK on
  the panel again. In `nmkin_3` the woman who is stuck on the way out is freed by a talk (her talk script, from moment 27 on,
  turns bit 5 of the reactor byte on, which is carried for it: a talk that moves the story, once, `var1.225=1->33` with the
  lift down; no exit or point waits for the bit, which only decides where she and her namesake of `nmkin_1` stand and
  whether the second door of `nmkin_1` opens, neither of which a server models: the walkmesh has no locks and the cast does
  not change with the moment, so she stays on the field after she is freed). A bot that also travels (`--story-advance --travel`) pursues a story step that OPENS the exit its route
  goes to (the ride back, which is not a step forward) and never one that would CLOSE it (the ride down, forward again once
  the lift is up), so the lift is ridden once each way and never flaps (`StoryBot::set_wanted_exit`). The way out of the reactor is the
  gateway from `nmkin_1` to `nrthmk`. **A member that is behind its leader at the lift still goes up:** the finale moves the
  party's story to 33 as soon as the leader stands in `nrthmk`, and from 33 the lift of `elevtr1` leads to the second reactor
  (closed here, nobody has reviewed it), so a member that comes to the lift after that takes the exit its leader took
  (nmkin_1) instead of the one its party's story now says (see "A member follows its leader's way out" above; the real-data
  test `on_the_real_lift_a_member_that_comes_after_the_leaders_finale_still_rides_to_nmkin_1` in `follow/real_tests.rs`
  reproduces the order on the owned data, and `travel-smoke.sh --escape` reports the times it happened in a live run).
* **The finale (27 to 33).** In `nrthmk` the loop of a nameless entity runs, whenever the moment is 27 or more and bit 7 of the
  reactor byte is clear, the cutscene scripts of the field's director: the explosion's movie, moment 33, the lift bit on, and
  a jump to `md8_4`. The adapter reads it the way it reads any script a main loop requests (the unguarded writes of the
  requested script, under guards the model can judge) as an entry point of `nrthmk`, id 1, that fires when a player comes to
  stand there, so a party's leader goes 27 to 33 by walking out of the reactor. Bit 7 is carried only so that the step's own
  guard can be judged, and **this server never sets it** (the loop that plays the late party's game over wants it set, and
  is not a point at all). The jump to `md8_4` is NOT performed (a director's jump is not a walk-on exit): the party stays in
  `nrthmk`, whose gateway to `md8_4` is data and always open.
* **Where the evidence ends: `md8_4`.** That field has no player-controlled character. It is a cutscene (the AVALANCHE members
  and Cloud as characters at moment 33, a director that plays their talk and jumps on to `md8_1`), which the server cannot
  play, judge or leave by a walk: a party gets there by `nrthmk`'s gateway and walks about on its walkmesh, and this slice stops
  there.
* **What a re-login at 27 does.** A re-login always joins `md1stin`, and the walk from there to the reactor goes through
  `nrthmk`, where the finale moves the story on to 33 before the party is back inside: so the escape through the reactor is
  one continuous run, never a restart. `scripts/travel-smoke.sh --escape` walks down, wins the fight, climbs back out and
  takes the finale in one run (the story steps and the saved blob `0221000101e121` asserted exactly, one countdown started and
  cleared, never time-up, 0 battle divergences), then restarts the server, logs the leader in again at the saved 33 and
  walks `md1stin` to `md8_4` by the gateways with nothing firing again. Nobody is stranded at the new moments
  (`chain_real.rs`: from every place a player arrives in a reviewed field, walking and the links reach an open exit at
  27 and 33 with the lift up and down), except on the lift itself at 33 (see "Not done").
* **Not done:** the places the reactor's characters stand and the doors they open by the other bits of the reactor byte at 27
  (the characters on the way out in `nmkin_3` and `nmkin_1`: the server's walkmesh has no locks and its cast does not change
  with the moment), the movie, and `md8_4` and `md8_1`. **One dead end:** the lift of `elevtr1` at moment 33 and later leads
  nowhere a server offers (its way on is the second reactor's, not reviewed; its way back is the first mission's, closed by
  the story from 33). The story never takes a party there; only a party that walks back into the reactor after the finale
  stands on it (a member that is behind its leader is taken through by the leader's way out, above). Reconnecting puts a
  player in `md1stin` again; reviewing `smkin_1` and `smkin_2` opens the lift's way on.

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
--headless --story-advance` (against a server run with `--no-random-encounters`, or give the bot
`--battle-auto`: a bot caught in a battle stands frozen). `scripts/phase-smoke.sh` runs it against your own library (read-only,
throw-away state): A advances and B does not, so they stop seeing each other; B joins A's party and is
placed in A's instance; the server restarts, both log in again, and the phases persisted.

Area travel is what **protocol v4** adds to v3: a v3 client does not know `AreaChange`, so the
version was bumped and a server refuses a v3 client at the handshake with a clear
`ProtocolMismatch` reject (instead of losing it at the first exit it crosses). Both sides must be
built from this version or later.

Not done: exits a talk or a director decides (the finale's jump from `nrthmk` to `md8_4` among them), the script exits, story
points and links of every field but the first mission's ten (they are left off until reviewed), conditional NPCs (the cast
does not change with the moment), NPC animation, cutscenes (`md8_4`, where the first mission's evidence ends, is one).

## Accounts, avatars and parties

**Accounts.** A client connects with a token or registers a display name. Registering creates
the account and its avatar profile on the server and hands the client its token **once**; the
client saves it under its state directory (`--state-dir`, default `client/` next to the
library) in `accounts/<server>.token`, owner-only, and uses it from then on. After
registration the server never trusts a client's name, tint, body or fighting style: they come from the
stored player character, and `--name` is only needed to register. Names are unique (case-insensitive,
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
`EncounterStart` carries only the formation id, a seed, who fights at which level (the canon companions first, then
each player, and since protocol v9 who commands each one) and a fingerprint of the server's battle data, so each
client rebuilds the very same battle
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
character it fights as (its fighting style, the stat curve, since protocol v8, see "Your own character" below; before that
the character its look named, Cloud unless it chose another look) and that character's level, experience,
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
`scene.bin` attacks, chosen by their own AI scripts since protocol v11 (`fmv_ff7_battle::ai`: a bounded
interpreter of the scripts, with counters after a blow, such as Guard Scorpion's tail-up Tail Laser; anything it
cannot follow falls back, for that turn, to a uniform pick among the damaging attacks the enemy has the MP
for). A counter comes in the same message as the blow that provoked it, and the client plays the two **one after
the other** (`scene::cue`: each performance of a batch begins when the one before it is over, so the Tail Laser is
seen after the blow, its name comes up in the banner when it begins, its numbers land after the blow's, and the
victory or defeat panel counts from the last performance that begins). `scripts/battle-smoke.sh --boss --build` fights the
real Guard Scorpion (formation 324, level 8) with the canon pair and two bots and FAILS unless the server logged at
least one Tail Laser counter (`counter battle=.. actor=.. attack=286`, and `counters=N` on the `battle ended` line),
both bots counted the same ones and both replayed the whole battle, counters included, with 0 divergences. Spells: the magic materia a character's starting record equips, plus a
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
the replay. Known gaps: no statuses, no summons, party elemental resistances are not modelled (limit
breaks and escape are in protocol v7, whole sides in v10, below).

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
Healing Wind heals the whole party, each member for half of its own max HP (protocol v10: its record is
"All Al (NS)"). Not modelled: limit levels above 1 and the kills and uses that earn them, Cait Sith's
Dice, Tifa's slot reels, Vincent's transformation, the whole-side limits of the higher levels. **Escape** is the last row of the menu (FF7 runs by holding L1+R1,
which this window cannot do cleanly): FF7's escape does not depend on levels but on the formation's *Run
Difficulty* (`scene.bin`'s escape counter, 1 for most fights) and a Run Check that succeeds one time in
four (always, after a preemptive strike or on a side attack); the actor spends its turn on four checks,
the difficulty that is left is kept, and a formation whose flag bit `0x0004` is clear (every counter of 9
or more in the owner's file: bosses and story fights) cannot be escaped at all. A successful escape ends
the battle for the whole party with `Escaped`: nothing is paid. The **gauge is saved at the end of every
battle**, whatever the outcome (escapes included), in the account's character record (a record of its own
is made for an account that has never won, so "nothing is written until its first win" now reads "until
its first win or the first battle that ends with a gauge worth keeping"); records written before v7 load
with an empty gauge. The headless bot gives its limit when the gauge is full (a healing one, which heals the whole
party, only when somebody is wounded) and never runs, unless a `--battle-script` tells it to (see below).

**Whole sides (protocol v10).** `Command::Magic`, `Item` and `Limit` carry a `Target`: one actor, or
`AllEnemies` / `AllAllies` (named from the commanding actor's point of view, so an enemy's AI will ask the
same way). On the party side `AllAllies` is the whole roster of protocol v9, the canon Cloud and Barret and the
players alike, so one Cure can heal five fighters, and a player can aim a Cure or a Potion at one companion with
`One`: the pointer walks the roster in actor order, the companions first, and then ALL. The canon companions give
a limit the same way, from its own record (Healing Wind on the whole party, only while somebody is hurt; a
damaging limit at the first living enemy). What can be aimed how is the record's own targeting bits, read from the owner's `kernel.bin`:
Cure, Life, Fire, Ice and Bolt can hit one target or a whole side ("All Tar/1 Tar"), Ultima, Megalixir and
Healing Wind only a whole side ("All Tar (NS)"), and no item has a toggle. FF7's rule for the first kind of
spell is that it needs an **All materia** paired with its magic materia, which allows it as many times per
battle as the materia's level (Terence Fergusson's *FF7 Party Mechanics Guide*): the engine keeps that count
per character (`Combatant::all_left`, read from the kernel's All materia, id 23, at level 1: once per battle,
the one All being counted for the whole starter grimoire), a whole-side cast spends one, and the Magic list
says `ALL x1`. An action that hits several targets **splits** its damage: 2/3 for each target (Fergusson's
*Battle Mechanics* FAQ), for the physical formula and Cure's, and for a spell only when it could have been
one target; a fraction of HP, a flat amount or "everything" (Life, potions, Megalixir) never splits, and
neither does a lone target. The pseudocode page on the FFRTT wiki writes the factor as 3/2; the FAQ, the Final
Fantasy Wiki and the community say 2/3, which is used (labelled in `crates/fmv-ff7-battle/src/action.rs`).
A whole side is one `Action` followed by one `Damage`, `Heal`, `Miss` or `Revive` per target in actor order
(no new events), every roll drawn per target, so the replay checks it like anything else. The enemies still
pick one random party member each turn (their own AI, which can ask for a whole side through the same
`Target`, is another change). `--battle-script` takes `magic:<id>:all` and `item:<id>:all`, and
`scripts/battle-smoke.sh --commands` casts Cure on the whole party over real processes.

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

**The opposite aid: made-up numbers that ARE stored (`--debug-bonus-exp`, `--debug-bonus-limit`).** A win pays
about 16 of the ~250 experience a level costs and these weak enemies rarely hurt anybody, so over real processes a
new character never levelled up or filled a gauge, and "a level and a limit gauge survive a real server restart"
went unproven (the slice smoke used to say so in a note). `fmv-server --debug-encounters --debug-bonus-exp N
--debug-bonus-limit M` closes it: every won battle pays each member that fought on `N` experience MORE than the
battle gave it, and every member's end-of-battle limit gauge is kept `M` higher than the battle left it (up to the
cap), both through the ordinary save path (the mirror, the saver, the game's own level-up rule), so what is stored
afterwards is indistinguishable from earned numbers. That is why they are guarded: each is refused unless
`--debug-encounters` is given too, **both are refused on the default state directory** (name a throw-away one with
`--state-dir`, or use `--ephemeral`), `--debug-bonus-limit` is refused together with `--debug-limit-full` (whose
early return would make it do nothing), the server warns about them loudly when it starts, and every use is logged
(`battle reward ... exp=<paid> ... debug_bonus_exp=N`, `limit kept ... debug_bonus_limit=M`). The `BattleEnd` that
clients verify by replay still carries what the battle itself earned (only the payment is bigger), so replays never
diverge. Nothing is written that says a number was made up: only ever run it on a throw-away state directory.
`scripts/slice-smoke.sh` runs its FIRST server with both (900 and 100) and proves that both new characters ended
session 1 at a higher level than they joined at (told `leveled_up=true`, stored) with a non-empty gauge, and that the
servers it then restarts WITHOUT the flags tell and fight them at exactly those levels and gauges.
`scripts/battle-smoke.sh --commands --build` uses all of it on the real field `md1stin` and owned data: one
server (with both debug flags) stays up through two runs of the same two bots; in the first they follow
`magic:0:all,defend,item:0,magic:0` (Cure on the whole party first, the canon pair included) and
`magic:0,magic:9999,item:0,defend` (spell 9999 does not exist: that entry must be skipped and counted) and win, in the second both try `escape` until the battle ends in Escaped (formation
12 has run difficulty 1; one attempt gets away 68% of the time). It FAILS unless the server applied each of
magic, item, defend and escape at least once (the bots' breakdowns and the server's own log lines must
agree), a canon companion gave its limit break through the server's rule (players have none, so no bot's `limit` is
ever applied), every battle was replayed by each bot from its own battle data, the companions' commands included, with
0 divergences, both battles started with the players' `limits=[255, 255]` and the debug gauge was never stored, and (with
the default scripts) the server logged exactly one `magic used ... spell=0 target=all-allies` line, bot A's Cure on the
whole party.
`FMV_SMOKE_SCRIPT_A`, `FMV_SMOKE_SCRIPT_B` and
`FMV_SMOKE_ESCAPE_SCRIPT` override the scripts (to try its failure paths).

**Status effects (protocol v12).** `fmv_ff7_battle::status` is the model and its module docs hold the
sources and every number: the 32 status bits of the game, what an attack, item or `scene.bin` record's status fields
mean (a mask and a byte, the low six bits of which are the chance out of 63 and the high bits say inflict, cure or
toggle; the data's own "no status" marker), and the clocks. The sources are Terence Fergusson's *FF7 Battle
Mechanics* FAQ (section 2, which could only be read as the Fandom wiki and the search results quote it), the
ff7-flat-wiki's status and attack-data pages and ff7.info; where they give no figure the module says so (APPROXIMATIONS).
A **Global Timer** goes up by the Speed Value every tick and a **unit of time** is 8192 of it (31, 91 or 152 ticks at the
fastest, default and slowest Battle Speed, which the FAQ's own figures check); every fighter also has a **V-Timer**, twice
as fast, which Haste doubles, Slow halves and Stop freezes. **Poison** takes max HP / 32 every 2.5 units (ignoring Defense
and Barrier, and it can kill); **Regen** gives the same every unit, 32 times; **Stop** lasts 15 units and **Paralysis** 8
(on the Global Timer: they freeze the V-Timer); **Barrier** and **MBarrier** last 30 units and halve physical and magical
damage; **Sleep** (30 units: the sources only say it outlasts Stop) holds the turn until a physical blow wakes it;
**Silence** closes the Magic command; **Haste** and **Slow** double and halve the ATB gauge and every clock (and cancel
each other, like **Sadness** and **Fury**: Sadness takes 30% off damage and half off what a hit adds to the limit gauge,
Fury takes 30% off the hit rate and doubles the limit gauge); **Darkness** halves a party member's Attack command's hit
rate (and an enemy's attack not at all, as in the game). A sleeper, a stopped or a paralysed fighter **holds** its turn:
its ATB gauge stays full (a sleeper's keeps filling, the other two freeze) and it is not offered a command
(`BattleState::ready_actors()` leaves it out, `CommandError::CannotAct`), an enemy waits with its full gauge and acts
the tick it can, and a blow that leaves it asleep, stopped or paralysed gets no counter. Death is HP 0 (and a knockout
removes every status) and Near-death is the display of 25% HP or less.

*From the data:* of the Steam copy's 2272 scene attacks 317 carry a status the rules play (145 of them nothing else), 17 items only change statuses (Antidote, Echo Screen, Eye drop,
Remedy, Speed Drink, Hyper, Tranquilizer, the curtains, the Hourglass, Dream Powder, Mute Mask, Dazers, ...), and the
kernel's Regen, Poisona, Esuna, Sleepel, Silence, Barrier, MBarrier, Wall, Haste, Slow, Stop, DeBarrier, DeSpell, Bio
(1 to 3), Freeze and the rest decode as actions: a **status-only** action (`Formula::None`) rolls to hit like a spell
unless it aims at allies or is an item, a damaging one keeps its damage and then tries its status on every target that
still stands. Which side a status-only action aims at is the record's own targeting bit; an enemy record's immunity mask
says which statuses AFFECT it (a set bit), so all ones is "immune to nothing" (209 of 625 enemies) and a clear bit is an
immunity (Haste and Slow share one). The starting party's grimoire has none of them, and the first mission's enemies
inflict none, which is why a smoke run needs the debug aid below. Draws: an action without a status effect draws exactly
what it did; one with a status draws one `below(63)` per target that still stands after its effects (`rules`, "RNG draw
order"); the clocks of Poison, Regen and the timers draw nothing themselves (a scripted enemy that Poison kills answers with its
Death counter, which draws as any counter does).

*On the wire:* three events (`StatusInflicted`, `StatusEnded`, `StatusTick`), two refusals (`CannotAct`, `Silenced`) and
one input, `BattleMember::status`: the statuses a member STARTS with (absent from the JSON when empty; a set the engine
does not play, or Haste with Slow, or Sadness with Fury, is refused when the battle is built). *In the client:* the party panel
and the enemy list tag each fighter with the statuses it has (`Psn`, `Slp`, `Sil`, ... in their colours, as many as the
window has room for, then a `+N` for the rest: at the default 1100 x 680 the party panel has two places, a code and the
`+N` or both codes of a member with two, a long name giving up a cell or two for it), the ATB bar is cyan under Haste, violet under Slow and grey while held, the Magic row of
the menu is greyed out while the character is silenced (with the reason under the cursor), the line under the status band
says "You are asleep", the popups float a status's name and the turns of Poison, and in the 3D scene a poisoned body is
tinted and pulses, a sleeper droops and is dim, and a stopped or paralysed one is grey and holds its idle frame.
The enemies' AI scripts read the status word `0x4000` of every actor from the engine, so a scripted enemy can branch on a status.
`--debug-status LIST` on `fmv-server` (refused without `--debug-encounters`, warned about at start-up) makes every member
of every battle start with those statuses; `scripts/battle-smoke.sh --status --build` runs the real server and two bots on the
real field with it and FAILS unless the server announced every status, both bots' rosters and status notes agree and their
counts of the status events are EXACTLY the server's, and both bots replayed the battle from their own data with 0
divergences. *Not played:* Confusion, Berserk, Frog, Small, Petrify, Slow-numb, Reflect, Manipulate, Death Sentence and
the rest (they decode and are ignored), armor protections, weapon status attacks, and Sadness and Fury across battles.
An enemy's script can queue an attack that has ONLY a status (44 of the 341 scripted enemies of the Steam copy do, checked by `ai_real_data`: a curse on
the party, or a buff such as Barrier on itself or its own side): it is played wherever the script aims it, costs the enemy no MP
(charging for it made some casters run dry and fall back to the plain behaviour for good: measured over every enemy of
`scene.bin`), and the statuses it inflicts are in the status word the scripts read, so a script that casts a buff until it has
it now stops. An enemy that Poison kills on the clock answers its death like one a blow kills: its Death counter runs in the
same step (nobody struck it, so a counter that aims at the attacker finds nobody and is skipped).

`md1stin`'s own encounter table (section 6 of the field file, Steam PC build): the first of its
two 24-byte tables is the enabled one, with rate 128 (a roll succeeds with probability 128/1024)
and three formations whose weights add up to 64: 301 (weight 24, two enemies), 302 (22, one enemy)
and 303 (18, two enemies); the second table is disabled. The server reads every field's table the
same way (checked over all 702 fields of that build: every enabled table's weights add up to 64
and every formation exists in `scene.bin`). A starting party of two players wins 301 and 303 every time (200 of
200 seeded runs each, both members attacking). Since protocol v9 the canon Cloud and Barret fight beside the players and
end those fights in about a hundred ticks, before every player has had a turn (measured by
`crates/fmv-server/tests/battle_real.rs`: the players act in only about a tenth to a half of the fights), which is why
the smoke scripts that need every bot to act fight formation 12 instead (two sturdier enemies of the game's own data,
won every time with every player acting in 200 of 200 seeds; `--formation` picks another) and only `--random` fights
`md1stin`'s own.

**Your own character (protocol v8; in the party since v9).** Every player has ONE original character that travels
across games. In FF7 it fights with the **stat curve** of a canon party member (its *fighting style*: Gunner, Mage and so
on; the stored field is still called `slot`) and is **added** to the story's own party in battle, never taking a canon
character's place (see "The canon party fights beside the players" below); dialogue is never name-swapped, and the
character's identity and appearance persist and each game draws them its own way. Three things are therefore separate,
in the protocol and in the store:

* the **identity**: name, colour and a game-agnostic body choice (the creator's Body row);
* the **binding**: how the hosted game draws the character, stored per game. In FF7 it is a generic
  placeholder body, a non-canon human model of your own `char.lgp` (`lean`, `sturdy` or `slight`: see
  the `playable` module of `fmv-ff7` for which models and why), tinted with the identity colour, and
  never a canon character's model (an account that predates the creator keeps the canon look it chose until it is
  converted with `fmv-characters restyle`, below). Every player sees every other player's binding, in the field and in
  battle;
* the **fighting style** (the record's `slot`): the stat curve of a canon party member, whose combat template,
  abilities and experience curve the character fights with. It is chosen once, at creation, and fixed (FF7's
  experience curves differ per character, so a change would change a player's level). It is internal: the creator's
  row is called "Fighting style" and its labels (the table below) name no canon character. New characters pick any
  style but the Swordsman's (Cloud's own curve: he stays the protagonist; an account that predates the creator may
  hold it); two players may hold the same style; a registration that names none gets the Gunner's (Barret's
  curve). In battle the party panel, the HP list, the banners and the logs show the player's own name; the engine
  uses the curve's template, and the model is the player's own body, drawn in the player's identity colour like the
  field body ("The player's colour on a model").

| curve | fighting style | stat curve of |
|---|---|---|
| 0 | Swordsman (not offered to a new character) | Cloud |
| 1 | Gunner (the default) | Barret |
| 2 | Martial artist | Tifa |
| 3 | Mage | Aerith |
| 4 | Guardian | Red XIII |
| 5 | Ninja | Yuffie |
| 6 | Gambler | Cait Sith |
| 7 | Marksman | Vincent |
| 8 | Lancer | Cid |

The server keeps it in `<state>/server/characters/<account>.json` (`fmv-persist`'s `PlayerCharacter`):
the identity and, per game, the binding, the slot (the fighting style's stat curve) and **pointers** to where the progress lives.
Pointers, never copies: the level, experience, AP, gil, limit gauge and inventory stay in the
character-progress record under `progression/<account>/<Game>.character.json` and the story in the
story blob beside it, exactly where they always were (that record's `character` field is the stat curve's id;
if a rolled-back build saved another one the slot wins and the next save writes it back). An account that
predates the record (every account of a v6 or v7 server) gets one at its first login, **added next to
its files, which are not touched**: a test loads the exact v6 and v7 file shapes and requires every file
byte for byte unchanged and every number the same. Its curve is the character v7 fought it as (the one
its look named, Cloud without a look; a migrated account may hold Cloud's) and its look is kept as
its binding (an account that never chose one gets the default placeholder body). The window's character
creator takes a name, a body, a colour and a fighting style; headless players take `--body` and `--slot` (a style:
`--slot gunner`, or the canon character whose curve it is, `--slot barret`; `fmv-client --help`).
`scripts/slice-smoke.sh` proves the whole slice on your real data over real
processes: two players with different identities and fighting styles register, party up, advance one player's
story (the server-validated story point of `md1stin`), talk to the field's guard (`fmv-client --headless --talk gu0`
prints what a character says as `talk:` lines, read from your own copy of the field script with the player's own
story: the guard says the game's own words, the same to both players) and win a shared battle whose roster is the
canon Cloud and Barret (server-controlled companions: `companion acted` lines, every command replayed by both bots with
0 divergences) followed by both players under their own names with their styles' stat curves, the rewards going to
the two players only; the server process is stopped and started again, the same players log in again and are told
exactly what they ended with (a login changes no record byte), and a second battle on the restarted server pays on
top of what was saved, to the point. The stored limit gauge of an account (the smoke writes 200 and 120, as an account
that predates v9 would hold) is preserved by the battle and by the restart.

**The canon party fights beside the players (protocol v9).** A battle's party is the story's canon party first, then
every PLAYER in party order: in the first reactor mission (`md1stin`, `md1_1`) Cloud and Barret, so a solo player
fights as [Cloud, Barret, the player] and two players as [Cloud, Barret, player 1, player 2], up to five (a player
party is still at most three). The canon party is a table (`CANON_PARTIES` in `fmv_ff7::story`: one row from the first
moment of the game, extended with each story party that comes later), chosen by the party leader's own story progress.
Nobody is replaced, whatever stat curve a player fights with: a new Barret-curve character gets [Cloud, Barret,
itself] and is drawn in its own body, never as a second Barret, and an account that predates the creator on Cloud's
curve gets [Cloud, Barret, itself] with no second Cloud. (The canon Barret is therefore no longer a stand-in NPC of the
field: he is in the party.)

* **The server commands the companions.** A canon member has no account and no connection: when its gauge is full the
  server issues its command as an ORDINARY command (heal the most hurt member below half HP with the cheapest healing
  spell it can pay for; else its limit break when its gauge is full and there is something worth aiming at; else Attack
  the first living enemy), announces it like a player's (`AppliedCommand`) and logs `companion acted battle=.. actor=..
  name=Cloud command=..`, so every client replays and verifies it with no change to the engine. `fmv_server::companion`
  is the whole rule, a pure function of the engine's state (deterministic, never refused by it). A companion fights at
  the party leader's level (`--battle-level` pins everybody's), is never paid and never saved, cannot be commanded by a
  client (`NotYourActor`), and does not keep a battle alive: a battle with no connected player left is dropped.
* **The players go first.** A companion whose gauge is full waits while a living, connected player still owes a turn
  this round (`fmv_server::companion::may_act`), so the players lead every round and Cloud and Barret follow. A player
  who is choosing holds them for at most `--companion-turn-wait-ms` (default 5000) after its own gauge filled, and no
  companion holds a full gauge longer than `--companion-max-wait-ms` (default 10000; 0 makes the companions act the
  instant their gauge fills, as the first v9 build did). Without it the canon pair finished most of md1stin's fights
  before a player's turn came (a solo player acted in 41-88% of them, two players in 11-41%); with it a solo player acts
  in every one and a companion never takes a turn in a fight in which a player got none (the real-data gate
  `with_pacing_every_player_acts_in_every_fight_of_the_first_field`). The `companion acted` line ends with `held_ms=`,
  how long it waited. Pacing only decides WHEN the server submits a companion's command, so clients need no change.
* **Players are paid, companions are not.** Experience and gil are shared among the living PLAYERS only
  (`fmv_protocol::reward_roster`, used by the server and by every replaying client), AP goes to each player, and only
  a player's progress, inventory and limit gauge are saved.
* **Players have no Limit Break** for now (`fmv_protocol::PLAYER_LIMIT_BREAKS`): every peer's engine gives a player's
  combatant none (so a Barret-curve player does not give Barret's limit break beside the canon Barret), the window
  shows no limit gauge for it and a `limit` command is refused (`NoLimit`). The gauge a stored account already has (the
  preview bot's is about 200) is **never overwritten or zeroed**: the server saves a gauge only for a member that has a
  limit break. Whether players get limit breaks of their own is the owner's call.
* **Drawn as themselves.** The canon companions are the canon characters' own battle models (weapons and clips); each
  player is its own body, the placeholder body and colour its entity has on the field (the snapshot's appearance ref),
  moving procedurally: the lunge, the hop back, the flinch and the fall. Colours follow the players' entities, never the
  order of the roster. An account that predates the creator and chose a canon look (the preview's Blizz may be one:
  `fmv-characters show` says) is drawn in battle as that look's FIELD model, as in the field, tinted, beside the canon
  battle model of the same character: it reads as a second Cloud until the account is restyled with a placeholder
  body (`fmv-characters restyle`, below: identity, tint and binding only, never the curve, the progress or the story).
  The pictures of `offscreen_battle_canon_party_with_every_player_in_its_own_body` (`cargo test -p fmv-client --
  --ignored --test-threads=1`, with `FMV_FF7_DATA` and `FMV_SHOT_DIR`) show the canon pair and two players, three
  players (five fighters, at three window sizes), and one alone in each of three cases: a new character on Barret's
  curve, an account that predates the creator and never chose a look (Cloud's curve, the default body), and one that
  chose Cloud's own look.

**Converting an existing account to an original look (`fmv-characters`).** An account made before protocol v8
(the preview's Blizz is one) is a canon look and the character that look named; to turn it into a character with
a placeholder body of its own, with the server STOPPED:

```
target/release/fmv-characters <state-dir> show <account-id|name>
target/release/fmv-characters <state-dir> restyle <account-id|name> --body <lean|sturdy|slight> [--tint rrggbb] [--dry-run]
```

`<state-dir>` is the SERVER's state directory (the one with `index.log`, `accounts/`, `avatars/`, `characters/` and
`progression/`; there is no default, and a path that is not one is refused before anything is created). The tool
opens the store like a server does, so while a server holds the directory it is refused (`locked by pid ...`, exit
1) and nothing is touched; copy the directory first anyway. `restyle` changes exactly `identity.body`, the
`identity.tint` (only with `--tint`) and the game's binding (`ff7/body/<body>`) in `characters/<id>.json`, and never
the slot (so the level curve stays what it was), the pointers, the name, the origin or any other file: the avatar
profile (what a rolled-back v7 reads, so a rollback shows the account exactly as before), the account record,
the index and every `progression/<id>/*.json` (level, experience, gil, AP, items, limit gauge, story) stay byte for
byte what they were. An account that has no record yet (every v6/v7 account until its first v8 login) first gets
the record its first login would have made, built by the server's own migration rule (`fmv_server::admin`), and
then the look change, in ONE atomic write. It prints a before/after summary (`slot (unchanged): 0`, `body: (none)
-> sturdy`, `binding: ff7/cloud -> ff7/body/sturdy`, the tint) and a verification block that re-reads every file it
did not write and prints its sha256 before and after (`-> same`); `--dry-run` shows the same plan and writes
nothing; asking for a body the game does not offer (`cloud`, a typo) is refused with the list. Exit codes: 0 done
(a dry run, or a restyle that had nothing to change), 1 refused, 2 usage. Tests (`fmv-persist`'s
`restyle_character` and `fmv-server`'s `character_admin`, over temp directories only) snapshot the whole tree of
exact v7- and v8-shaped state directories before and after and require every file but `characters/<id>.json`
(and the lock) byte for byte identical, a dry run that writes nothing, and the refusal while locked. The tool ships
in the release package next to `fmv-server`.

**Deleting a character, and getting it back.** A player deletes a character from the character menu (**Del**, then
**Y**; see "Character menu"): the client asks the server with the character's own login token, so only the one who holds
the token can, and the server never deletes a character that is in the world. The delete is **soft**:

```
<state-dir>/trash/<UTC time>-<account id>/      e.g. trash/20261109T165905Z-7/
    manifest.json                               who, when (unix and UTC text), and the files the entry holds
    accounts/7.json  avatars/3.json  characters/7.json
    progression/7/Ff7.json  progression/7/Ff7.character.json     the records exactly as they were
```

The account's records are moved there, its name is free at once (anyone can register it, and gets a fresh character with
nothing of the old one), its token is void and its account id is retired for good (ids only grow). Nothing of any other
account is read, written or moved. The commit is one synced `release` entry of `index.log` (the entry an orphan's release
writes), between a plan written first and the file moves done after it, so the index and the files agree at every instant
a crash can stop at: opening the store finishes a delete whose entry was written and abandons one whose entry was not
(`fmv-persist`'s `store/trash.rs` has the argument and `store/trash/tests.rs` builds every crash state). With the server
STOPPED, an operator can do the same and undo it (the same state directory argument as `restyle`):

```
target/release/fmv-characters <state-dir> delete  <account-id|name> [--dry-run]
target/release/fmv-characters <state-dir> trash
target/release/fmv-characters <state-dir> restore <trash-entry|account-id|name> [--dry-run]
```

`delete` frees a name whose player has no token, or removes a character by hand; it hashes every file of the state
directory before and after and fails the run if anything but the account's own records, `index.log` and the trash
differs (`moved: accounts/7.json (sha256 ...) -> trash/.../accounts/7.json, the same bytes`, then `N other file(s) and M
director(ies): byte for byte unchanged`). `trash` lists the entries (name, character, when, files, state). `restore` brings
an entry back as a **new** account: the name must still be free (else it is refused and nothing changes), the character
gets a new account id and avatar id (the old ones are retired and the index refuses to replay a smaller id), every record
that carries an id is re-keyed, the account is marked claimed (it is nobody's orphan) and gets a **new token, printed once**
(the client forgot the old one when it deleted); the player puts it in `accounts/<server>.<name>.token` of the client's state
directory, two lines: the token, then `name=<display name>`. The entry is renamed `.restored-<new id>` and cannot be restored
twice. `--dry-run` runs the same checks and writes nothing. Exit codes as `restyle`. Snapshots (`Store::snapshot`) are
rollback images of the live records and do not copy the trash. `scripts/delete-smoke.sh` runs all of it over real
processes on the FIXTURE server (no game data, temp state only): a character in the game is refused; one that left is
deleted on the server with its own token (its records in the trash byte for byte, every other file unchanged but for
`index.log`); its name registers again as a different account; its old token is refused, also after a server restart;
and `fmv-characters restore` gives another deleted character back with a new token that logs in.

**Wire (added after v14; the integrator bumps the version).** `ClientMsg::DeleteCharacter { protocol_version, client_build,
token }` is the first message of its own connection instead of `Hello`; the server answers `ServerMsg::CharacterDeleted
{ name }` and closes, or `Reject`: `InvalidToken` (the server does not know the login: nothing to delete, the client forgets it
locally, which also settles a delete whose answer was lost), `AlreadyOnline` (the character is in the world, from any computer:
nothing deleted), `ProtocolMismatch`, `RateLimited` (a delete of the same character is under way) or `Internal`. A login that
authenticated before a delete cannot join after it: the accounts a run deleted are tombstones checked under the world lock by
every join. The server logs `character deleted: its records are in the trash and its name is free` with the account, the
trash entry and the file count.

**Protocol v8** (`fmv-protocol`) carries all of the above; its module docs describe the flow,
and golden JSON tests pin the wire format (the engine's own event, command and outcome types
included). v8 replaced `Hello`'s colour and look by the character to create (`Hello::character`:
colour, body, slot), added the character card to `Welcome` and the player's own name to every battle
roster member (`BattleMember::name`, required); neither side can read the other's `Hello`, so a v7
client (or server) is refused at the handshake, cleanly, because the version of any `Hello` is readable.
(v8 is also the first version every client of which knows the story lane's `StoryUpdate`, which v7 gained
without a version of its own: only an account that advanced a story is sent one.)
**Rolling back v8:** a v7 server never reads or writes `characters/`, so every record of v7 shape comes
back exactly as it was (the migrated accounts included); a character created by v8 comes back as the
canon character of its slot (its avatar profile carries that look as a compatibility mirror, which v8
never reads) and fights as that slot, except slot Cait Sith, who has no canon look and fights as Cloud.
Its `characters/` record waits for v8. A client or server of another protocol version is refused with a
clear message:
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

**Protocol v9** (`fmv-protocol`) puts the story's canon party in the battle roster: every `BattleMember` now carries a
`controller`, `Player { entity, account }` (the field entity is how every client finds that player's look and colour:
a superset of just an account) or `Companion` (the server commands it; no account), in place of v8's bare `entity` and
`account`; `party[i]` still fights as `ActorId(i)`, the companions first. A companion's commands travel as the same
`AppliedCommand`s as a player's, so no client needs to know who issued one. Neither version can read the other's
roster, so the version had to change: a v8 peer is refused at the handshake, cleanly, as v7 was (golden JSON tests pin
the player roster, the companion roster and the controller, and one reads a v8 member as a v9 one and fails on purpose).
**Rolling back v9:** v9 writes nothing a v8 build cannot read: the stat curve is the same `slot` field, no companion is
ever saved, a player's stored limit gauge is kept exactly as it is and no record is re-keyed or moved, so roll the
server back to a v8 build on the same state directory (and the clients with it: a v8 client is refused by a v9 server
and the other way round). Characters created by v9 are the same as v8's.

**Protocol v10** (`fmv-protocol`) makes the target of a spell, an item and a limit a `Target`: one actor (`{"One":3}`) or
a whole side (`"AllEnemies"`, `"AllAllies"`), named from the commanding actor's side, in place of a bare actor number
(`Command::Attack` keeps its one `target`). It changes no event, message or stored record (a whole side is one `Action` and
an effect per target in actor order), so only the commands differ, and they differ for everybody who gives one: a player,
and the server for a canon companion (its commands travel in the same `BattleEvent`s). Neither version reads the other's
command, and a v9 `Hello` is still a v10 `Hello`, so the version number is what refuses the older peer: a v9 client or
server is refused at the handshake, cleanly, as v8 was (golden JSON tests pin every shape, a test decodes a v9 command
with a v10 reader and the other way round and fails on purpose, and a real socket test checks the refusal of v9 and every
older version). **Rolling back v10:** v10 writes nothing a v9 build cannot read: the All materia uses of a battle are
engine state derived from the player's own data and nothing stores them, so roll the server back to a v9 build on the
same state directory (and the clients with it: a v9 client is refused by a v10 server and the other way round).

**Protocol v11** changes no message, event or stored record: it is the version of the ENGINE that every peer replays a
battle with. Since v11 the enemies act by their own `scene.bin` AI scripts (see "Battle commands and items" above), and a
v10 engine would pick different enemy moves from the same seed, so its replay would diverge at the first enemy turn: a
v10 client or server is refused at the handshake instead (the real socket test refuses v10 and every older version).
**Rolling back v11:** nothing it stores is new, so point the server and the clients back at a v10 build together.

**Protocol v12 (statuses)** adds three battle events, two refusals and one optional field of the roster (see "Status
effects" above), so a v11 peer cannot decode them and the version has to move with them: the server and the clients change
together. It stores nothing new (a status is battle state; the starting statuses are a debug input), so rolling back is
pointing the server and the clients back at a v11 build together.

**Protocol v13** is again a change of the ENGINE only: a scripted enemy's status-only attacks are played, and an enemy
that a turn of Poison kills runs its Death counter in the same step (see "Status effects" above). No message, event or
stored record changed shape, but a v12 engine would skip those attacks and counters, so its replay would diverge: a v12
client or server is refused at the handshake. **Rolling back v13:** nothing it stores is new, so point the server and
the clients back at a v12 build together.

**Protocol v14** adds one client message, `Interact` (the OK button; see "The OK button, ladders and jumps" above).
The lift, the two talks that move the story and the ladders that wait for OK work only through it, so a v13 client could
not ride the lift down: it is refused at the handshake instead. Nothing new is stored (the steps it makes are moments of
the story record that v2 already stores). **Rolling back v14:** point the server and the clients back at a v13 build
together. The story record keeps its v2 shape, so a v13 server can read what v14 wrote, but it has none of v14's talks,
links or scripted battle, and how its exits judge a moment past 13 was never tested: once a player has gone past moment
13, prefer rolling forward to rolling back.

**Protocol v15 (the countdown message)** adds one server message,
`ServerMsg::Countdown { state }`, with `CountdownState::Running { remaining_ms, total_ms }` and
`CountdownState::Over { reason: CountdownEnd::TimeUp | Cleared }` (see "After the Guard Scorpion"): sent when the step that
won the Guard Scorpion starts the bomb's countdown, to a player that joins or leaves the party that has one, and when it
is over. A peer that does not know it cannot decode it (an unknown message is a decode error), so a v14 client would be
cut off at the first countdown a v15 server starts: the version has to move with it and a v14 peer be refused at the
handshake. `StoryStep` gained a `countdown` and `AreaRuntime` a `countdown_live` (library API, not wire). Nothing new is
stored (a countdown is a clock of the server process), and the story record keeps its v2 shape: the reactor byte's bit 7
is carried so that the finale's guard can be judged but this server never writes it, so a record is readable by a v14
server, which just has no finale, no countdown and no `Countdown` message. **Rolling back:** point the server and the
clients back at a v14 build together.

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

**Music and sound.** `import-pc` also takes the install's music and sound files into the library when it
has them: `data/music_ogg/*.ogg`, `data/music/*.wav` and `music.idx`, `data/sound/audio.dat` and `audio.fmt`
(the Steam edition has all of them, about 0.9 GB). Nothing is converted and the originals are untouched;
each file is stored under its hash like the rest and listed in the set's manifest. **To add the sound to a
library you made before the importer took it, run `import-pc` again on the same install with the same
labels** (`--edition`, `--region`, `--language`, `--revision`, or none if you gave none the first time): it
adds the sound files to the set that exists and changes nothing else in it. Other labels would make a
second FF7 set, and the server and the client then refuse to choose between two ("the library holds several
FF7 PC sets"), so check with `fmv-import list` that there is still one. If you start the game through the
launcher script (`ff-multiverse.sh`), run it once with `FMV_FF7_REIMPORT=1`: it imports the install again
the way it did the first time, which adds the sound to the library it made. A library without sound files is
fine: the game runs without sound and says so in one line (see "Sound").

## Sound

The window plays your own FF7's music, menu sounds, the sounds of a battle's blows and the sounds the fields' own scripts play, read from
the same library as the game's data (the
importer takes the music and sound files along with the rest, see above). The client never receives audio
from a server and keeps none anywhere but the library.

- **Field music.** A field's script names its track (its `MUSIC` command); the client reads it from your
  copy of the field, finds the track in the game's own `music.idx` and crossfades into it when the server
  moves you to the area. A field that names none leaves the music playing, as in the game. A track starts as
  soon as it has decoded (about a second); the loop points in the file are honoured sample for sample.
- **Battles.** The field's battle music (`BMUSC`), else the standard battle theme, plays when a fight starts.
  A win plays the victory fanfare once and then the field's music again; running away or losing goes straight
  back to the field's music. The next fight's music and the fanfare are decoded in the background as soon as
  you enter a field, so that a fight does not wait for them.
- **Menus.** The battle menu and the character menu click, confirm, cancel and buzz with the game's own sounds.
- **A battle's sounds.** Each blow plays the sound your own game data names for it, **at the moment it lands** (the
  instant its number floats up over the target, not when the server's message arrived): a party member's attack plays
  its weapon's hit sound, critical-hit sound or miss sound (read from the weapon record of your `kernel.bin`), and an
  enemy's attack plays the impact sound of its attack record in your `scene.bin`; a counter is heard after the blow that
  provoked it, and the killing blow of a fight is heard even though the fight is already over. The sound ids are the
  game's own (1-based, entry `id - 1` of `audio.fmt`: `crates/fmv-ff7/src/sound.rs`, "Battle sound ids", has the evidence,
  27 of 27 checked ids name the clips the community calls by those roles). **What the data does not name is silent,
  not guessed:** a spell's effect makes its own sound in code that is not in your files (every spell's attack record says
  "no sound"), an item has no sound field, the limit breaks have no attack record, and Cait Sith and Vincent fight unarmed in
  this engine. The clips a fight can need (its party's weapons and its scene's attacks) are read from `audio.dat` when
  the fight starts, off the game's thread, and nothing else of the 70 MB file. They share the master volume, **M** and
  `--mute`/`FMV_AUDIO` with the rest. **`FMV_BATTLE_SOUNDS`** picks which are played: `off` none, `data` (the default)
  what the data names, `all` also an enemy's death and an escape, which only a hand-made list of the sound bank names (by
  ear, and nothing says when the game plays them: a boss has a death clip of its own).
- **A field's own sounds.** A field's scripts play sounds on the game's four effect channels, with two opcodes: `SOUND` (channel 1)
  and `AKAO` 40 to 43 (channels 1 to 4; the pan and the sound id are its operands, id 0 stops the channel): 4 387 and 2 579 plays in
  446 and 260 of the 702 fields, 566 between them (`crates/fmv-ff7/src/field_sound.rs` has the evidence for all that follows, counts
  and ids only; `FMV_FF7_DATA=... FMV_FF7_AUDIO_DATA=... cargo run --release -p fmv-ff7 --example field_sound_survey` recomputes it
  from your `data/` folders, and `cargo test -p fmv-ff7 --release --test field_sound_real -- --ignored` checks it). The client plays:
  - **Ambient loops.** What a field's scripts leave playing when they have run (every entity's init part, then its main part, run as
    far as their waits and conditions can be known): 48 fields have one at the start of the story (55 loops; every one of the 208 plays
    in an init part is a stop or a loop clip). They start when you enter the field, on the channel the script names, and stop when you
    leave. **The conditions are judged against your story** (the moment and the story bits the server keeps), so the reactor of
    `nmkin_2` to `nmkin_4` hums (sound 38, channel 3) until the story reaches 27 and sounds its alarm (sound 60, channel 2) from there
    on, and **when the server moves your story the loops are read again and changed**: the hum fades into the alarm while you stand
    there. A condition on something the story does not carry (the party, a variable, the random number) is **not guessed**: what lies
    behind it is left out, counted, and said in the log; so is a loop whose channel a part of a script that was not run could still
    change. A fight takes over: the loops fade out when it starts and come back, from their start, when it is over.
  - **Ladders and jumps.** When the server puts you through one (it tells your client with `MoveRejected`, and you stand where the link
    leads), the sounds that the line's script, and the scripts of your own character it calls, play, at the times the script gives:
    the take-off of a jump (sound 62) half a second in, a ladder's first sound (sound 21) at once. A script's sounds after a walk, a jump or a
    climb have no time in the script and are not played (of the archive's 190 links, 123 in 26 fields play something at a time the script
    gives; 47 more, in 9 fields, have only sounds of that other kind: the plan line counts them, `links 5 (+1 untimed)`). **The lift** of `elevtr1` plays the sound its panel's script starts when the
    story bit it sets flips (sound 42, a loop clip, once through: its stop is behind waits the data do not give). A sound an event plays
    replaces what its channel was playing, as the game's channels do.
  - **Doors.** A gateway is data and makes no sound. A door whose script plays before its jump does when you come through it; the
    server offers script exits in the reviewed fields only, and none of those has a door with a sound (150 of the archive's 1 114
    walk-on script exits play a sound before the jump, in 63 fields; 6 of them can be followed from their line to the jump with
    everything judged).
  - **Not played, on purpose**: sounds inside a script's loop (a spark every two to four seconds in `md8_3`: the rhythm is a random
    number), a cutscene's one-shots (their picture is not shown), the sounds of a talk, and the volume and pan envelopes of `AKAO` 160
    to 175 (a sound plays at its start pan, at the master volume). Only the clips a field needs are read from `audio.dat`, off the
    game's thread, when you enter it (and again for the ones a story change adds).

  They share the master volume, **M** and `--mute`/`FMV_AUDIO` with the rest. **`FMV_FIELD_SOUNDS`** picks them: `data` (the
  default) plays what the scripts say, `off` asks for nothing and plays nothing (the field's music and a battle's sounds are not
  affected). **A party member's client keeps its own story** (the server tells it its own account's, not the leader's), so it
  hears the field for ITS moment and not the lift the leader rode: in the first mission's walk the member's client stays at the
  moment 1 throughout.
- **Keys and flags.** In the game **M** mutes and unmutes (the music keeps moving while muted); **-** and **=**
  change the volume (the character menu has no sound keys: its name field would take them as letters). `--mute` or `FMV_AUDIO=off` builds no sound at all (no device is opened, nothing is decoded);
  `FMV_AUDIO=null` runs the whole soundtrack against a clock with no device; `FMV_AUDIO=auto`, the default of
  a window, plays through the default output and falls back to the null sink when there is none. A
  `--headless` bot is silent unless `FMV_AUDIO` is set; with `null` or `auto` it logs what it plays.
  The victory fanfare is the track `fanfare` (a five-second jingle without a loop); the game's data do
  not name it, so if your ears say another track of `music.idx` is the real one, `FMV_AUDIO_FANFARE=fan2`
  (say) plays that instead.
- **When something is missing**: no sound files in the library, a missing or damaged track, no sound card. Each is one line starting
  `audio:` on stderr and silence for that part; the game carries on. A sound card is not needed to play: without
  one the client says `audio: no audio output device; continuing silently` (once; libasound's own complaints are
  kept off the terminal) and runs the same soundtrack against a clock. With one it says
  `audio: playing through "<device>" (<rate> Hz, <n> channels)`.
- **Runtime dependency.** The client needs `libasound.so.2` (ALSA; PipeWire and PulseAudio serve it through
  their ALSA plugins), which the Bazzite image has. Without it the binary does not start (the dynamic loader
  says which library it misses), so a machine that cannot have it cannot run this build.

The log is the evidence of a headless run, which has no ears: `audio: field md1stin: music ob, battle bat (the
standard theme), after the fight ob` when the area's script has been read, `audio: decoded bat: 167 s of music
in 842 ms` as each track finishes, `audio: playing bat (battle)` at the moment a track really starts, `audio: battle
sounds: 7 loaded in 5 ms` when a fight's clips were read, `audio: battle sound 18 (hit)` (or `critical`, `miss`, `impact`)
at the moment each blow's sound plays, a closing `headless: audio: sound Null, tracks started: ob > bat > fanfare > ob`,
and, when the run ends, `audio: battle sounds played: 86 (16x39 18x12 46x1 227x34)` (each sound id and how many times it
played). The field's sounds say what they read, what they start and what they play, in short lines (`audio: field nmkin_2 sounds
(moment 13): ambient ch3:38; links 3, lift 0, doors 0; unjudged 0, open 0; 13 ms` when a plan is read, and again when the story
changes; `unjudged` counts the conditions that were skipped, `open` the loops left out, a link count may say `(+1 untimed)`
for a link whose sound has no time to give it), `audio: field loop start ch3 38` and `audio: field loop stop ch3 38 (the field
was left)` (or `(the story moved)`, `(a battle)`), `audio: field sound ch1 62 (link)` (or `(lift)`, `(door)`) at the moment an
event's sound plays, and, when the run ends, `audio: field sounds played: 4 sounds (21x1 42x1 62x2), 4 loops (38x4)`.

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

The package holds no audio and no game data. The client binary links `libasound.so.2` at run time (see
"Sound"); `scripts/package.sh` does not change what it ships for that, and on a machine without a sound
card the packaged client still starts and plays on silently.

## Deployment

`deploy/` holds a systemd **user** unit template and the install, upgrade and rollback
procedure. The deployment host and port are **not assigned yet** and appear there as
UNKNOWN placeholders; see `deploy/README.md`.

## State directory layout

```
<state>/library/     owned-content library (fmv-import): blobs and one manifest per edition (the music and
                     sound files included)
<state>/server/      the server's accounts, avatar profiles, player characters (characters/, v8),
                     progression and character progress (LOCK = single writer); trash/ holds the
                     records of deleted characters, one timestamped entry each (made by the first delete)
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
scripts/battle-smoke.sh  shared-battle smoke test (real data; modes --fixture --random --progress --commands --solo --status --boss)
scripts/travel-smoke.sh  walking between fields: two bots, md1stin -> md1_1 and back (real data); --chain: the first reactor mission
                         down to the Guard Scorpion in nmkin_5 with its real story, the lift ridden, the ladders and jumps
                         climbed, the woman and the materia talked to, the fight won, the member following the leader
scripts/phase-smoke.sh   story phasing: A advances md1stin's story and B does not, party, restart, persist (real data)
scripts/slice-smoke.sh   the foundations slice: two original characters, a story, a talk, a shared battle, a real server restart
deploy/                  systemd user unit template and deployment procedure
docs/LAUNCHER.md         the package as an owner-only launcher port: layout, entry point, data dir
IMPLEMENTATION-PLAN.md   first-milestone plan and design constraints
```
