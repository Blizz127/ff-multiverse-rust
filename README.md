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
from its own `scene.bin` and `kernel.bin`. The synthetic **FIXTURE** plaza still exists
as a labelled test area; it says nothing about FF7 support.

| Milestone | Scope | State |
|---|---|---|
| M1 | One real FF7 field, two connected players, server-side walkmesh validation | done |
| M2-a | Accounts, persistent avatar profile, per-game progression store | done |
| M2-b | Parties (invite, accept, leave, kick, promote), phase-key instance placement | done (single phase) |
| M2-c/d | Shared battle, battle return, per-player rewards | done (battle subset; rewards logged and sent, not stored yet) |
| Later | FF8, FF9, FF10 and FF12 adapters on the same core | planned |

Not done yet: choosing an FF7 character model for your avatar, NPCs, field scripts, music, and
the window's scrolling camera (it shows the whole background). The increment-by-increment plan
is in `IMPLEMENTATION-PLAN.md`.

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
the window, **WASD** walks, **P** invites the nearest player, **Y** accepts an invite, **L**
leaves the party. A missing display, GPU or server is reported with one clear line.

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

**Run two headless bots** (use `addr` from the READY line, the same `--game ff7` flags as the
server when it hosts FF7). The first time a bot connects it registers its `--name` as a new
account and saves the token it is given; each bot runs for the given duration and then prints
one SMOKE line:

```
target/release/fmv-client --server <addr> --name A --headless --duration 5
target/release/fmv-client --server <addr> --name B --headless --duration 5
SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n> party=<n> battles=<n> victories=<n>
```

`peers_seen` of at least 1 means that client saw the other player; `party` is the largest
party this client was in during the run (0 if none). Bots can form a party: give one
`--party-invite-first-peer` and the other `--party-accept`, and both lines show `party=2`.
`battles` and `victories` (appended in protocol v3) count the shared battles the client
entered and the server ended in victory. These line formats are a contract: `scripts/smoke.sh`
parses them by key (it ignores the fields it does not know).

**Window mode** (needs a display):

```
target/release/fmv-client --server <addr> --name Me
```

WASD or the arrow keys walk (up walks up the screen, whatever the camera looks like), **P**
invites the nearest player to your party, **Y** accepts the pending invite, **L** leaves the
party, Esc quits. Party members get a gold ring around their marker, and the window title
shows your party size and a pending invite. **B** asks the server for a debug battle (refused
unless it runs with `--debug-encounters`). In a battle the window shows enemy and party bars
(HP and ATB gauge; a yellow border means that character can act) and **Space** or **Enter**
attacks the first living enemy with your character. (The battle view has been checked by unit
tests only: this box has no display.)

**One command for the basics:** `scripts/smoke.sh` starts the server, runs two headless bots
concurrently, checks both SMOKE lines and stops the server. It uses `target/release` by
default; `--debug`, `--build` and `--bin-dir <dir>` are available, and `--help` lists the
rest. It runs the processes with throw-away XDG directories, so it never touches your real
library or state.

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
rewards (experience and gil shared among the living, AP to each): they are logged and sent, not
stored, because an account's progression blob is also its story phase. A server without
battle data (the FIXTURE one, or `--ff7-flevel`) refuses every battle request cleanly.
`scripts/battle-smoke.sh` runs two bots through one real battle on `md1stin`.

**Protocol v3** (`fmv-protocol`) carries all of the above; its module docs describe the flow,
and golden JSON tests pin the wire format (the engine's own event, command and outcome types
included). A client or server of another protocol version is refused with a clear message.

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
three binaries, this README, a generated `LICENSE-NOTICE.txt` and a `manifest.json` into
`dist/ff-multiverse-<version>-<gitshort>-linux-x86_64/`, and writes the `.tar.gz` plus a
sidecar `.manifest.json` next to it. The manifest records the version, git commit
(`-dirty` if the tree is dirty), rustc version, target triple, build time and the sha256 of
each binary; the sidecar adds the tarball's sha256. The script prints the tarball path
and its sha256, honours `CARGO_TARGET_DIR`, and can be re-run safely. It fails if any
staged file is off the allowlist or matches an owned-data pattern. It needs `jq`.

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
<state>/server/      the server's accounts, avatar profiles, progression (LOCK = single writer)
<state>/client/      a client's saved login tokens: accounts/<server>.token (mode 0600)
```

`<state>` is `$FMV_STATE_DIR`, else `~/.local/share/ff-multiverse`. Back up `server/` by
stopping the server and copying it (a hot snapshot API exists in `fmv-persist`, not yet wired
to a command). Never commit or share any of it.

## Repository layout

```
crates/                  the Cargo workspace (see the table above)
scripts/package.sh       build and package a release tarball
scripts/smoke.sh         two-bot local smoke test
deploy/                  systemd user unit template and deployment procedure
IMPLEMENTATION-PLAN.md   first-milestone plan and design constraints
```
