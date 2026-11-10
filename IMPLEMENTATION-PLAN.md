# FF Multiverse Rust — First-Milestone Implementation Plan

Historical first-milestone plan. The active full-game/Disc 1 scope and current coverage
are tracked in [docs/DISC1-IMPLEMENTATION.md](docs/DISC1-IMPLEMENTATION.md).
The following notes describe the original planning state, not today's implementation.

Original status: PLANNING NOTES ONLY. Written 2026-11-08 for Devbox task 13252ac0f51b.
Nothing here has been implemented, built or tested. No repository exists yet; the
parent CloudAgent creates it and owns all implementation once its devbox runtime is
connected. Source: `PROJECT-BRIEF.md` (approved direction: a NEW Rust client and
server reading owned assets).

Facts marked **[verify]** come from recall of community FF7 format knowledge, not
from inspecting owned media. The coding owner must confirm them against the owner's
actual files before relying on them.

**Brief changed while this plan was written (00:46Z).** A new "Latest deployment and
operation goals" section was appended. This plan is compatible with it:
- PokeMMO-like model: owned import, game selection, account/party, avatar,
  per-title progression.
- WoW-style phasing (§6).
- Server eventually on the existing BansheeRealm host; Linux Rust client eventually
  on the Alienware/Bazzite machine.

All host, port, auth and artifact values belong in the separate
`DEPLOYMENT-CONTRACT.md` requested there. That document is not part of this plan's
task and isn't created here. The brief also records that the parent CloudAgent
coding launch was refused (credits), so implementation is currently blocked
upstream of this plan.

---

## 0. Requirement → section checklist

| Requirement (task / brief) | Section |
|---|---|
| M1: one actual FF7 area, two connected players | §3, §8 |
| M2: party, then shared battle | §8 |
| Shared Rust core vs separate game adapters | §2 |
| Protocol | §4 |
| Persistent custom avatar | §5 |
| Story phasing and per-game progression boundaries | §6 |
| Exact owned asset inputs, required and currently missing | §7 |
| Sequential disc import as a separate pipeline, optional PC input, provenance | §7.3 |
| Fixtures are not FF7 support | §1, §8 |
| Preserve saves and originals, no redistribution, FFNx/7th Heaven unverified | §1, §7.4 |
| Build-first, minimal smoke, 45-minute checkpoints | §9 |
| Effort-setting evidence | §10 |

---

## 1. Non-negotiable constraints, carried into the design

1. **Only owned input.** Read only media and files the owner supplies. No asset bytes
   are committed to the repo, bundled in releases, or sent over the network.
2. **No redistribution between our own processes either.** The server never streams
   asset bytes to clients. Every client and the server each read their own local
   owned copy. The protocol carries asset IDs, content fingerprints and provenance,
   never payloads (§4).
3. **Originals are read-only.** Install directories, disc images and original saves
   are opened read-only. All writable state (library, settings, accounts, progression,
   logs) lives in a separate state directory, e.g. `$XDG_DATA_HOME/ff-multiverse/`.
4. **Modded installs are a distinct provenance class.** An FFNx or 7th Heaven install
   is tagged `modded`, read only, and never written to. Compatibility with FFNx or
   7th Heaven is an open investigation, not a promise.
5. **Fixture ≠ FF7 support.** Any synthetic or placeholder area is labeled `FIXTURE`
   in code, UI and release notes. M1 only counts as done when a real field decoded
   from owned media renders with two synced players.
6. **No assumed decompilation.** We re-implement the systems we need (field walk,
   battle subset) from file formats. We don't assume a complete FF7 PC decomp
   exists. Community references (ergonomy-joe, Xeeynamo, OpenVIII, Memoria) are
   reading aids only, not code to copy.

---

## 2. Shared core vs game adapters (proposed workspace layout)

