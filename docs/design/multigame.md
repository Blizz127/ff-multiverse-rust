# Multi-game design: FF8 and FF9 next to FF7

Status: DESIGN NOTES ONLY. Written 2026-11-09 by teammate `sw-ffdesign` (swarm wave 4). The code audit was
read at `main` `2fea4b5` and re-checked after the talk-dialogue merge `aa3462a` and its follow-up `094681e`. Docs only: nothing here is
implemented, built or tested. Scope: FF8 and FF9. Later titles (FF10, FF12) wait for an owner clarification and
are not covered; nothing below should make them harder.

**No owned FF8 or FF9 files exist on this box.** Every claim about how an FF8 or FF9 file or
archive is laid out carries a tag, in one of three tiers:

| Tag | Meaning |
|---|---|
| `[verify: doc <id>]` | A public page I fetched and read says so (source ids in Appendix 1). Still unconfirmed against the owner's files: those pages describe particular builds. |
| `[verify: snippet <id>]` | Seen only in a search-result snippet, or resting on one forum poster's remark (even on a page I fetched). A lead, not a documented fact. |
| `[verify: inference]` | No source: recall, a guess, or an inference. May simply be wrong. |

The HobbitDur FF8 pages (the most detailed source) describe the 2000 PC build (`FF8_EN.exe`) and,
for its exe-table page, the 2013 English PC release. **None of them describes the 2019 Remaster.**
The FF7 pipeline is not assumed to carry over: where this design reuses an FF7 idea (archive
reader, tile-and-depth backgrounds, walkmesh plus gateways) the FF8/FF9 equivalent is a claim to check.
The per-game evidence tables (claim, source, tier, how to check on owned files) are in Appendix 2,
so that the body stays about decisions.

---

## 0. Summary

1. **Connected, but different (the owner's rule).** Accounts, one persistent original character per
   player, parties, phasing, networking, the launcher and the owned-content library are shared.
   Each game keeps its own combat, progression, menus, art style and feel. The shared layer unifies
   *envelopes* (who sits in a battle, whose input is accepted, the input log, replay verification,
   result fan-out) and never the *engine* (section 1).
2. **The owner's cross-game character and story model is the top requirement (section 2).** One
   original character per account travels across FF7, FF8 and FF9; in each game it fills a canon
   combat slot next to the cast, drawn in that game's art style from the owner's own files, while the
   canon cast stays unique, NPC-driven and untouched in story scenes, and the player is acknowledged
   by an added dialogue layer. Today's code conflates look, combat character and identity (the look
   picker decides who you fight as, and the default look is Cloud), so FF7 needs a retrofit
   (section 2.10) before the model can be shown honestly in any game.
3. **The shared layers are mostly ready; three seams are FF7-shaped (section 3).** The battle
   messages on the wire re-export the FF7 engine's own types and `fmv-protocol` depends on
   `fmv-ff7-battle`; the server calls the concrete FF7 `BattleState` and has FF7 `match` arms for
   looks and characters; the client is an FF7 renderer and battle UI used directly (49 client
   files import the FF7 crates).
4. **The critical path to a first two-player FF8 area is short and needs none of those three seams
   except the field half of the client one (section 9.1).** It is a field adapter on today's single-game
   server, exactly as `md1stin` was: importer generalisation, an FI/FL/FS reader, field decode (walkmesh,
   camera, gateways, background), an FF8 `GameAdapter`/`AreaRuntime`, and the field half of the client
   presentation seam. Battle, the battle envelope, multi-game hosting and the identity/slot model come
   after it.
5. **The story model's scenes rest on a hosted script VM that does not exist for any game.** FF7 has
   a bounded, read-only *talk-script walker* (`fmv_ff7::script::dialogue`, merged in `aa3462a`) that runs an
   NPC's talk script just far enough to collect its message windows and never changes the world; the
   README still lists world-changing field scripts as not done. "Cast stays NPC-driven per player phase"
   and "player visibly present in story scenes" are blocked on a hosted VM per game, while the **talk half
   of the dialogue layer already has its seam** (section 2.8). The milestone tables keep the VM on its own
   track and do not pretend otherwise.
6. **FF9's packaging is the biggest technical risk.** The Steam release is a Unity build whose data
   sits in Unity archives (`p0data*.bin`, `*.assets`) and, for gameplay tables, possibly in compiled
   code rather than data files. A cheap read-only probe of the owner's install should come before
   any FF9 commitment (section 6.2, 8).
7. **Thirteen owner decisions are listed in section 11, each with a recommendation**, and the owned
   files the owner must supply per game are in section 8.

---

## 0.1 The owner's vision and play model (2026-11-09, relayed by the Devbox coordinator)

The owner's words (10:28 CST): "ff14 + pokemmo is the best way of doing it for myself, the best part is i can play solo
and go through the games or select whatever game or with friends. cool part is connor playing pokemmo alone doing his
stuff we can team up or go solo. being able to take a "stargate" to other ff worlds to play their game is great too."
Earlier the same morning: "probably need a 3-4 max party", "do what pokemmo does" (co-op) and, for story scenes, "ff14
aand ff11 is the best way of doing it" ("perfect ok lets do it"). The design targets that follow from them:

* **Solo or co-op at any time.** Players share the world and see each other (as today, per story phase), and each keeps
  their OWN progress per game. Battles are solo by default, fought beside the player's own canon companions; fighting
  together is opt-in (PokeMMO's model: ordinary battles are solo even inside a party, co-op is group content such as
  raids). A party is a social group the players form and leave freely, never a forced shared battle. **This changes
  today's rule** (a party always battles together) and is planned for after the single-player early game.
* **A battle holds at most 4:** the story's canon party first, then as many players' own characters as fit (two in the
  first mission: Cloud, Barret and two originals). Nobody canon is displaced; a player who does not fit plays on solo.
  Story bosses can be fought as a linked group, raid-style, each player's story moving only if they stand at that point.
* **Story scenes like FF14 and FF11.** A player's Interact starts a per-player scene instance; the canon cast speak their
  real lines from the owner's own files; the player's character is present as a witness, never in Cloud's place; party
  members nearby can watch together. Scenes are data (a script reference plus a cast list), replayable with a custom
  cast. The first proof is Cloud and Tifa at the bar in 7th Heaven (Game QA acceptance: the talk starts it, the lines
  play in order, the witness is at the right scale with no clipping, camera and control come back cleanly, no replay or
  soft-lock after a save and reload). This is the hosted-script-VM track of section 2.7.
* **Pick any FF game, and travel between them.** One original character per player crosses every game (section 2),
  each game keeping its own feel (section 1). The owner wants an in-world way to travel between the FF worlds (his
  "stargate"): a portal hub that is ORIGINAL to FF Multiverse (no game's art), from which each game's world is entered
  at that player's own progress in it. It needs the multi-game hosting of section 4 (today's server hosts one game) and
  an identity that is already game-agnostic (it is: `PlayerCharacter`, per-game bindings and progress pointers).

---

## 1. Principle: connected, but different

| Concern | Connected: one implementation, shared | Different: one per game, never unified |
|---|---|---|
| Players | accounts and tokens, display names, one original character (identity), friends | the character's build in that game's own systems (FF7 materia/limits, FF8 junction/GF/draw, FF9 equipment abilities/Trance) |
| World | area hosting, movement authority, instances, phase-key comparison, party formation | the area data, walk rules, camera, exits, NPC placement, scripts and story variables |
| Battle | **lifecycle**: seating, seeded RNG, tick authority, input log, replay check, result fan-out, reward-settlement hooks | rules, formulas, timing model, command set, enemy AI, battle UI |
| Persistence | atomic records, single writer, schema versions, size caps | what a game's progression and character-sheet blobs contain |
| Content | the library (content-addressed blobs, provenance manifest, fingerprints); "bring your own copy, read-only, never on the wire" | file formats, decoders, edition detection |
| Client | window, input, UI toolkit, character menu, party panel, name rendering | field renderer, model loaders, battle overlay, menus, fonts and window skins |
| Minigames | the table: challenge, seat, turn timer, settle | Triple Triad (FF8) and Tetra Master (FF9) rules |

Rule of thumb: **unify the envelope, never the engine.** If a rule is identical in all three games it
is probably FF7's rule wearing a general name.

```
fmv-core        ids, provenance, phase keys, identity types          (never imports a game crate)
fmv-party  fmv-persist  fmv-library                                   game-agnostic services
fmv-adapter-api   what an adapter offers: areas today; appearances, slots, progression, battle later
fmv-ff7 / fmv-ff8 / fmv-ff9  (+ fmv-ffN-battle)     may use core and adapter-api, never each other
fmv-protocol    envelopes only: no game crate        fmv-server / fmv-client host adapters via traits
```

---

## 2. Cross-game character and story model

This section is the owner's direction (relayed 2026-11-08, 5:31 to 5:34 PM CST). Everything else in
the document is designed around it.

### 2.1 Requirements, as relayed

