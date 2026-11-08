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
   opened read-only. All writable state (library, settings, accounts, progression, logs)
   lives in a separate state directory (planned default: `$XDG_DATA_HOME/ff-multiverse/`).
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

**Milestone 1 (one real FF7 field with two connected players) is in progress.**
Real-FF7-field rendering has not landed: decoding a field from your own PC data, drawing
it, walking on its walkmesh and syncing two players on it are still being built. Until M1
is complete, any area a server hosts is the synthetic **FIXTURE** plaza, which is test
content only and says nothing about FF7 support.

| Milestone | Scope | State |
|---|---|---|
| M1 | One real FF7 field, two connected players, server-side walkmesh validation | in progress |
| M2 | Accounts and avatars, parties, then a shared battle | planned |
| Later | FF8, FF9, FF10 and FF12 adapters on the same core | planned |

The increment-by-increment plan is in `IMPLEMENTATION-PLAN.md`.

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
| `fmv-library` | Owned-content library: provenance manifest and content-addressed index. Never writes to original sources. |
| `fmv-import` | Owned-content import CLI: PC install scan now, sequential disc import planned. |
| `fmv-ff7` | FF7 adapter: format decoders (LGP, LZSS, field) and the FF7 area runtime. Reads owned data only. |
| `fmv-ff7-battle` *(coming)* | FF7 battle subset for the shared-battle milestone. |
| `fmv-persist` *(coming)* | Accounts, avatar profiles and per-game progression in SQLite. |
| `fmv-party` *(coming)* | Party membership, leader and phase-key instance selection. |

## Quickstart

Requirements: a recent stable Rust toolchain on Linux. Dependencies resolve from the
local cargo cache (`.cargo/config.toml` sets `net.offline = true`; remove that line when
adding new crates).

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

`fixture=true` means it is hosting the synthetic FIXTURE area.

**Run two headless bots** (use `addr` from the READY line). Each runs for the given
duration and then prints one SMOKE line:

```
target/release/fmv-client --server <addr> --name A --headless --duration 5
target/release/fmv-client --server <addr> --name B --headless --duration 5
SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n>
```

`peers_seen` of at least 1 means that client saw the other player. These two line formats
are a contract: `scripts/smoke.sh` parses them.

**Window mode** (needs a display):

```
target/release/fmv-client --server <addr> --name Me
```

**One command for all of the above:** `scripts/smoke.sh` starts the server, runs two
headless bots concurrently, checks both SMOKE lines and stops the server. It uses
`target/release` by default; `--debug`, `--build` and `--bin-dir <dir>` are available, and
`--help` lists the rest. It runs the processes with throw-away XDG directories, so it never
touches your real library or state.

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
at the handshake and exchange no asset bytes.

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

## Repository layout

```
crates/                  the Cargo workspace (see the table above)
scripts/package.sh       build and package a release tarball
scripts/smoke.sh         two-bot local smoke test
deploy/                  systemd user unit template and deployment procedure
IMPLEMENTATION-PLAN.md   first-milestone plan and design constraints
```