```
ff-multiverse/                 (Cargo workspace; created by the parent CloudAgent)
  crates/
    fmv-core/        game-agnostic: ids, entity/transform, sessions, party, phasing keys,
                     battle-instance framework (tick and turn authority), event bus
    fmv-protocol/    versioned wire messages, codec, handshake, asset-fingerprint types
    fmv-persist/     accounts, avatar profiles, per-game progression (SQLite via rusqlite)
    fmv-server/      authoritative server binary: tick loop, area instances, validation
    fmv-client/      client binary: window/render (wgpu + winit), input, net, UI
    fmv-library/     owned-content library: provenance manifest, content-addressed index
    fmv-import/      import pipeline: disc reader, ISO9660, PC-install scanner, identify
    fmv-adapter-api/ trait boundary that every game adapter implements
    fmv-ff7/         FF7 adapter: LGP, field, model, texture, battle-data decoders and runtime
    (later) fmv-ff8, fmv-ff9, fmv-ff10, fmv-ff12
```

**Core owns (game-agnostic):**
- Account and session identity, connection lifecycle.
- World and area instances as opaque `AreaId { game, adapter_area_key }`.
- Entity positions: core stores adapter-provided coordinates and leaves movement
  rules to the adapter.
- Party: membership, leader, invites, follow/regroup.
- Phasing: opaque `PhaseKey` comparison and instance selection (§6).
- Battle-instance framework: participants, server tick, command queue, deterministic
  RNG seed, result broadcast. Rules come from the adapter.
- Avatar profile storage (§5) and per-game progression blobs (§6).

**The adapter trait (`fmv-adapter-api`)** is roughly:
- `identify(library_entry) -> Option<EditionProvenance>`
- `list_areas()`, `load_area(area_key) -> AreaRuntime` (walkmesh or collision,
  spawn points, gateways, encounter table ref)
- `validate_move(area, from, to) -> Result<Pos>`, used by the server for collision
  authority
- `render_assets(area_key)`, client side only: backgrounds, models, textures
- `avatar_appearances()` maps a core avatar profile to this game's playable models
- `phase_key(progression) -> PhaseKey`, `default_progression()`
- `battle_rules()`: formation load, turn/ATB timing, command resolution

**FF7 adapter owns:** LGP archive reading, field file decode, field model and
animation decode, TEX/palette decode, scene.bin and kernel.bin parsing, the FF7 walk
and camera model, the FF7 battle subset, and the FF7 progression/phase mapping.

**Rule:** core never imports an adapter. Server and client link adapters behind
the trait, so FF8+ adapters can be added without touching core.

---

## 3. Milestone 1 definition: one actual FF7 area, two connected players

**Done when:**
1. The owner imports their owned media (or points at an installed PC copy). The
   library records full provenance.
2. The server and both clients each read the same field from their own local owned
   copy. Fingerprints match at handshake.
3. Each client renders that field: background layers and walkmesh-constrained
   movement with the correct camera.
4. Each player appears as an FF7 field model chosen via their avatar profile, both
   clients see both players, and positions are synced in real time.
5. The server rejects moves that leave the walkmesh.

**Candidate first field [verify]:** `md1stin`, the Sector 1 station opening platform,
or another early Midgar field present in the owner's edition. Gateways, field
scripts, dialogue and NPCs stay disabled in M1. The area runs in a static
"free roam" state, clearly labeled.

**Out of scope for M1:** field scripts/events, NPCs, music/SFX, FMV, menus, world map,
saving into FF7 save format.

---

## 4. Protocol (M1 shape)

- **Transport:** TCP with length-prefixed frames on LAN/tailnet for M1, kept behind a
  `Transport` trait so QUIC (quinn) or UDP can replace it later without touching
  message types. Codec: `postcard` (serde), with an explicit `protocol_version: u16`.
- **Handshake:**
  `ClientHello { protocol_version, client_build, account_token, adapter_versions }`
  → `ServerHello { session_id, server_build, accepted }`.
- **Content agreement (no bytes exchanged):**
  `AreaManifest { game, area_key, edition: EditionProvenance, fingerprint: blake3 }`.
  The client answers `HaveArea { fingerprint }` or `MissingArea { reason }`. If the
  fingerprint differs (different region or revision), the client can't join that
  instance. An explicit cross-edition ID mapping is a later feature, never an
  implicit merge.