| # | Requirement |
|---|---|
| O1 | Each player has **one persistent original character** (identity and appearance) that travels across FF7, FF8 and FF9. "Thirty Clouds running around" is exactly what the owner does not want. |
| O2 | In each game that character takes a **canon combat/party slot** (for example fighting in Tifa's slot) alongside the protagonist. The canon character is **not erased** from story scenes or quests. |
| O3 | Protagonists and cast stay **unique and NPC-driven per player phase** (Cloud, Squall, Zidane), and the cast keeps its defining moments. |
| O4 | The player is **visibly present in story scenes**, mostly observing, taking part where natural, like an adventurer in FF14, FF11 or WoW: fights alongside the cast, asks questions, offers help, is acknowledged. Dialogue addresses the player naturally. **Never name-swap the player into Cloud's lines;** player-facing lines are an added content layer. |
| O5 | Identity and appearance persist across games but are **rendered in each game's art style**. Abilities **adapt to each game's own system** (FF7 materia/limits, FF8 junction/GF, FF9 equipment abilities/Trance). |
| O6 | **Friends join as their own characters**: parties of original characters plus the canon cast as needed. |

### 2.2 The model: five things the code conflates today

| Thing | Scope | What it is | Today (code) | Target |
|---|---|---|---|---|
| **Identity** | account, all games | name, a coarse look specification (build, hair, skin, outfit palette, tint), optional pronouns | `AvatarProfile { display_name, tint, base_archetype }` in `fmv-persist`; only `"default"` is ever used as the archetype | `base_archetype` grows into a structured `AvatarSpec`; additive fields |
| **Appearance resolution** | per game | which model in *that game's own art* draws this identity | `per_game_overrides: game -> appearance_ref`; the server accepts only ids the adapter lists (`accounts::canonical_appearance`, FF7 only) | adapter publishes a **wardrobe** built from the owner's files; the spec is matched to it; the chosen id is the override |
| **Combat slot template** | per game | which canon combat role the original fills: growth curve, weapon class, command or limit set | derived from the **look**: `progress::character_for_look` + `FF7_LOOKS`; recomputed at every login (`progress::resolve` overwrites `character`) | an explicit, stored **slot** choice, independent of the look |
| **Character sheet** | per game | everything the game's own systems keep: levels, AP, equipment, materia, junctions, GFs, stock, abilities, trance, card collection | `CharacterProgress { character, level, exp, ap, gil, inventory, limit }`: FF7-shaped | a per-game sheet blob with schema version, plus a small generic summary |
| **Story phase** | per game | where the player is in that game's story (selects the instance) | `Progression` blob -> `PhaseKey` (`accounts::phase_key`), opaque to core | unchanged; layer flags (2.8) live beside it, never in it |

Progression never leaks between games (plan section 6): only identity, friends and the launcher are shared.

### 2.3 Seating: who is in the battle

The roster is built **per battle by the server** from four inputs and recorded in the battle start
message (so replays and reconnects reproduce it):

1. the players in the battle (their accounts, slot templates and sheets);
2. the **canon party of the current segment** (not "the protagonist of the game": see below);
3. the game's slot count (FF7 3, FF8 3, FF9 4 `[verify: inference]` for the data-side number);
4. the battle's **roster policy**.

```
seat each player first, as Player(account, template)
for each seated player whose template equals a canon member of the segment's canon party:
    that canon member steps aside (not in this battle; still in the story)
fill the remaining slots from the canon members who did NOT step aside, in the segment's
    priority order (segment lead first), as AI companions with canon sheets for the phase
canon members left over stay out of the battle (off screen)
(no canon member of that template present: the player simply takes a free slot, or, if none
 is free, the lowest-priority canon member is left out)
```

| Roster policy | Meaning | Default for |
|---|---|---|
| `PlayersFill` | players seat first, canon cast fills what is left | random encounters, most boss fights |
| `CastLed` | the segment lead is guaranteed a seat, players take the rest | fights the story is about that lead |
| `CastOnly` | a defining moment: the cast fights, the players watch from observer marks and share a small participation reward | duels and set pieces the owner names (a curated list per game) |

**The segment lead is data, not a constant.** FF7 has stretches without Cloud, FF8's Laguna dreams
replace the whole party, FF9 splits the party and has stretches led by Steiner, Vivi or Dagger. The
canon party, the lead and the slot count therefore come from **segment data per phase**
(`SegmentCast { lead, party, slots, policy }`), curated from the game's own script state. What the
originals do while the cast is swapped out is owner decision D6.

Worked examples (players A, B; templates in brackets):

| Game, situation | Roster |
|---|---|
| FF7, segment lead Cloud, A (Tifa slot) alone | Cloud (AI), A, Barret (AI): Tifa steps aside |
| FF7, A (Tifa), B (Barret) | Cloud (AI), A, B: Barret and Tifa step aside, nobody else needed |
| FF7, three players in a party of three | A, B, C: the lead yields; Cloud stays in the story, not in the fight |
| FF8, Squall's segment, A (Zell slot), B (Selphie slot) | Squall (AI), A, B |
| FF8, a Laguna dream | `CastOnly`: Laguna, Kiros, Ward fight; A and B watch from marks |
| FF9, A (Freya slot), B (Vivi slot), Zidane's segment | Zidane (AI), A, B, plus one canon member (AI) |

Consequences for existing numbers: `PartyConfig::max_size` is the number of *players*; it must not
exceed the game's slot count (FF7 3, FF8 3, FF9 4), because a full party of players displaces the lead.

### 2.4 What "steps aside" means

A displaced canon member is **not removed from the story**: it stays an NPC in the instance, in
every scene and quest, and returns to the roster in the next battle that has room. Its sheet is
untouched. Two players may choose the same template: a template is a combat *role*, not an identity.
In the battle UI the seat shows the **player's** name; the template shows only as role (weapon class,
command set). **The protagonist's own template (Cloud, Squall, Zidane) is not offered to players:**
the protagonist stays unique, and the role of the story's lead is never a player's.

### 2.5 Canon cast: NPC-driven, per phase

* **Presence.** A phase instance runs the game's own scripts; the canon party members are script
  entities driven by the VM, not by players. In free roam the segment lead follows the leading
  player (the FF14 "trust" pattern); in scenes it does what the script says.
* **Canon sheets.** A companion's combat sheet is a **pure function of the phase**
  (level, equipment, abilities), not any player's data: `CanonSheet(phase, template)`. FF7: the
  game's own starting records and scripted growth; FF8: the same plus the game's own automatic
  junction logic `[verify: inference]`; FF9: default equipment tables (location `[verify: inference]`).
  Level for scaling games is derived from the players (FF8 scales enemies to the party average:
  `[verify: doc H-SCENE]`).
* **Companion decisions must stay replayable.** Two ways, both safe for client replay: (a) **engine-internal,
  seeded ally logic** (like the enemy side in `fmv-ff7-battle`, a pure function of state and seed, no extra
  messages), or (b) **server-issued commands recorded in the input log** as `AppliedCommand` for an actor with
  no account. Recommendation: (a) for canon companions, (b) as an escape hatch. Nothing like this exists today: a
  party actor acts only when its player's session commands it (a member that disconnects "stays in the engine as an
  uncontrolled character"), the engine's own AI covers only the enemy side, and the `policy` closure in
  `fmv-server/src/world/battle.rs` (`play_with`) is **test scaffolding** that plays test seats, not a server mechanism.

### 2.6 Appearance in each game's art style

* **No art ships and none is created at first.** Each adapter builds a **wardrobe** from the owner's own
  files: the models that game already contains, filtered to ones it can pose (idle, walk, run), ranked
  against the identity's spec, with a palette or tint applied at render time. Players pick among the
  top matches; the pick is stored as the per-game override (the field `per_game_overrides` already exists).
* **Canon cast models are reserved for NPC use** (decision D2): the wardrobe excludes the protagonists
  and the canon party. A deliberate "cosplay" opt-in can come later, and never in an instance where that
  canon NPC is present.
* **Wardrobe survey first (FF7 data is on this box).** The FF7 table of playable looks lists eight canon
  characters and nothing else (`fmv-ff7/src/playable.rs`). Whether non-canon models with idle, walk and run
  clips exist in `char.lgp` is unknown. The survey is: scan the 5,454 model-loader records across the 702
  fields for skeletons whose loader lists at least three clips of the right kind. Its result decides
  whether FF7 can honour O1 at all in the field.
* **Per game:** FF8's models are paletted TIMs `[verify: doc H-MCH]`, so CLUT-style recolour is natural; the first
  FF8 area's own NPC cast (the models in that field's `chara.one`) is a ready wardrobe `[verify: doc H-MCH]`.
  FF9's textures are Unity textures `[verify: inference]`: tint-multiply only.
* **Battle bodies are the visible conflict.** `battle.lgp` holds battle bodies for the playable canon characters and
  the enemies, with nothing like a generic ally, so a player in Tifa's slot would look like Tifa in battle. Honest
  interim options, in decision D4.
* **The wardrobe list is part of the content agreement:** its fingerprint joins the area fingerprint at
  join time, because two editions can list different models.

### 2.7 Presence in story scenes

* **Scene lock and marks.** While a scripted scene runs, players are placed on **observer marks** (walkable
  points near the scene, away from scripted paths), locked to idle or free look, and released afterwards
  (the FF14 cutscene-lock pattern). The server computes marks from the walkmesh; marks never appear in the
  game's own data.