- **M1 messages:**
  - `JoinArea`, `AreaSnapshot { entities }`
  - `MoveIntent { seq, pos, facing, anim_state }` (client → server)
  - `EntityUpdate { id, pos, facing, anim_state, server_tick }` (server → clients,
    about 20–30 Hz)
  - `MoveRejected { seq, corrected_pos }`
  - `PlayerJoined`, `PlayerLeft`
- **M2 additions:**
  - `PartyInvite`, `PartyAccept`, `PartyState`
  - `EncounterStart { battle_id, formation_ref, seed, participants }`
  - `BattleCommand { battle_id, actor, command, target }`
  - `BattleEvent { tick, events }`
  - `BattleEnd { result, rewards_per_player }`
- **Authority:** the server owns positions (validated against the walkmesh), party
  state, encounter rolls and battle resolution. Clients predict locally and
  reconcile.

---

## 5. Persistent custom avatar

- An `AvatarProfile` lives in core/persist:
  `{ avatar_id, account_id, display_name, base_archetype, color/variant choices,
  per_game_overrides: Map<GameId, AppearanceRef> }`.
- It stores references only: `AppearanceRef` is an adapter-defined ID, such as
  "FF7 field model X, variant Y". The profile never stores mesh or texture bytes, so
  the database is safe to back up and share.
- **FF7 mapping in M1:** the player picks one of the FF7 field character models from
  their own owned copy. Display name and tint are core-level and show on every game.
- **Persistence:** SQLite in the server's state directory. Profiles persist across
  sessions and across games. Each game's adapter resolves the profile to its own
  closest model, with a fallback if no mapping exists.
- **Later:** a deeper custom-avatar editor, game-agnostic body/outfit parameters,
  per-game renderers.

---

## 6. Story phasing and per-game progression boundaries

- **Per-game progression is separate per player:**
  `Progression { account_id, game, blob, schema_version }`. Only the adapter
  interprets the blob. FF7's blob would hold the story-progress value, the
  equivalent of the save's "game moment" counter **[verify offset]**, plus flags
  the adapter needs.
- **Phase key:** the adapter maps progression to an opaque, comparable `PhaseKey`
  for a given area. Core compares keys and picks instances; it never interprets them.
- **Instance rules:**
  - **Solo:** a player enters the instance matching their own phase key.
  - **Party:** the party leader's phase key selects the shared instance. Members
    keep their own progression. Story advancement inside a shared instance applies
    only to members whose progression permits it (configurable later).
  - **M1:** a single "free roam" phase for the chosen field. No story state is
    executed or changed. The fields exist in the data model, so M2+ doesn't need a
    schema change.
- **Cross-game boundary:** progression never leaks between games. Account, avatar
  and party are shared; abilities, inventory and story are per game.
- **Saves:** importing an original `.ff7` save to seed progression is optional and
  later. It is always read-only and never writes back to the original.

---

## 7. Owned asset inputs

### 7.1 Critical-path gate: which owned media exists (owner input required)

PC data (LGP archives) and PlayStation disc data (raw files on ISO9660) need
**different decoders** for fields, models and textures. The first real-area
increment (§8, M1-b onward) can't be scheduled until the owner declares:

- **Platform and edition:** PC 1998 (Eidos/Square), PC 2012 Square Enix store
  re-release, Steam 2013, PS1 NTSC-U, PS1 PAL (language), PS1 JP original, PS1 JP
  International. Later console ports (PS4, Switch, etc.) are a different format
  family and out of scope until assessed.
- **Region, language and revision** (e.g. disc version, patch level).
- **Path:** install directory or disc images (BIN/CUE, ISO, CHD), or a physical
  drive for sequential disc import.
- **Modding state:** whether FFNx or 7th Heaven is installed in that copy (affects
  layout; tagged `modded` per §1.4).

**Branch A, PC install available (likely fastest to a real area):** decode LGP → field
files → background, walkmesh and camera; char archive → HRC/P/TEX field models.

**Branch B, PS1 discs only:** implement the disc pipeline first (§7.3). Then decode
the PS1 field formats (`FIELD/*.DAT` scripts and walkmesh, `*.MIM` backgrounds,
`*.BSX` models **[verify]**). The adapter trait is the same, but the decoders are
different.

### 7.2 Exact inputs needed for M1 and M2 (Branch A paths [verify] against owned copy)