* **FMV cannot show the player.** Both games use prerendered cutscene video (FF8's video archives are `.pak` files
  `[verify: doc F-IDX]`; FF9's packaging is unread): plan player-facing beats **before and after** each FMV, never
  inside it.
* **Two kinds of conversation.** *Talking to an NPC* is per player and client-local, and it exists for FF7 today
  (`aa3462a`: Enter or Space next to a character opens its own talk script's windows; nothing about dialogue crosses
  the network, and the player's own `ASK` answers stay with that player). *Scripted scenes* are shared and need the
  hosted VM; there the game's own choice windows are answered by the **phase owner** (the leader) and the others see
  the choice. A vote option can come later.
* **Participation** means addressable moments: a canon NPC turns to the nearest player, offers help, asks a
  question with two or three short replies from the layer (2.8). Replies set layer flags and never change a
  canon branch.

### 2.8 The dialogue layer (added content, never canon text)

* **The seam already exists (FF7, `aa3462a`).** `fmv_ff7::script::dialogue::Message` carries a
  `MessageOrigin`: `Canon { entity, dialog }` (the field's own text, shown exactly as written) or
  `Added { layer }` ("a line this project adds on top of the game: never canon", which the client's dialogue
  window draws with a different border). A window pulls from a `MessageSource`; the talk walker is one
  (`TalkSession`) and "an added content layer can be another". On the client, `ui::dialogue::Conversations`
  hands the window its source for an NPC, and `StoryVars` is the hook through which "a server-fed
  implementation that mirrors a real player's progress" reads story variables (today `StartOfGame`). The README
  already promises that whoever you play is never written into a line about Cloud or the cast. **One `Added` source
  exists already** (`094681e`): after a canon "Received ..." line whose gift opcode (`STITM`, `SMTRA`, `GOLDu`) the
  walk does not perform, the walker queues an added note that nothing was given (`EFFECT_NOTE_LAYER`, "not-performed"),
  so the canon line never misleads. What is missing: the player-facing layer sources, and moving `Message`,
  `MessageOrigin`, `Beat`, `MessageSource` out of `fmv-ff7` (and `StoryVars` out of FF7's savemap shape: bank and
  address) into a game-neutral crate, with a talk walker per game (FF8 `.jsm`, FF9 `.eb`).
* **Original text only.** Layer lines are written for this project, stored in the repository as data, per
  language, and **ship with the client** (no dialogue goes over the network; each client reads canon text from
  its own copy and layer text from the package). They never copy or edit text from the game's files. Canon
  lines are read from the owner's files untouched; canon name control codes always resolve to canon names.
  `{player}` and pronoun tokens exist only in layer lines.
* **Anchors.** A line fires at an **anchor**: `(area, anchor kind, our stable id)`, mapped per game and edition
  to a script position by an anchor table that also stores a hash of the script segment. A hash that does not
  match the owner's edition means the anchor is skipped, never mis-fired.
* **Kinds:** `Acknowledge` (a canon NPC notices the player), `Aside` (after, never inside, canon dialogue),
  `Offer` (help; may point at an optional quest), `Banter` (field or battle barks), `Farewell`.
* **State:** a small per-player, per-game flag set ("acknowledged by Barret") stored **beside the story blob,
  never in it**, so layer state cannot move a player to another instance. It is the server's: the client asks for the
  flags and the story variables of the account it plays (FF7's story variables are the savemap banks the walker reads
  through `StoryVars`; the account's story blob is where a server-fed hook would read them).
* **Rendering:** drawn in a window styled after the game's own message window from the owner's files, with
  the client's UI font as fallback, because display names may use scripts the game font lacks (names
  allow CJK and others: `fmv-core/src/names.rs`). Authenticity versus Unicode is part of decision D8.
* **Pronouns:** the identity may carry a pronoun set so authors can write natural second- and third-person
  lines; lines that need none stay neutral.

### 2.9 Abilities adapt to each game

The original's capabilities in a game are defined by **that game's own system and the slot template**, and
nothing crosses games. FF7: the slot's growth curve, weapon class and limit set, plus materia the account
owns (materia is not modelled per account yet: today's spells come from the starting record plus a starter
grimoire). FF8: the slot's stat base and GF compatibility table, junctions the account sets, magic it draws, GFs
it earns in its own phase. FF9: the slot's learnable-ability list (equipment teaches), Magic Stone capacity
and the slot's Trance set. Details per game are in sections 5 and 6.

### 2.10 The FF7 retrofit (today's code to the model)

Concrete facts from the code at `094681e`:

* `fmv_ff7::playable::DEFAULT_APPEARANCE` is `ff7/cloud`: today's default really is "everyone is Cloud".
* The look picker offers Cloud, Tifa, Barret and Aerith in the character menu (README; `fmv-client/src/ui/*`),
  and the table behind it has eight canon looks.
* `ProgressSeed.character = character_for_look(...)` runs **at every login**, and `progress::resolve` sets
  `stored.character = seed.character`: changing the look changes who the account fights as.
* `BattleMember.character` is already an engine-neutral "which canon template" id, and `Combatant::from_character`
  already builds a fighter from a template, a level and equipment. **The slot model needs no engine change.**

| Step | Change | Needs the script VM? |
|---|---|---|
| F1 | A stored, explicit **slot** per account and game (registration and the character menu get a "role" choice). `character_for_look` becomes `slot_for_account`. **Migration:** snapshot the current look-derived character as the slot once, then decouple the two. | no |
| F2 | Stop defaulting everyone to Cloud; reserve the canon looks for NPC use; run the **wardrobe survey** (2.6); interim default look comes from its result, canon looks stay selectable behind a clearly labelled flag until it lands | no |
| F3 | Battle bodies: interim per decision D4 | no |
| F4 | Seating algorithm (2.3) replaces "i-th party member is `ActorId(i)`"; ally logic for canon companions (2.5) | no |
| F5a | **The layer on talk conversations** (2.8): `Added` sources wrapped around the canon `TalkSession` (acknowledge, aside and offer lines after an NPC's own text), flags kept server-side, `StoryVars` fed from the account's story | **no** (the seam exists) |
| F5b | Canon protagonist as a script-driven NPC in instances; observer marks; the layer inside shared scenes (2.7) | **yes** |

F1 to F4 remove the "thirty Clouds" problem at the identity, slot and wardrobe level without a VM, and F5a gives the
player a first visible acknowledgement in FF7 on the dialogue code that already exists. F5b is the story model
proper and waits for the VM track.

### 2.11 What this section needs that does not exist yet

| Needs | Status |
|---|---|
| per-game script VM hosted by the server (FF7 field script, FF8 `.jsm`, FF9 `.eb`), with party-affecting opcodes intercepted | **does not exist for any game.** FF7 has script analysis and, since `aa3462a`, a read-only talk walker (`script::dialogue`: message, branch and variable opcodes only; MAPJUMP, BATTLE, MINIGAME and GAMEOVER just end the walk, gift opcodes are not performed); the README still lists world-changing field scripts as not done. The effects the walker declines to perform are the first draft of the intercept list for a hosted VM, where the server owns the inventory, the party and the battle |
| a game-neutral message and story-variable layer (`Message`, `MessageSource`, a `StoryVars` that is not FF7's savemap) | the types exist, in `fmv-ff7`; promotion is part of R9 |
| segment data (`SegmentCast` per phase) | curated per game; source is the VM's story variables |
| canon sheets per phase | pure functions of owned data; per game |
| wardrobe, scene staging, layer runtime | per game adapter plus shared runtime |

---

## 3. What is shared and what is per game today

Audit of `main` at `2fea4b5`, re-checked at `094681e` (read, not run).

| Layer | Crate | Verdict | Evidence |
|---|---|---|---|
| ids, provenance, phase, identity types | `fmv-core` | **shared**, small FF7-isms | `GameId` already has `Ff8`, `Ff9`; `PhaseKey(Vec<u8>)` is opaque. FF7-isms: `MAX_INVENTORY_STACKS = 320` (its comment ties it to FF7's slot count), `AnimState { Idle, Walk, Run }`, `InventoryEntry { item: u16, count: u16 }` |
| adapter trait | `fmv-adapter-api` | **shared but only half there** | `GameAdapter { game, list_areas, load_area }` plus `AreaRuntime` (walkmesh, exits, NPCs, encounters). The plan's section 2 also lists `identify`, `avatar_appearances`, `phase_key`, `default_progression`, `battle_rules`, `render_assets`: **none exist in code**. `Exit.arrival_facing` and `arrival_hint` already anticipate FF8-style gateways |
| wire protocol | `fmv-protocol` | **mixed** | handshake, area, party messages are generic. `Cargo.toml` depends on `fmv-ff7-battle`; `battle.rs` re-exports `ActionRef, ActorId, BattleEvent, Command, Outcome` from it; `BattleRules::build` returns the concrete FF7 `BattleState`; `ServerMsg::{EncounterStart, BattleEvent, BattleEnd, BattleRejected, CharacterProgress, Inventory}` are FF7-typed; `Hello` carries no game or schema version; `NPC_REF_CHARS = 16` |
| persistence | `fmv-persist` | **mostly shared** | `Progression` is an opaque blob (256 KiB cap), `AvatarProfile` is generic; `CharacterProgress` (32 KiB cap) says game-agnostic but its fields (`ap`, `limit`, one `u8` character) are FF7-shaped |
| parties, phasing | `fmv-party` | **shared** | `PartyConfig` is per game ("one manager per game"); `DEFAULT_MAX_PARTY_SIZE = 3` is named FF7's |
| server | `fmv-server` | **mixed, single game** | hosts "the areas of one `GameAdapter`"; `world/battle.rs` calls the concrete `BattleState`; `accounts::canonical_appearance` and `progress::character_for_look` have FF7 arms and a `_ => None`; `ff7.rs` wraps the adapter; `main.rs` selects `GameKind` |
| client | `fmv-client` | **FF7 throughout** | 49 files import `fmv_ff7*` (37 excluding tests and screenshot code); about 27,400 lines (tests and screenshot code included) in `battle/`, `avatar/`, `ui/dialogue*`, `backdrop.rs`, `ff7.rs`; the new dialogue window is built on a game-neutral pair of traits (`Conversations`, `MessageSource`), but their types live in `fmv-ff7` and the one implementation is FF7's; `ff7.rs` fills process-wide statics for the window to find (`avatar::source::remember`, `battle::scene::model::remember_battle_lgp`); the battle menu and overlay are FF7's (Attack/Magic/Item/Defend/Limit/Escape, blue windows); 10 `ui/` files name FF7 or its looks |
| library, import | `fmv-library`, `fmv-import` | **mixed** | blob store, manifest, `Identification`, fingerprints are generic; `fmv_library::ff7` resolver, `scan.rs` FF7 `WANTED` list, `serial.rs` FF7 serial table (`EXPECTED_DISCS = 3`), `steam.rs` FF7 appids; per-file cap `DEFAULT_MAX_FILE_SIZE = 4 GiB`; imports **copy** into `library/blobs/<blake3>` |
| game crates | `fmv-ff7`, `fmv-ff7-battle` | **per game** | decoders, area runtime, models, battle engine |
| second adapter | `fmv-fixture` | **proof of plug-in** | a synthetic adapter shows what a second `GameAdapter` needs today |

What is **already right**: `GameId::{Ff8, Ff9}`; opaque `PhaseKey` and `Progression`; per-game party config;
`Identification { verified: false }`; the replay-verified battle design (a pure engine of owned data plus seed
plus inputs, server authoritative, client replay, golden JSON tests); the "nothing waits on an animation" rule.

---

## 4. Refactors so FF8 and FF9 plug in without FF7 assumptions

Each keeps FF7 green. **FF7 regression gates:** `scripts/smoke.sh`, `scripts/battle-smoke.sh` (modes
`--fixture --random --progress --commands`), `scripts/travel-smoke.sh`, the golden JSON tests in
`fmv-protocol`, and the `#[ignore]` real-data tests (`FMV_FF7_DATA`).

| # | Refactor | What changes | Needed for first FF8 area? | Size |
|---|---|---|---|---|
| R1 | **Importer and resolver generalisation** | `GameScanSpec` per game (wanted files with milestone tags, Steam appids, language layout) replaces the hard-coded FF7 `WANTED`; one `fmv_library::resolve(game, ...)`; server/client `--ff7-*` flags become `--content`; launcher gets `--game` and per-game first-run import | **yes (minimal: FF8 spec)** | M |
| R2 | **Client presentation seam** | a `GamePresentation` trait (field background and camera, model and animation loading) and shared render-model types (the FF7 `FieldModel`/`FieldAnimation` shapes promoted to a game-agnostic crate); retire the process-wide statics; FF7 moves behind it | **yes (lite: field only)** | L |
| R3 | **Decoder toolbox** | shared LZSS/LZS if FF8's matches FF7's (the FF8 page links FF7's LZS page `[verify: doc F-PC]`; the byte stream is `[verify: inference]`); archive traits; later a Unity container reader | yes (LZS, archives) | S |
| R4 | **Adapter API v2** | `GameAdapter` gains optional, defaulted capabilities: `appearances()` (wardrobe), `slot_templates()`, `progression()`, `battle()`; FF7 implements them, FF8 starts with none | no | M |
| R5 | **Battle envelope and `BattleSession`** | see 4.1; protocol v8 | no (needed for FF8 battle) | L |
| R6 | **Multi-game server** | adapter registry keyed by `GameId`, `World` partitioned per game, one `PartyManager` per game, `Hello` carries the game and per-game schema versions | no (needed for FF7 and FF8 on one server) | L |
| R7 | **Identity, slot, wardrobe** | section 2.10 F1 to F4: persist, server, character menu | no | L |
| R8 | **Per-game sheets and transactions** | a generic character-sheet envelope (blob, schema version, small summary); per-game inventory limits; a **two-account transfer journal** for card stakes | no | M |
| R9 | **Story runtime** | promote the message and story-variable types out of `fmv-ff7`; a talk walker per game; then the hosted script VM, instance runtime, staging and the layer inside scenes (section 2) | no | XL, per game |

Sizes are rough: S = a checkpoint, M = a few, L = a handful, XL = a track of its own.

**R4 in one sketch** (a proposal; names are placeholders). Everything new is defaulted, so FF7 keeps compiling and FF8
starts with areas only:

```text
trait GameAdapter {                                      // today
    fn game(&self) -> GameId;
    fn list_areas(&self) -> Vec<AreaInfo>;
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError>;
    // R4: all defaulted
    fn wardrobe(&self) -> Vec<WardrobeEntry>                   { vec![] }   // appearances the owner's files can draw (2.6)
    fn slot_templates(&self) -> Vec<SlotTemplate>              { vec![] }   // canon combat roles a player may take (2.2)
    fn segment_cast(&self, phase: &PhaseKey) -> Option<SegmentCast> { None } // canon party, lead, slots, policy (2.3)
    fn progression(&self) -> Option<&dyn ProgressionRules>     { None }     // sheet schema, caps, starting sheet, reward rule
    fn battle(&self) -> Option<&dyn BattleRules>               { None }     // R5: builds a BattleSession
    fn tables(&self) -> Option<&dyn TableRules>                { None }     // card games (5.5, 6.5)
}
```

The identification half of the plan's section 2 (`identify`, `phase_key`) stays outside the trait: identification belongs to
the importer (R1) and the server derives phase keys generically from the progression blob (`accounts::phase_key`).

### 4.1 The battle envelope (R5)

Today the wire carries the FF7 engine's own serde types. Adding FF8 and FF9 as enum variants would couple
the protocol to every engine and bump the version for every game's change. Instead:

* **Opaque to `fmv-protocol` only.** The wire keeps game-neutral fields (`battle_id`, `game`, `rules_version`,
  `data_fingerprint`, `seed`, participants as `account -> actor`, `tick`) and carries
  `setup`, `input`, `events` and `rewards` as **game-owned, versioned blobs** (`(game, battle_schema_version)`),
  with golden JSON tests living in each game's battle crate.
* **Typed to the server.** The server still needs typed answers for validation and settlement. The seam is the
  one that already exists, `BattleRules::build`, but today it returns the concrete FF7 `BattleState`. It should
  return a `Box<dyn BattleSession>`:

```text
trait BattleSession: Send {
    fn step(&mut self, ms: u64) -> Vec<EventBlob>;          // advance; events as opaque blobs plus a neutral digest
    fn ready_actors(&self) -> Vec<ActorId>;                 // who may be commanded now (gauge, turn)
    fn submit(&mut self, actor: ActorId, input: &InputBlob) -> Result<Vec<EventBlob>, Refusal>;
    fn outcome(&self) -> Option<Outcome>;                   // Victory | Defeat | Escaped | game-defined extras
    fn rewards(&self) -> RewardEnvelope;                    // per account: neutral summary plus game blob
    fn is_ai(&self, actor: ActorId) -> bool;                // companion, see 2.5
}
```

* **Replay verification stays per game.** A client builds its own `BattleSession` from its own owned data and
  the same setup blob and compares events; "BATTLE DIVERGENCE" and the red band survive unchanged.
* **Setup grows for FF8/FF9.** FF7 rebuilds a fighter from `(template, level, weapon, armor, limit)`. FF8 and FF9
  stats depend on the player's own build (junctions and GFs; equipment and learned abilities), so each member's
  setup carries a **build snapshot** of ids and numbers (never asset bytes), composed by the server from the
  stored sheet; a client never sends its own.
* **Version negotiation.** `Hello` (or a new join message) must carry the chosen game and per-game schema
  versions; the handshake has neither today. A bridge keeps the FF7 v7 messages until FF7 migrates.
* **Prove the seam before the real game.** Build a second **synthetic** engine in `fmv-fixture` (labelled
  FIXTURE, toy rules with a junction-like and a trance-like mechanic) and run it through server, client and
  `battle-smoke`. If the envelope carries two engines it will carry FF8.
* **Smaller wire items:** raise `NPC_REF_CHARS` (16 is short for likely FF9 asset names); a per-game inventory
  cap instead of the FF7-derived 320; extra `AnimState` values or an escape hatch for animations FF8/FF9 add.

### 4.2 Client shells: what is shared and what a game brings

The client is where FF7 is most deeply wired in (section 3), and "different feel" lives here: menus, fonts, windows,
the battle overlay. Split it along the same line as the server:

| Shared by every game (not audited line by line) | Per game (a `GameShell` behind R2) |
|---|---|
| window, input, gamepad, device mode, network loop, token store, login, the character menu, the party mirror and panel, the UI toolkit (`ui/widgets`, `ui/text`, `ui/palette`) | field renderer (the tile-depth pass, camera, walkmesh overlay), model and animation loaders, **battle overlay and command menu** (FF7: blue windows and Attack/Magic/Item/Defend/Limit/Escape; FF8: Draw, Cast, GF, Junction screens; FF9: skills, Trance, a four-member panel), **out-of-battle menus** (FF8 Junction and card screens, FF9 equipment-ability and Magic Stone screens), fonts and window skins from the owner's files |

The character menu is the one shared screen that changes with this design: it creates the **identity** and the
wardrobe pick (2.2, 2.6) and, per game, the **slot** choice, instead of FF7's single "look" row. Above both sits a
**game picker** (the launcher entry script and a client screen) that opens one game's adapter per session: the account
is the same in every game, the party and the progression are not.

---

## 5. FF8

### 5.1 What makes FF8 feel like FF8, and what it asks of the engine

| Pillar | Fact (mechanics are public knowledge; format claims are tagged elsewhere) | Implication |
|---|---|---|
| Junction | No MP pool. Characters attach Guardian Forces and **junction magic to stats**; stock is up to 100 per spell and 32 distinct spells per character; spells come from drawing, Draw Points, or refining cards and items `[doc WP8]` | a character's combat stats are a **function of its build** (junctions, GFs), so the battle setup must carry a build snapshot (4.1); a Junction screen is a menu outside battle |
| Draw | Magic is drawn from enemies or Draw Points mid-fight | a `Draw` command; per-player draw-point state |
| GF summons | Summoning takes time (the summoner's gauge becomes a summon timer); GFs earn AP and learn abilities `[doc WP8]` | a per-account GF roster in the sheet; a per-seat summon timer in the engine; the summon cinematic is presentation only |
| Limit breaks | Usually at low HP, some with timing-based inputs; Aura raises the odds `[doc WP8]` | per seat; the roll uses the engine's seeded RNG; timing input is a **bounded parameter** on the command, not a client-side result the server trusts blindly |
| Level scaling | Enemy levels scale with the party's average level; some bosses are capped `[doc WP8]`; the encounter record's level byte encodes "party level, plus or minus a fifth" and caps `[doc H-SCENE]` | in a shared battle, **team level = the mean of the players' levels**; AI companions copy it |
| EXP | A flat 1000 EXP per level `[doc WP8]` | trivial curve; the sheet keeps EXP |
| Triple Triad | A collectible card minigame; cards refine into magic `[doc WP8]` | a multiplayer table feature (5.5) |
| Three characters | Three fight at once (the Wikipedia battle caption; common knowledge) | three slots (2.3) |

### 5.2 The data we expect on the PC releases (summary; claim-by-claim in Appendix 2a)

* **Three PC builds, treated as separate editions** (editions are never mixed, README rule 6): the original
  2000 PC release (not on Steam), the 2013 Steam port (Steam app 39150 `[verify: snippet S-APP]`) and the 2019
  Remaster (Steam app 1026680 `[verify: snippet S-APP]`).
* **2000 and 2013:** data in `<install>\Data\` as sets of **three files**, `.fs` (the data), `.fl` (a CRLF text list of
  absolute paths) and `.fi` (12 bytes per file: uncompressed size, offset in the `.fs`, compression flag 1 = LZS,
  0 = none) `[verify: doc H-PC]`. The set names are not given by those pages; a forum post mentions
  `battle.fl/.fi/.fs` under `Data\lang-en\` `[verify: snippet S-BAT]`. The LZS the page links to is FF7's
  `[verify: doc F-PC]`; whether the byte stream is FF7's, including a leading length word, is `[verify: inference]`.
* **2019 Remaster:** a different container: `main.zzz` in the game root, plus `other.zzz` (music as `.ogg`, among
  other things) `[verify: snippet Z1]`. The entry layout of ZZZ is unconfirmed (tools exist: zzzDeArchive, qt-zzz
  `[verify: doc Z2]`; any byte-level description I recall is `[verify: inference]`). Backgrounds were reported
  unchanged from 2013 and the main cast's models upgraded `[verify: snippet S-DIFF]`.
* **Per field** (2000/2013 builds): background image `.mim` and tile list `.map` (16-byte records of 16 by 16
  tiles, each with a depth `Z` that orders it against models), walkmesh `.id`, camera `.ca`, gateways and door
  triggers `.inf`, script `.jsm`, dialogue `.msd`, plus particle, mask and sound side files; NPC models and their
  animations in `chara.one`; the main characters' models `d000` to `d075` in `main_chr.fs`
  `[verify: doc H-MAP, H-WALK, H-CA, H-INF, H-MCH]`.
* **Battle:** `scene.out` is 1024 encounters of 128 bytes with no header; each lists a stage id, up to eight monster
  slots (at most four enabled), positions, levels and flag bits (no escape, surprise attack, back attack, skip
  victory screens) `[verify: doc H-SCENE]`. Monster files `c0mXXX.dat` have a section table (11 sections; stats in
  section 7, AI scripts in section 8) `[verify: doc H-MON]`. `kernel.bin` has 31 sections after the header: battle
  commands, magic, junctionable GFs, enemy attacks, weapons, Renzokuken finishers, characters, items, the
  ability families, limit-break data for the temporary characters, Irvine, Zell and Rinoa, and more
  `[verify: doc H-KERN]`.
* **World map:** a separate family (`wmx.obj`, `texl.obj`, `wmset*.obj`, `chara.one`, `music*.obj`)
  `[verify: doc F-IDX]`; deferred (5.6).

### 5.3 Adapter shape (`fmv-ff8`)

* **Walking.** `.id` is a triangle array and an edge-neighbour ("access") array with one `-1` per wall edge
  `[verify: doc H-WALK]`; the FF7 walk code (`fmv-ff7/src/walk.rs`: validation against the walkmesh and its edge-access table, which the
  server calls once per movement sub-step) is the model, and its tests port over. That FF7 and FF8 walkmeshes are the same family is claimed by one index page only
  `[verify: snippet F-IDX]`.
* **Exits.** `.inf` has 12 gateway records of 32 bytes (an exit segment, arrival x/y, **an arrival triangle in the
  target field's walkmesh, a target field id** where `0x7FFF` is empty and values below `0x48` mean "world map",
  and an arrival facing byte) and 12 door triggers `[verify: doc H-INF]`. These map straight onto
  `fmv_adapter_api::Exit` (`arrival_hint` = the triangle, `arrival_facing` = `Some(..)`; both fields exist for this).
  The target id is "the line index in `maplist`" `[verify: doc H-INF]`; where `maplist` lives is
  `[verify: inference]`. World-map targets stay **closed** until a world area type exists, like script exits in FF7.
* **Camera and background.** `.ca` is a per-camera record (three axis rows over 4096, a translation, a zoom) with
  the documented projection `screen = (R*P + T).xy * zoom / z`, said to align with `bghall_1`'s walkmesh
  `[verify: doc H-CA]`. The tile list is drawn through a 4096-slot ordering table with models, slot = tile `Z`
  `[verify: doc H-MAP]`: the same "tile by tile with a depth" idea the FF7 renderer uses, so a **shared tile-depth
  renderer** in the presentation seam (R2) serves both.
* **Not in the first cut:** NPC placement and conditional content (needs `.jsm` analysis, as FF7 needed script
  analysis), random encounters (decided by script `[verify: inference]`), world-map exits, music, FMV.
* **Fingerprint:** bind the provenance and the field's own file hashes, the way `area_fingerprint` does for FF7.

### 5.4 Battle engine plan (`fmv-ff8-battle`), in stages

Same design as FF7: a pure engine of owned data, seed and inputs; server-authoritative; clients replay; golden tests.

| Stage | Adds | Needs (all `[verify]` until read) |
|---|---|---|
| A | encounters, level scaling, monster stats, Attack, Defend, Item, ATB | `scene.out`, `c0mXXX.dat` section 7, `kernel.bin` characters, battle commands, items |
| B | junction stat computation, Magic from stock, **Draw** | `kernel.bin` magic, junction abilities; stock rules |
| C | GF summons, GF abilities, compatibility | `kernel.bin` GF sections; initial compatibility data (probably `init.out`: the page exists in the wiki index `[verify: doc H-IDX]`, contents unread) |
| D | limit breaks, statuses, the monsters' AI scripts | `kernel.bin` limit sections; `c0mXXX.dat` section 8; the wiki documents an enemy-AI VM `[verify: doc H-IDX]` |

FF7's engine runs the enemies' own AI scripts since protocol v11 (`fmv_ff7_battle::ai`, a bounded interpreter with
the plain pick among their attacks as the per-turn fallback); FF8 starts with the plain pick and adds its AI VM in
stage D. The documented formulas (`battle/formulas` in the wiki index) are unread: reading them
is the first task of stage A, with approximations labelled as the FF7 engine labels them.

### 5.5 Triple Triad as a multiplayer feature

* **Rules (public):** 3 by 3 board, five cards each, ranks 1 to 10 on four sides, elements; special rules Open,
  Same, Same Wall, Plus, Elemental, Random, Sudden Death (Combo follows Same and Plus); trade rules One,
  Difference, Direct, All `[verify: snippet S-TT]`. The wiki lists the rule bitmask (Open `0x1`, Same `0x2`, Plus
  `0x4`, Random `0x8`, Sudden Death `0x10`, Wall `0x40`, Elemental `0x80`), the trade enum, 110 cards (ids 0 to 109,
  rare 77 to 109) and element flags `[verify: doc H-CARD]`.
* **Where the card stats live is not documented** on the pages I read: not on the card-list page, not in the
  exe-table page. The PS1 page says Triple Triad *graphics* sit in the exe on PC `[verify: doc H-PS1]`, and a
  forum poster believes card stats and art were in the exe in 2000/2013 and are a separate file in the Remaster
  `[verify: snippet Z1]`. So the **executable may be an owned-file input** (section 8).
* **Design.** A game-agnostic `Table` in core (challenge, accept, seat, rules agreed, turn timer, settle) with a
  per-game rules crate (`fmv-ff8-cards`). Seeded RNG covers the coin flip, Random hands and Elemental tiles; every
  move is an input-log entry, so both clients replay and verify like a battle; the **card-table fingerprint** is part
  of the table handshake.
* **PvP rules:** the challenger proposes rules and the defender accepts or declines; the single-player "regional
  rule spread" has no place between players (it can come back for NPC matches).
* **Stakes:** friendly matches (no trade) first. Stakes need explicit mutual consent and the **transfer journal**
  (R8: a crash between two account writes must not duplicate or lose a card). Rare cards are unique in a single save
  (their locations live in it `[verify: doc H-CARD]`); in a many-account world they cannot be: per-account copies
  excluded from stakes is the recommendation (decision D7).
* **It can start early:** the rules need no owned data (test with a synthetic FIXTURE card table); only the real
  table does.

### 5.6 World map

A different area type (3D terrain streaming, vehicles; files above). FF7's world map is not done either. Gateways
with a world-map target stay closed. A world adapter is a later track and its own design.

### 5.7 The first area

**`bghall_1`**, which I recall as the Balamb Garden main hall `[verify: inference]`, as the first two-player FF8
area: it is the field the community camera page checks its projection against (the page says the walkmesh lines up
with the floor, staircase and bridge) `[verify: doc H-CA]`, it is early and social, and it needs no script to be
walkable. The choice stands only if the owner's edition has it and it is what I recall; the probe (section 8)
answers the first, a look at the decoded field the second.

---

## 6. FF9

### 6.1 What makes FF9 feel like FF9

| Pillar | Fact | Implication |
|---|---|---|
| Active Time Battle | Agility sets how quickly and in what order everyone acts `[doc WP9]`; four party members fight (well known; the Wikipedia text does not give a number) `[verify: inference]` | four slots; the ATB is the engine's |
| Trance | Triggers for a short time when the character takes a set amount of damage; transforms signature commands (Vivi's Black Magic becomes Double Black) `[doc WP9]`; the gauge fills from damage this character takes, not damage to others, full at 255; the temporary characters (Marcus, Blank, Cinna, Beatrix) have none `[snippet S-TR9]` | per-seat gauge and state in the engine; the **trance set belongs to the slot template** |
| Equipment-taught abilities | Gear grants abilities while worn; enough AP makes one permanent; action abilities cost MP, support abilities are automatic; Magic Stones cap how many are equipped `[doc WP9]` | the sheet holds AP per ability **per template** (Memoria's player record keeps an AP array sized by the character's "menu type" `[verify: snippet M-PLAY]`); equipment and AP are the account's own |
| Character roles | Each character has exclusive commands (Zidane steals, Garnet and Eiko summon eidolons, Vivi casts black magic) `[doc WP9]` | the slot template gives the command set |
| Active Time Events | Cutscene views of other places; sometimes control of two teams when the party splits `[doc WP9]` | in a shared game: the leader picks; others watch (2.7) |
| Tetra Master | 4 by 4 board, up to six blocked squares, five cards each, four hexadecimal stats and arrows; an arrow at an unguarded card captures it, at a guarded one the two fight `[snippet S-TM]` | a multiplayer table feature (6.5) |
| Moogles, Mognet, synthesis | save and shop, an in-game mail side quest `[doc WP9]` | per player |

### 6.2 The Steam packaging, and why it is the biggest risk

* **A Unity build.** Steam app 377840 `[verify: doc M-README]`. Data is not in an FF7-style archive family. The
  Memoria wiki gives the roles `[verify: doc M-FOLD]`:

| Archive | Holds |
|---|---|
| `StreamingAssets/p0data1X.bin` (nine files) | field maps |
| `p0data2.bin` | battle data |
| `p0data3.bin` | world map |
| `p0data4.bin` / `p0data5.bin` | 3D models / model animations |
| `p0data61.bin`, `p0data62.bin`, `p0data63.bin` | music / sounds / effect sounds |
| `p0data7.bin` | event scripts |
| `FF9_Data/resources.assets` | nearly all text (per language, `.mes`), plus other assets |
| `FF9_Data/sharedassets2.assets` | UI atlases and textures, including every Tetra Master card image |

* **Inside the field archives:** `Assets/Resources/FieldMaps/<field id>/` with `atlas.png` (every background tile on
  one sheet), `<id>.bgs.bytes` (the tileset that arranges it into layered, animated scenery), `<id>.bgi.bytes` (the
  walkmesh), effect and texture side files `[verify: doc M-FOLD]`. A community toolkit claims to reproduce
  `.bgs`, `.bgi` and `.eb` (event script) files byte for byte and to copy about 674 real fields
  `[verify: snippet S-MK]`.
* **Battle:** `BattleMap/BattleScene/EVT_Battle_*/dbfile0000.raw16.bytes` holds monster stats, skills and
  formations; `*.raw17.bytes` the scripted attacks and camera `[verify: doc M-FOLD]`.
* **The container itself is unconfirmed.** That they are Unity serialized archives is plausible: a Steam guide opens
  `p0data*` in Hades Workshop's "Unity Assets Viewer" `[verify: snippet S-HW]`. UnityFS versus another layout,
  block compression, and which asset types appear (`Texture2D`, `TextAsset`, skinned meshes, animation clips) are
  `[verify: inference]`. **A new decoder crate (`fmv-unity`) is needed**: a bounded, panic-free reader written to
  `fmv-ff7`'s rules. The local cargo cache on this box already holds `lz4_flex`, `lzma-rust2`, `bzip2`, `ruzstd` and
  `zip` (`~/.cargo/registry/cache`); whether they resolve under `net.offline = true` is `[verify: inference]`, and no
  Unity-format parser is cached.
* **Gameplay tables may not be data files.** Memoria "externalises" items, weapons, armor, active and support
  abilities, ability learning per character, character parameters, base stats, leveling, default equipment,
  commands, statuses, shops, synthesis and **Tetra Master base stats** into CSV under `StreamingAssets/Data`
  `[verify: doc M-DB]`. That page does not say where the *vanilla* game keeps them, and Memoria's own player code
  reads them from in-memory dictionaries `[verify: snippet M-PLAY]`. The likeliest home is the game's compiled
  assembly `[verify: inference]`. Three routes follow, decision D9: **(1)** read the tables out of the owner's
  vanilla assembly (a .NET metadata reader; only if they are there), **(2)** read Memoria's CSV from a
  Memoria-patched install (a `modded` provenance class, never mixed with vanilla), **(3)** author tables from public
  knowledge as labelled approximations (last resort). The probe in section 8 decides which routes exist.
* **Everything else is per language in the same files**, so FF9's library set has no "one language per set"
  problem; language is a text lookup.

### 6.3 Adapter shape (`fmv-ff9`)

* A field is background atlas plus tileset (`.bgs`) plus walkmesh (`.bgi`) `[verify: doc M-FOLD]`. A gateway table
  like FF8's `.inf` is not in anything I read, and field exits are probably script-driven
  `[verify: inference]`: the **first FF9 area is free roam with no exits**, like FF7's first milestone.
* Camera lives in the scene data (probably the `.bgs`) `[verify: inference]`. Models are Unity skinned meshes and clips
  from the model and animation archives; players start as **markers**, as FF7 did, and get models once the Unity reader
  decodes meshes.
* A shared "Unity" decoder is a track of its own (N1 in 9.4) and can start from the probe.

### 6.4 Battle engine plan (`fmv-ff9-battle`), in stages

| Stage | Adds | Needs (`[verify]` until read) |
|---|---|---|
| A | four seats, ATB, Attack, Defend, Item, monsters from `dbfile0000.raw16.bytes`, formations | the raw16 layout; the tables route (decision D9) for characters and items |
| B | equipment, AP, learned abilities, Magic Stones, action and support abilities | ability, item and leveling tables |
| C | **Trance** (gauge, state, per-template command set), eidolons | trance data, command tables |
| D | statuses, the scripted attack sequences (`.raw17`), enemy AI | battle scripts |

### 6.5 Tetra Master

A table feature like Triple Triad (5.5): 4 by 4, up to six blocked squares, five cards each `[verify: snippet S-TM]`,
battles resolved by seeded rolls from hexadecimal stats (so the engine is deterministic and replayable), the card
art in `sharedassets2.assets` `[verify: doc M-FOLD]`, base stats in a Memoria CSV `[verify: doc M-DB]` and a vanilla home
that is unknown `[verify: inference]`. The same `Table`, the same transfer journal, the same stakes policy.

### 6.6 Party splits and Active Time Events

FF9 splits its party and runs stretches led by Steiner, Vivi or Dagger. That is exactly why seating keys off the
**segment's** canon party (2.3), and why "what originals do in swapped segments" is decision D6. The story variable that
tells the segment apart is the "scenario counter" (a Memoria knowledge-base page of that name exists
`[verify: doc M-WIKI]`, contents unread); the FF8 equivalent lives in the field-script variables
(`[verify: doc H-IDX]`, a `Variables` page exists, contents unread).

---

## 7. Multiplayer that keeps each game's feel

### 7.1 One shared battle, per game

| Aspect | FF7 (today) | FF8 | FF9 |
|---|---|---|---|
| Commands | each player commands only its own seat when the gauge is full | the same; each seat has its own Magic/Draw/GF/Item menus | the same; plus the seat's Trance command set |
| What is per account | level, EXP, AP, gil, inventory, limit gauge | stock, junctions, GFs and their AP, level, items | equipment, learned abilities and AP, level, items |
| Setup on the wire | `(template, level, weapon, armor, limit)` | a **build snapshot**: junction map, GFs, equipment (ids and numbers) | a **build snapshot**: equipment, learned abilities |
| Scaling | formation levels fixed | enemies scale to the mean of the players' levels | per scene `[verify: inference]` |
| Special input | none | timing inputs on summons and some limits: bounded command parameters | none significant |
| Companions | canon members as AI (2.5) | same | same |
| Minigame | none | Triple Triad table | Tetra Master table |
| Social hub | any area | Balamb Garden hall (the first area) | chosen after the probe |

### 7.2 Rules that apply to all three

* **Presentation never blocks the clock** (the existing FF7 rule: "nothing waits on an animation"). FF8 and FF9 play
  long cinematics (GF summons, limit breaks, eidolons) that the originals use to pause the fight. Faithful pausing is
  slow when three players each summon; overlaying them while the clock runs is fast but less faithful. This is
  decision D10; the recommendation keeps the clock running and gives the casting seat a short, explicit engine-side
  "beat".
* **Rewards are per account**, computed by the engine, never pooled; the FF7 approximations (gil split like EXP, items
  dealt like cards) are labelled and may differ per game.
* **A disconnected player's seat is taken over by the companion logic** until it returns (today it stands idle: "stays
  in the engine as an uncontrolled character").
* **The server composes every setup** from its stored sheets; a client never sends its own build.
* **Parties exist per game** (one `PartyManager` per game); friends, chat and the identity are cross-game, a party is not.

### 7.3 Two walkthroughs

**A shared FF8 battle.** A (Zell slot) and B (Selphie slot) stand in the same field with Squall as the segment lead.
An encounter rolls on the server's seeded RNG. The roster is Squall (AI), A, B; **team level is the mean of A's and
B's levels** and Squall's canon sheet copies it. Every player has their own menus: A draws Fire from an enemy and it
lands in **A's** stock, never B's; B starts a GF summon, B's seat shows a summon timer while A and Squall keep acting,
and the cinematic is an overlay (decision D10). When A is at low HP the engine, on its seeded RNG, offers A's limit (the Zell
template's Duel); A's timing inputs travel as bounded parameters on the command and the server accepts them inside a
window. If B's connection drops, the companion logic plays B's seat until it returns. The battle ends in Victory,
rewards are computed per account by the engine, and each client replays every step from its own files: the red band
stays green.

**An FF9 party.** A (Freya slot), B (Vivi slot) and C (Steiner slot) are in a Zidane segment: the roster is Zidane (AI),
A, B, C. When a fourth friend D joins (the party cap is four) the lead yields: Zidane stays in the story and leaves the
fight. Each seat has its own ATB gauge, its own equipment (the abilities A's gear teaches, and the AP A earns, are A's)
and its own Trance gauge, which fills when **that** character is hit; B's Trance (the Vivi template's) turns Black Magic
into Double Black for B alone. When the story splits the party the leader's team is followed, the others watch, and
`story_advance_allowed` is false for them for that stretch (the existing phasing rule).

---

## 8. Owned-media gates and read-only import

### 8.1 Rules reused unchanged from FF7

Bring your own copy; originals opened read-only; nothing in the repository, a release or on the wire; modded installs
tagged `modded` and never mixed; editions, regions and revisions never mixed; the area fingerprint binds the
provenance; `Identification { verified: false }` until a known-hash table exists (README "Legal and data rules";
`fmv-library`, `fmv-import`).

### 8.2 The gate, the same for both games

1. **The owner supplies the install root plus the Steam `appmanifest_<appid>.acf` next to it** (the importer already
   reads it: app id, install folder, build id) and says which edition and language is meant.
2. **A read-only probe** (`fmv-import probe <game> <dir>`, part of R1) enumerates and hashes: file names, sizes,
   modification times, blake3 of each candidate file, the appmanifest facts, and whether known mods are present. It
   does not decode and writes no game bytes. The report is plain text and safe to paste.
3. **We compare** the report with the expected candidates below and with the anchors in Appendices 2 and 3. Surprises
   change this document, not the code's assumptions.
4. **Import** into a library set with provenance:
   `{ game, platform: Pc, edition, region: unknown, language, revision: "build <id>", sources: Install{path}, modded }`
   and `Identification { verified: false, method: "steam-appmanifest + file-presence" }`. Edition label text is pinned the
   day the first set is imported, because it feeds instance compatibility and fingerprints (FF7 precedent:
   `Steam Edition (appid 3837340)`).

### 8.3 FF8: what the owner supplies

The owner supplies **one** FF8 edition's install root (Steam 2013, app 39150, or Steam Remastered, app 1026680; both ids
`[verify: snippet S-APP]`) and its appmanifest. Expected candidates:

| Candidate file(s) | Role | Needed for | Tag |
|---|---|---|---|
| `Data/lang-<xx>/field.fi`, `.fl`, `.fs` (or `Data/field.*`) | field archive: backgrounds, walkmesh, camera, gateways, scripts, text, NPC models and animations; the main characters' `main_chr.fs` nested inside | **first area (A)** | trio structure `[verify: doc H-PC]`; the name `field` is `[verify: inference]`, the `lang-<xx>` folder `[verify: snippet S-BAT]`; nesting `[verify: doc H-MCH]` |
| `main.fi/.fl/.fs` | `kernel.bin`, `init.out` and other core data | battle | name and contents `[verify: inference]`; one forum post edits `kernel.bin`, `wepon.bin` and `init.out` inside a Remaster ZZZ (which one is not stated) `[verify: snippet Z1]` |
| `battle.fi/.fl/.fs` | `scene.out`, stages, monsters, character and weapon models | battle | the name `[verify: snippet S-BAT]`; contents `[verify: doc H-SCENE, H-MON, F-IDX]` |
| `world.*`, `menu.*`, `magic.*` | world map, menus and text, magic effects | later | names `[verify: inference]` |
| the game executable (`FF8_EN.exe` `[verify: inference]`) | card stats and rules constants (2000/2013) | Triple Triad | `[verify: snippet Z1]`; exe tables exist `[verify: doc H-EXE]` |
| Remaster only: `main.zzz`, `other.zzz` in the game root | everything above packed in the ZZZ container | if the owner has the Remaster | `[verify: snippet Z1]` |
| not needed first | movies (`.avi`, `.pak`), music (`.sgt` in 2013, `.ogg` in the Remaster), saves | later | `[verify: snippet S-DIFF]` |

### 8.4 FF9: what the owner supplies

The owner supplies the **Steam FF9 install root** (app 377840 `[verify: doc M-README]`) and its appmanifest, and says
whether Memoria or other mods are installed. Expected candidates (all `[verify: doc M-FOLD]` unless noted):

| Candidate file | Role | Needed for |
|---|---|---|
| `StreamingAssets/p0data1X.bin` (nine files) | field maps (`FieldMaps/<id>/` atlas, `.bgs`, `.bgi`, effects) | **first area** |
| `p0data4.bin`, `p0data5.bin` | 3D models, animations | models |
| `p0data7.bin` | event scripts | story runtime |
| `FF9_Data/resources.assets` | texts per language, other assets | names, dialogue |
| `p0data2.bin` | battle scenes: monsters, formations, scripted attacks | battle |
| `FF9_Data/sharedassets2.assets` | UI atlases, Tetra Master card art | UI, cards |
| `p0data3.bin` | world map | later |
| `p0data61.bin`, `p0data62.bin`, `p0data63.bin` | music, sounds, effects | later |
| the game's compiled code, `x64/FF9_Data/Managed/*.dll` `[verify: inference]` | candidate home of the gameplay tables (items, abilities, characters, Tetra Master stats) | battle (route 1 of decision D9) |
| Memoria files if present (`Memoria.ini`, `Plugins/`, `StreamingAssets/Data` CSV) `[verify: doc M-README, M-DB]` | **modded** provenance class | route 2 of decision D9 only |

The path of `StreamingAssets` relative to the install root differs between the sources I read
`[verify: inference]`; the probe lists what is really there.

### 8.5 Editions and languages

* **FF8:** Steam 2013 and Steam Remastered are two editions (never merged); a language is a `lang-<xx>` folder
  `[verify: snippet S-BAT]` or a set of ZZZ entries `[verify: inference]`; one language per set, as FF7.
* **FF9:** one Steam edition; all languages live in the same files, so there is no per-set language split.
* **Mods:** Memoria, Demaster, Angelwing, Moguri, Tonberry and the like make a set `modded`
  `[verify: snippet S-DIFF]`. Players with different modded states cannot share an instance, by fingerprint.
* **PS1:** not first (the owner chose PC for FF7). The disc reader (`iso9660`, `image`, `cue`) is reusable; FF8's PS1 data
  sits in `FF8DISCn.IMG` containers `[verify: doc H-PS1]`.

### 8.6 Library storage for large archives

The library **copies** each file into `library/blobs/<blake3>` and caps one file at 4 GiB (`DEFAULT_MAX_FILE_SIZE`). FF9's
Unity archive sizes are unknown `[verify: inference]` and may be large. Options: copy (simple, disk is plentiful) or a
**reference-in-place** entry (`path`, size, mtime, blake3, re-checked at every open) for archives above a threshold. The
choice waits for the probe's sizes (the storage half of decision D12 is a lead decision).

---

## 9. Milestone plan

Increments end with a build, a startup check and one-feature smoke, as `IMPLEMENTATION-PLAN.md` section 9 says. Sizes
are rough (S a checkpoint, M a few, L a handful, XL a track).

```
Track B  shared work, starts now, needs NO FF8/FF9 media      Track A  FF8 first area (critical path), needs FF8 media
  B1 wardrobe survey (FF7 data)                                 A0 probe -> A1 import -> A2 archive reader
  B2 slot split + FF7 migration (R7: F1-F3)                     -> A3 field decode -> A4 adapter + two bots
  B3 battle envelope + synthetic second engine (R5)             -> A5 window -> A6 models -> A7 smoke
  B4 adapter API v2 (R4)
  B5 multi-game server (R6)                                   Track N  FF9: N0 probe (early, cheap) -> N1 Unity reader
  B6 sheets, transfer journal, card rules (R8)                  -> N2 field decode -> N3 adapter + two bots -> ...
Track C  FF8 depth, after A                                   Track E  story runtime: per-game script VMs; owner-gated,
                                                                       parallel, NOT on the A, C or D path
```

### 9.1 Track A: the first two-player FF8 area (critical path)

**This needs none of:** the battle envelope, multi-game hosting, the identity/slot model, a script VM, a protocol bump.
FF8 runs on today's single-game server (`--game ff8`), exactly as `md1stin` did.

| # | Increment | Ready-check | Media | Refactor | Size |
|---|---|---|---|---|---|
| A0 | **Probe**: `fmv-import probe ff8` reads the appmanifest, lists and hashes the candidate set | report shows the set; anchors of Appendix 3 compared; deviations written down | FF8 install | R1 min | S |
| A1 | **Library set** with provenance (`import-pc` generalised) | `list` shows labels; source mtime and hash unchanged | FF8 install | R1 min | S |
| A2 | **`fmv-ff8` archive reader**: `.fl` list, `.fi` index, `.fs` data, LZS, nested archives (ZZZ if Remaster) | lists the fields; extracts one; sizes equal the `.fi` | yes | R3 | M |
| A3 | **Field decode**: walkmesh and access, camera, gateways, tile background with depth | an offscreen render of the chosen field shows the walkmesh projected through its own camera on its background (the wiki reports `bghall_1` lines up `[verify: doc H-CA]`) | yes | R2 lite | L |
| A4 | **`Ff8Adapter` and `Ff8Area`**, server `--game ff8`, client opens it | READY line names `Ff8/bghall_1`; two headless bots see each other; an off-mesh move is corrected; fingerprints bind the edition | yes | none | M |
| A5 | **Window**: background, walkmesh overlay, player markers | the window shows the field with two players | yes | R2 lite | M |
| A6 | **Models**: avatars drawn as FF8 models from the field's own `chara.one` cast (2.6) | players walk as models | yes | R2 | L |
| A7 | `scripts/ff8-smoke.sh` | two bots pass, read-only, throw-away state | yes | none | S |

A **FIXTURE** FF8-shaped area is not FF8 support (README rule 5): none of A counts until a real field decodes from owned media.

### 9.2 Track B: shared work that can start now

| # | Increment | Ready-check | Needs |
|---|---|---|---|
| B1 | **Wardrobe survey** (FF7 data on this box): which skeletons have idle, walk and run clips | a table of candidate non-canon looks, or "none"; decides decisions D2 and D4 | FF7 data |
| B2 | **Slot split and FF7 migration** (F1 to F3): explicit slot, no Cloud default, canon looks reserved | login no longer derives the fighter from the look; an old account keeps who it fought as | none |
| B3 | **Envelope, `BattleSession`, synthetic second engine** (R5) | `battle-smoke` passes on FF7 and on the FIXTURE engine, 0 divergences | none |
| B4 | **Adapter API v2** (R4) | FF7 implements the new capabilities; FF8 starts with the defaults | none |
| B5 | **Multi-game server** (R6) | one process serves FF7 and the FIXTURE adapter, parties per game | none |
| B6 | **Sheets, transfer journal, card rules** (R8, 5.5) | a two-account transfer survives a kill between its two writes; a synthetic card table plays through server and clients | none |

### 9.3 Track C: FF8 depth (after A)

| # | Increment | Notes |
|---|---|---|
| C1 | Exits and travel: gateways to `Exit`, arrival triangle and facing | world-map targets stay closed |
| C2 | NPC placement: `.jsm` analysis, as FF7 needed script analysis | no VM yet |
| C3 | Identity, slot and wardrobe in FF8 (needs B2, B4) | the owner's model made real in a second game |
| C4 | Battle stages A to D (5.4; needs B3, B5, `main` and `battle` sets) | server-authoritative, replayed |
| C5 | Triple Triad PvP (5.5; needs B6; the card table needs the exe or Remaster file) | friendly first |
| C6 | World-map area type | its own design |

### 9.4 Track N: FF9, after the FF8 area but with an early probe

| # | Increment | Ready-check |
|---|---|---|
| N0 | **Probe** (cheap, can run now-ish): names, sizes, hashes, mods | tells us the container family and whether decision D9's route 1 can exist |
| N1 | **`fmv-unity`**: bounded, panic-free reader for the field archives first | lists `FieldMaps/*` and compares the count with about 674 `[verify: snippet S-MK]` |
| N2 | Field decode: atlas, `.bgs` tileset, `.bgi` walkmesh, camera | an offscreen render of one field |
| N3 | `Ff9Adapter`, server `--game ff9`, two bots, markers | as A4; the first area is free roam (6.3) |
| N4 | Models: skinned meshes and clips from the model and animation archives | players walk as models |
| N5 | Tables route decided (decision D9), then battle stages A to D (6.4) | needs B3, B5 |
| N6 | Tetra Master PvP (6.5) | needs B6 |

### 9.5 Track E: story runtime (all games, owner-gated)

| # | Increment | Needs the hosted VM? |
|---|---|---|
| E0 | **The layer on talk conversations in FF7** (F5a): `Added` sources around `TalkSession`, a server-held flag set, `StoryVars` fed from the account's story, the message types promoted out of `fmv-ff7` | no |
| E1 | A **talk walker per game** (FF8 `.jsm`, FF9 `.eb`), which also gives NPC dialogue and a first story-variable model for each | no |
| E2 | **Segment data and canon sheets** (2.3, 2.5) per game | partly (story variables) |
| E3 | The **hosted script VM** per game, party-affecting opcodes intercepted, run per instance | this is it |
| E4 | Scene staging (observer marks, scene lock) and the layer inside shared scenes (2.7) | yes |

**None of Tracks A, C or N waits for any of it.** Sections 2.5 to 2.7 are not deliverable without E3; the talk half of
2.8 is (E0, E1). The size of the E3 job per game is real: FF8's wiki has a page per documented field opcode, with ids
running from `000` to `177` in hex `[verify: doc H-IDX]`, and FF7's read-only walker already interprets dozens of message,
branch, request and variable opcodes just to answer "which messages show, in what order".

---

## 10. Risks

| # | Risk | Effect | Mitigation |
|---|---|---|---|
| 1 | FF9 data is in Unity archives; the exact container, compression and asset types are unconfirmed | no FF9 field, model or table until read | probe first (N0); a bounded `fmv-unity`; fuzz it; keep FF9 behind FF8 |
| 2 | FF9 gameplay tables may be in compiled code (or absent from data) | battle blocked, or forced onto modded or approximate tables | decide decision D9 after the probe; label approximations; never mix vanilla and modded sets |
| 3 | `net.offline = true` | new crates may not resolve | `lz4_flex`, `lzma-rust2`, `bzip2`, `ruzstd`, `zip` are already cached; a Unity parser is not; confirm resolution before relying on them |
| 4 | Edition drift: the best FF8 documentation describes 2000/2013; the Remaster repacks and changes models | wrong layouts | separate edition labels; Appendices 2 and 3 record what was confirmed; decoders are total and fail soft |
| 5 | Format claims here are unverified | wasted work | every claim tagged; the probe and the checklist come before decoders |
| 6 | No hosted script VM exists (FF7 has only a read-only talk walker) | the scenes half of the story model (O3, O4 of 2.1) is blocked | free-roam areas first (as FF7 M1); a separate track E, starting with the talk layer (E0), which needs no VM; do not promise story presence in scenes early |
| 7 | Few non-canon models may be pose-ready (FF7 survey unknown) | "thirty Clouds" cannot be fully avoided in the field | B1 first; recolour; reserved canon models; an opt-in cosplay later |
| 8 | Battle bodies: FF7's battle models are canon only | a player in Tifa's slot looks like Tifa | decision D4, honest labels, a later composite pipeline |
| 9 | Dialogue layer volume and edition drift of anchors | little value, or mis-fired lines | a tiny first set; anchors carry a script-segment hash; a lint that bans copying canon strings |
| 10 | Timing-based inputs (FF8 summons and limits) under latency | unfair or unresponsive | inputs as bounded command parameters with server-side windows; the engine never trusts a client result |
| 11 | Protocol churn per game | repeated version bumps | envelope plus per-game schema versions and golden tests |
| 12 | Two-account card transfers | duplicated or lost cards after a crash | the transfer journal (R8) before any stake |
| 13 | FMV | the player cannot appear in prerendered video | player beats before and after FMV |
| 14 | Names outside the game font | broken glyphs in game windows | client-font fallback (decision D8) |
| 15 | Archive sizes and the 4 GiB cap | slow or impossible FF9 import | reference-in-place (decision D12) |
| 16 | Balance and feel of AI companions and scaling | frustration or trivial fights | engine-internal seeded logic; label approximations as FF7 does; tune against owned-data sanity checks |

---

## 11. Owner decisions, with recommendations

| # | Decision | Options | Recommendation | Blocks |
|---|---|---|---|---|
| D1 | **Seating.** Does the protagonist always keep a combat seat, or do players seat first? | (a) protagonist always fights, so players per battle = slots minus one; (b) players first, the cast fills what is left, the protagonist yields to a full party | **(b)**, with per-battle policies `PlayersFill` (default), `CastLed`, `CastOnly` (a curated list of defining moments); party cap = slot count (FF7 3, FF8 3, FF9 4); the protagonist's own template is never offered to players | seating, party caps (R7) |
| D2 | **Original-character looks.** | (a) canon models reusable; (b) canon cast models reserved for NPC use, looks from a wardrobe of the owner's own non-canon models plus recolour; (c) commissioned original art | **(b)**; cosplay as a later opt-in, never where that canon NPC is present; no new art at first | wardrobe, F2 |
| D3 | **AI companions.** Is the canon cast acceptable as AI companions in battle when slots are free? | yes (engine-internal, seeded, replay-safe) / no (leave seats empty) | **yes**, engine-internal, with server-issued recorded commands as an escape hatch | F4, ally logic |
| D4 | **Battle bodies** while only canon battle models exist (FF7). | (a) the slot's canon battle body with the player's name and tint, labelled interim; (b) neutral stand-in silhouettes; (c) recolour or accessory composites from owned models; (d) delay battle presentation for originals | **(a) now**, **(c)** as the target once B1 reports; say plainly that (a) looks like the canon member | F3 |
| D5 | **FF7 retrofit timing and existing accounts.** | do F1 to F4 before any FF8/FF9 demo of originals / after | **before**: snapshot each account's current look-derived character as its explicit slot once, then decouple; stop defaulting to Cloud | B2 |
| D6 | **Party-swap segments** (FF8 Laguna dreams, FF9 splits and Steiner/Vivi/Dagger stretches, FF7 stretches without Cloud): what do originals do? | (a) watch from marks (`CastOnly`); (b) seat if the segment's party fights and slots allow; (c) separate "mirror" content | **(a)** for dream and vision segments, **(b)** for ordinary sub-parties; the leader chooses which team a split follows, others watch | segment data, Track E |
| D7 | **Card games** (Triple Triad, Tetra Master). | friendly matches only / stakes with consent; rare cards unique or per account | **friendly first**; stakes later with mutual consent and the transfer journal; rare cards per account and excluded from stakes | B6, C5, N6 |
| D8 | **Dialogue layer.** Who writes it, pronouns, name rendering, canon choices. | project-authored original text, optional pronoun set in the identity, game-styled window with client-font fallback for names, leader answers canon choices | **all of those**; start with one showcase NPC conversation on the FF7 seam that already exists (`MessageOrigin::Added`), not a scene | layer, Track E (E0) |
| D9 | **FF9 gameplay-table route** if the tables are in the game's code. | (1) read the owner's vanilla assembly; (2) read Memoria CSV from a Memoria install (`modded`); (3) author approximations | probe first; **(1)** if the tables are there; accept **(2)** as an explicit `modded` set that cannot play with vanilla sets; **(3)** last resort | FF9 battle |
| D10 | **Cinematic locks** (summons, limit breaks, eidolons). | freeze the battle clock for everyone / overlay while the clock runs | **overlay with a short engine-side beat** (faithful pausing is slow with three summoners) | FF8, FF9 battle |
| D11 | **Hosting.** One multi-game server process or one per game? | one process with an adapter registry, a `World` partition and a `PartyManager` per game; or separate servers with a shared account service | **one process**; parties stay per game, friends and identity are cross-game | R6 |
| D12 | **Editions and storage.** Which FF8 edition first, does the owner run Memoria on FF9, and how are big archives stored? | FF8 Steam 2013 / Remastered / both; FF9 vanilla / Memoria; copy / reference-in-place | **FF8 2013 first if owned** (best documented), Remastered as a second edition label; FF9 vanilla; **reference-in-place** for archives above a threshold chosen after the probe; PS1 later | A0, N0 |
| D13 | **Order of work.** | A first, then FF9 / A and B in parallel with an early FF9 probe | **A and B in parallel; B2 first** (the owner's model blocks every public demo of originals), **N0 early**, FF9 proper after the FF8 area | planning |

---

## Appendix 1. Sources

Read in full or in part with the fetch tool (summaries by the tool; check wording against the pages when it matters):

* **H-IDX** https://hobbitdur.github.io/FF8ModdingWiki/ (table of contents)
* **H-PC** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/miscellaneous/pc-media/
* **H-PS1** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/miscellaneous/playstation-media/
* **H-WALK** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/field/field-file-format/field-walkmesh/
* **H-INF** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/field/field-file-format/field-gateways/
* **H-CA** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/field/field-file-format/field-camera/
* **H-MAP** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/field/field-file-format/field-background-tile-data/
* **H-MCH** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/field/field-file-format/field-character-models/
* **H-SCENE** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/battle/battle-structure-sceneout/
* **H-MON** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/battle/monster-file/
* **H-KERN** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/main/kernel/ (the index page only)
* **H-CARD** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/list/card-list/
* **H-EXE** https://hobbitdur.github.io/FF8ModdingWiki/technical-reference/exedata/exe-data/ (2013 English PC release)
* **F-PC**, **F-IDX** https://wiki.ffrtt.ru/index.php/FF8/PC_Media and https://wiki.ffrtt.ru/index.php/FF8 (backup wiki)
* **Z1** https://forums.qhimm.com/index.php?topic=19209.0 (zzzDeArchive thread; claims that rest on one poster's remark are tagged `snippet`); **Z2** https://github.com/Sebanisu/zzzDeArchive
* **WP8** https://en.wikipedia.org/wiki/Final_Fantasy_VIII (Gameplay; the first 100,000 characters)
* **M-README** https://github.com/Albeoris/Memoria
* **M-FOLD** https://github.com/Albeoris/Memoria/wiki/Mod-folders
* **M-DB** https://github.com/Albeoris/Memoria/wiki/Externalised-databases
* **M-WIKI** https://github.com/Albeoris/Memoria/wiki (page list only)
* **WP9** https://en.wikipedia.org/wiki/Final_Fantasy_IX (Gameplay; the first 100,000 characters)

Seen only as search-result snippets (leads, not facts):

* **S-APP** Steam community discussion URLs `steamcommunity.com/app/39150/...` (FINAL FANTASY VIII) and `.../app/1026680/...` (FINAL FANTASY VIII - REMASTERED)
* **S-BAT** https://forums.qhimm.com/index.php?action=profile&area=showposts&u=1102 (a post on the Steam version: copy `battle.fl`, `battle.fi`, `battle.fs` from `FINAL FANTASY VIII\Data\lang-en\`)
* **S-DIFF** https://finalfantasy.fandom.com/wiki/Final_Fantasy_VIII_version_differences and its Talk page; Steam and ResetEra threads on Remaster mods
* **S-TT** https://finalfantasy.fandom.com/wiki/Triple_Triad_(Final_Fantasy_VIII); https://gamefaqs.gamespot.com/ps/197343-final-fantasy-viii/faqs/72525/rule-variations
* **M-PLAY** https://github.com/Albeoris/Memoria/blob/main/Assembly-CSharp/Global/ff9play.cs
* **S-HW** https://steamcommunity.com/sharedfiles/filedetails/?id=903977018 (Hades Workshop and `p0data4`)
* **S-MK** https://pypi.org/project/ff9mapkit/1.0.0b22/
* **S-TR9** https://finalfantasy.fandom.com/wiki/Trance_(Final_Fantasy_IX)
* **S-TM** https://finalfantasy.fandom.com/wiki/Tetra_Master_(minigame); https://www.cavesofnarshe.com/ff9/tetramaster.php

Tried and not available: the Qhimm wiki on Fandom (HTTP 402), PCGamingWiki's FF9 page (HTTP 403). The OpenVIII and Deling
READMEs name no editions or archives. No game data was downloaded.

---

## Appendix 2. Evidence tables (what is claimed, by whom, how to check)

### 2a. FF8

| # | Claim | Source | Tier | How to check on the owned files |
|---|---|---|---|---|
| 1 | Data is in `<install>\Data\` as sets of three files (five, or six for the "maximum installation") | H-PC, F-PC | doc | list `Data/` and `Data/lang-*`; count the trios |
| 2 | `.fi` is 12-byte records: u32 uncompressed size, u32 offset in the `.fs`, u32 compression (1 = LZS, 0 = none) | H-PC | doc (byte order and offset base are not stated) | `len(.fi) % 12 == 0`; offsets fall inside the `.fs` |
| 3 | `.fl` is CRLF text, one absolute path per line, in `.fi` order | H-PC | doc; the pairing by position is implied | line count equals `len(.fi) / 12` |
| 4 | The sets are called field, main, battle, world, menu, magic | none | inference; S-BAT mentions `battle.fl/.fi/.fs` under `Data\lang-en\` (snippet) | the probe's listing |
| 5 | The LZS is FF7's LZSS, with a leading length word | F-PC links FF7's LZS page | doc for the link; inference for the length word | decode one entry; compare the size with the `.fi` |
| 6 | Nested archives: `field.fs` holds `mapdata.fs` (PS1 note) and `main_chr.fs` | H-PS1, H-MCH | doc | list `.fl` entries that end in `.fs` |
| 7 | `.id`: u32 count, 24-byte triangles (three vertices of four int16: x, y, z, a copy of vertex 0's z), then 6-byte access records (three int16, -1 is a wall); size `4 + 30*count`; 895 retail files; 512-triangle ceiling | H-WALK | doc (2000 PC) | size identity per file; count of `.id` entries |
| 8 | The walkmesh is the same family as FF7's | F-IDX only | snippet | run the FF7 walkmesh decoder on one field |
| 9 | `.inf`: 12 gateways of 32 bytes (segment, arrival x/y, arrival triangle, target id, facing) and 12 door triggers of 16 bytes; 676 bytes; `0x7FFF` empty, below `0x48` the world map; 893 files with 1632 live gateways; older 672, 576 and 504-byte variants | H-INF | doc (2000 PC) | file size 676; gateway counts |
| 10 | `.ca`: per-camera record of axes over 4096, translation, zoom (0x28 bytes with a repeated zoom); projection `(R*P + T).xy * zoom / z` | H-CA | doc | project the `bghall_1` walkmesh and compare with the background |
| 11 | `.map`: headerless 16-byte tile records of 16 by 16 tiles, terminator `0x7FFF` plus 12 zero bytes; `Z` is camera distance over 4; a 4096-slot ordering table; median 8 distinct `Z` per field | H-MAP | doc | composite one field; count `Z` levels |
| 12 | `main_chr.fs`: `d000` to `d075` (71 files); MCH has a 0x100-byte header, 128 by 128 paletted TIMs; animations come from the field's `chara.one` (packed 10-bit angles) | H-MCH | doc | list; parse one |
| 13 | `scene.out`: 1024 records of 128 bytes, no header; level byte rules; monster id = `c0mXXX` number + 0x10 | H-SCENE | doc | size equals 131,072 |
| 14 | `c0mXXX.dat`: 11 sections (c0m127 has 2); stats are section 7 | H-MON | doc | header section count |
| 15 | `kernel.bin`: header (section 0) then 31 sections | H-KERN | doc (index page only) | parse the header |
| 16 | Triple Triad: 110 cards (rare 77 to 109), rule bitmask, trade enum | H-CARD | doc | compare card names with the text data |
| 17 | Card stats are in the exe (2000, 2013) and in a separate file in the Remaster | Z1 (one poster) | snippet | locate a 110-record table in the exe; record its offset |
| 18 | Remaster: `main.zzz` in the game root, `other.zzz` with music `.ogg` | Z1, snippets | snippet | list the root |
| 19 | ZZZ entry layout | none | inference | read zzzDeArchive's source as a reading aid; verify on `main.zzz` |
| 20 | Steam ids 39150 (2013) and 1026680 (Remaster) | S-APP | snippet | read `appmanifest_<appid>.acf` |
| 21 | PS1: `FF8DISCn.IMG` with 8-byte index entries (sector, length), LZS | H-PS1 | doc | later |
| 22 | These pages describe the 2000 PC and 2013 builds, not the Remaster | the pages' own text | doc | n/a |

### 2b. FF9

| # | Claim | Source | Tier | How to check on the owned files |
|---|---|---|---|---|
| 1 | Steam app 377840 | M-README | doc | the appmanifest |
| 2 | Archive roles (section 6.2 table) | M-FOLD | doc | the probe lists the files and sizes |
| 3 | Field archives hold `Assets/Resources/FieldMaps/<id>/` with `atlas.png`, `<id>.bgs.bytes`, `<id>.bgi.bytes`, effect files | M-FOLD | doc | after the Unity reader: enumerate |
| 4 | `.bgs`, `.bgi`, `.eb` formats; about 674 real fields | S-MK | snippet | count the fields |
| 5 | Battle: `BattleMap/BattleScene/EVT_Battle_*/{*.raw17.bytes, dbfile0000.raw16.bytes}`; raw16 holds stats, skills, formations | M-FOLD | doc | enumerate |
| 6 | Texts per language (`.mes`) in `resources.assets` | M-FOLD | doc | enumerate |
| 7 | Tetra Master art in `sharedassets2.assets`; card text in `minigame.mes`, `minista.mes` | M-FOLD | doc | enumerate |
| 8 | The archives are Unity serialized or bundle files | S-HW; the format is mine | snippet; inference | the decoder's first run reads the header signature |
| 9 | Vanilla gameplay tables live in the compiled assembly | none | inference | list `Managed/*.dll`; look for static data; if absent, routes 2 or 3 |
| 10 | Memoria externalises the tables into `StreamingAssets/Data` CSV | M-DB | doc (Memoria, not vanilla) | modded provenance only |
| 11 | Party of four | none | inference | the party array size in data |
| 12 | A "scenario counter" is the story variable | M-WIKI (page title only) | doc (title) | find it in the event scripts |
| 13 | Memoria installs next to `FF9_Launcher.exe`, with `Plugins/` and `Memoria.ini` | M-README | doc | detect for the `modded` tag |

---

## Appendix 3. First-contact checklist (when the owner's files arrive)

**FF8**

1. Run the probe. Record the trios (or ZZZ files), the language folders, the executable name. For every `.fi`, check
   `size % 12 == 0` and that its `.fl` has as many lines.
2. Compare the retail anchors: 895 `.id`, 893 `.inf` (1632 live gateways), `d000` to `d075` (71 files), `scene.out` 131,072
   bytes, 110 cards. Equal anchors raise confidence that the 2000/2013 pages describe this build; a difference is recorded,
   never forced.
3. Decode one field (`bghall_1`): walkmesh size identity, `.inf` size 676, camera projection over the background,
   tile `Z` levels.
4. Find the card table (exe, or the Remaster's file) and note its offset.
5. Write the result into Appendix 2 by changing each confirmed row's tier to "confirmed on owned <edition>, <date>",
   as `fmv-ff7` did ("Verified on owned FF7 Steam Edition (en) on 2026-11-08").

**FF9**

1. Run the probe: names and sizes of `p0data*`, `*.assets`, `Managed/*.dll`; mods present or not.
2. First run of the Unity reader: header signature, block compression kinds, asset counts per archive, asset names;
   compare the field count with about 674.
3. Answer decision D9: are items, abilities and characters in the assembly, in assets, or only in Memoria's CSV?
4. Decode one field (`.bgs`, `.bgi`, atlas) and choose the first area from the ones that decode cleanly.