| Purpose | PC file (1998-style layout) [verify] | PS1 equivalent [verify] | Milestone |
|---|---|---|---|
| Field backgrounds, walkmesh, camera, gateways, encounter refs | `data/field/flevel.lgp` | `FIELD/*.DAT`, `*.MIM` | M1 |
| Field character models and animations | `data/field/char.lgp` | `FIELD/*.BSX`, related | M1 |
| Game kernel data (stats, items, magic, text) | `data/kernel/KERNEL.BIN`, `kernel2.bin` | `INIT/KERNEL.BIN` | M2 |
| Enemy formations, scenes | `data/battle/scene.bin` | `BATTLE/SCENE.BIN` | M2 |
| Battle models, backgrounds | `data/battle/battle.lgp` | `BATTLE/*` | M2 |
| Music, SFX, FMV, world map | `data/sound`, `data/movies`, `data/wm/*` | various | later |
| Original saves (optional seed, read-only) | `save/save0*.ff7` (1998); Steam: per-user documents folder | memory card images | later |

The Steam 2013 and 2012 re-releases reorganized some data into language-specific
folders (e.g. `data/lang-xx/`). That layout is **uncertain** and must be checked on
the owner's copy.

### 7.3 Sequential disc import (separate owned-content pipeline)

Independent of the client and server; can be developed in parallel.

1. **Session:** "Import FF7 (disc set)" opens one import session that persists across
   disc swaps.
2. **Per disc:** read sectors (physical drive raw read, or BIN/CUE/ISO/CHD image),
   parse ISO9660 and identify the disc. Identification uses the boot executable ID
   in `SYSTEM.CNF` (e.g. SCUS/SCES/SLPS serials **[verify exact IDs]**), the volume
   label, the disc number and content hashes.
3. **Copy:** copy the needed files into the content-addressed library
   (`library/blobs/<blake3>`). Files shared across discs are deduplicated by hash,
   and the source disc of each file is recorded.
4. **Prompt:** "Insert disc N" until the set is complete or the user stops. A
   partial set is allowed, but only fields present can load.
5. **Select later:** once complete, the user picks the game from the library with no
   further disc swaps.

**Optional PC input:** scan an installed directory read-only, identify the edition
from file presence, sizes and hashes, and index files the same way.

**Provenance per library entry:**
`{ game, platform: PC|PS1, edition, region, language, revision, source: disc N
serial | install path, modded: bool, imported_at, file_hashes }`.

**Incompatibility rule:** different edition, region or revision assets are never
mixed in one library set. Disc 2 of a PAL set can't complete an NTSC-U set.

**Feasibility to verify by the coding owner:** raw CD reading on Linux (ioctl/SG
access, read-only), CHD support (crate availability), and sector modes (MODE2/2352).
Universal disc-format compatibility won't be claimed.

### 7.4 Currently missing (as of this plan)

- **No owned media in the project home, and none declared by the owner.** Per task
  scope, nothing was searched beyond this project directory.
- Owner declaration of edition, platform, region, revision and path (§7.1). **This
  blocks M1-b.**
- Whether the owner's copy is vanilla or FFNx/7th Heaven-modded.
- A repository and a connected devbox worker. Per the brief, the Cursor CLI is not
  logged in and the worker hasn't started. Both are parent CloudAgent and
  infrastructure gates.
- Verified format specs for the chosen branch, from primary sources and the owned
  files themselves.

---

## 8. Increment sequence (targeting 45-minute checkpoints)

Each increment ends with a focused build, a startup check and a one-feature smoke
(§9), then goes to the owner to playtest.

| # | Increment | Ready-check (smoke) | Real FF7? |
|---|---|---|---|
| M1-a | Workspace skeleton, `fmv-protocol` handshake, server plus two clients see each other as markers on a **labeled FIXTURE plane** | Server starts; two clients connect; both log `PlayerJoined`; markers move on both screens | **No: FIXTURE, not FF7 support** |
| M1-b | `fmv-library` and `fmv-import` PC-install scanner (Branch A) or disc reader (Branch B); provenance manifest written to the state dir; originals untouched | Import command lists identified edition and indexed files; source dir mtime/hash unchanged | Reads owned media; no rendering yet |
| M1-c | FF7 field decode: LGP extract (in memory) → chosen field's background layers and walkmesh; client renders background with walkmesh debug overlay | Client opens the chosen field and the background is visible | **Yes, if decoded from owned media** |
| M1-d | Field model: load one FF7 field character model and idle/walk animation; walk on the walkmesh with the field camera | Character walks and stays on the walkmesh | Yes |
| M1-e | Network the real field: fingerprint handshake, server-side walkmesh validation, two clients synced in the actual field with avatar-chosen models | Two clients in the same real field see each other move; off-mesh move is corrected | **Yes: M1 complete** |
| M2-a | Persist accounts, avatar profiles and progression stubs (SQLite) | Restart server; avatar persists | Yes |
| M2-b | Party: invite/accept, leader, party UI, phase-key instance selection (single phase for now) | Two players form a party; state shows on both | Yes |
| M2-c | Shared battle: server rolls or forces an encounter from the field's encounter table; loads the formation from `scene.bin`; minimal battle view; server-authoritative ATB/turn; commands from both players; results to both | Both clients enter the same battle; both issue Attack; HP changes match on both | Yes (battle subset) |
| M2-d | Battle return: rewards applied per player's own progression; back to field | Both return to field with separate progression updated | Yes |

Disc import (Branch B, §7.3) runs as a parallel track. If the owner has only PS1
discs, it moves onto the critical path before M1-c.

---

## 9. Build and smoke policy

- Build features first. Per increment: build only the changed crates
  (`cargo build -p <crate>`), run a startup check (binary starts and exits or serves
  cleanly), and do a single smoke of the changed feature. Then hand off to the owner,
  who playtests and reports bugs.
- No routine full test suites, test swarms or independent-review gates. Per the
  brief, avoid bulk compiles during infrastructure setup. Choose cargo job counts
  from measured load and memory, not fixed ceilings.
- Essential checks are kept: originals and saves never modified (read-only opens,
  before/after hash on the source in the import smoke), no asset bytes on the wire
  or in the repo (a `.gitignore` for library/state dirs; a protocol review that no
  message carries payload bytes).
- Never report an unrun check as passed. Report blockers concretely instead of
  claiming a release.

---

## 10. Effort-setting evidence (this session, read-only inspection)

Question: launch arguments say `--effort max`, but the visible header says `high`.

| Source | Value |
|---|---|
| `claude` process argv (PID 1662323) | `--model claude-opus-5-5[1m] --effort max --permission-mode bypassPermissions ...` |
| `claude` process's own environment (`/proc/1662323/environ`) | no `EFFORT`/thinking variables |
| Tool child-shell environment (injected by Claude Code) | `CLAUDE_EFFORT=high`, `CLAUDE_CODE_EFFORT_LEVEL=high` |
| `~/.claude/settings.json` | `modelSettings.claude-opus-5-5.effortLevel: "high"` |
| Coordinator task record routing | `model_default.effort: "high"`, `requested_effort: null` |
| `claude --help` (v2.1.319) | `--effort <level>` accepts `low, medium, high, xhigh, max` |

**Conclusion:**
- The effort variables are absent from the launcher-level process environment. They
  appear only in the environment Claude Code passes to tool processes, which points
  to Claude Code reporting its effective session level as **high**.
- So **max is not effective**, and the header is accurate, not stale.
- **Why the `--effort max` flag lost is undetermined.** Candidates: the
  `settings.json` per-model `effortLevel: high` entry overriding the flag, max being
  clamped for `claude-opus-5-5[1m]`, an in-session `/effort high`, or another
  source. No documentation was consulted (offline scope), so no precedence rule is
  asserted.
- No configuration was changed.

**Recommendation for the owner/launcher (not applied):** if max is wanted, the owner
identifies the overriding source first, without blind edits to global settings.
After relaunch, re-verify via the header and the injected tool environment.

**Risk for the parent CloudAgent launch:** the brief requires the coding owner at
maximum effort. The same per-model setting could silently downgrade that launch, so
verify the effective level after launch, not just the flag.
