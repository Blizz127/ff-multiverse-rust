# Disc 1 route and implementation manifest

This manifest records the work needed after the Sector 7 station arrival through the
original Disc 1 ending. It is an implementation inventory, not a claim that those chapters
are playable. The production scene registry currently ends at the passenger-train arrival;
the next train lesson, return, departure and station definitions remain unregistered.
See [DISC1-IMPLEMENTATION.md](DISC1-IMPLEMENTATION.md) for the implementation objective and
the distinction between prior acceptance results and the current changes.

[disc1-route-manifest.json](disc1-route-manifest.json) contains chapter records, exact
field/maplist IDs, pinned instruction references, required modes, known gates, exclusions
and an all-disc field catalog. It deliberately records `path_executed: false` for this
audit. No chapter was played through and no later battle was simulated for this manifest.

## What the evidence proves

The owned original English PC archive decodes into 702 fields with no field/script decode
errors. Its maplist contains 788 names. The static inventory records 1,036 used gateways
and 2,511 candidates from the bounded script-exit scanner. These are **all-disc counts**,
not the number of Disc 1 fields or reachable exits. The owned battle archive has 1,024
addressable formations. Source hashes and logical archive paths are in the JSON; no game
dialogue, bytecode, images, models or audio are included.

An instruction reference proves the opcode and operands occur at that location. A gateway
proves a stored destination. A scanner path adds candidate call/guard context, but it can
combine incompatible conditions when scripts change state. None alone proves a complete
route. Chapter order below is a planning sequence grounded in original destinations,
dispatchers and branch conditions; each joined path still needs executable acceptance.
Story moments and filename prefixes are insufficient evidence by themselves.
Native mode-dispatch references below come from a pinned PSX decompilation; they do not
independently establish identical PC machine-code behavior.

Offsets in the JSON are relative to the field script section; field IDs are the owned
maplist indices. A field can belong to several chapters. Associated optional fields and
later revisit scripts do not automatically become first-visit requirements.

## Chapter spine and authored branches

The table names representative anchors; the JSON lists the larger field groups and
instruction sites. “Required” below means the subsystem is required to implement the
authored branch, not that every optional branch must be taken in one playthrough.

| Segment | Owned field anchors / IDs | Required behavior and boundaries |
| --- | --- | --- |
| Sector 7 and Seventh Heaven | `mds7st3`146, `mds7`151, `mds7pb_1`154, `mds7pb_2`155; memory `nivl_e1`294 | Town/bar producer flags, all affection choices, canon party selection, payment, rest/tutorials and the memory transfer. Completion of the next-mission setup leads to the train at moment108; a moment alone does not select every producer. |
| Second train and Reactor 5 | `tin_1`139 through `tin_4`142, `tunnel_1`161–`tunnel_3`163, subway164–169, reactor128–132, `southmk2`127 | Live carriage timers, escape-position byte3:127, alternative tunnel arrivals, ladders, item effects and simultaneous-button input. Required reactor memory enters `nvmkin1`322 and returns. Air Buster364 has a side-attack layout; movies25/26 and the resulting party/health state cannot be skipped. |
| Church, house and park | `chrin_1a`182/`chrin_1b`183, loft184–186, `church`181, `eals_1`187, `ealin_2`190, `mds6_3`194 | Aerith recruitment/naming, help/barrel choices, Aerith-only battle alternatives and affection. The early house dream uses `niv_cl`276 before returning; sneaking out has failure/retry state. |
| Wall Market and Corneo | `mrkt2`195, `mrkt1`205, shops196–204, Honey Bee214/216/218–220, mansion206–211 | Timed squats, purchases, outfit/key-item flags, all three Corneo selections and their different fights/dialogue. The presidential intercut goes through `blin70_1`266 and `blin70_2`267 before the sewer; it is not the later rescue chapter. |
| Sewers, graveyard and pillar | `colne_b1`212, `colne_b3`213, `mds7st1`144/`mds7st2`145, pillar158–160, `mds6_22`193 | Sewer boss386, train-position puzzle and dynamic collision/triangle locks, party changes, pillar battle412 and movies27/28. Aftermath producers and rescue preparation remain separate stateful interactions. |
| Rescue preparation and Shinra approach | house188/189, memory station146/147, `mkt_w`196, climb222–225, entrance227/228, lobby234 or stairs229–231, floors238–254 | Elmyra’s post-pillar flashback and return (248→251→253), house continuation, the 300-gil purchase of three item85 batteries and independent climb consumption; lobby/stairs alternatives; stealth/failure encounters; mayor puzzle and paid hints; keycards, model pieces and optional floor rewards. The mayor samples engine byte1:18, so its answer is not determined by new-game defaults. |
| Lab, capture and escape | lab256–263, cells258, president266/267, roof268, `eleout`233, lobby234 | Canon membership and controlled-party switches, affection and branch-dependent battle rosters, boss456, cell choices and blood-trail variants. The field invokes460 once, but its battle metadata chains to461; roof464 has its own battle mode. |
| Motorcycle and first world travel | `blackbgb`103 → `roadend`226 → `mds5_5`170 → `wm0`1 | Movie30, motorcycle module0, tutorial/result flags, minigame damage/health handoff, Motor Ball468, party selection and the gated world exit. `blackbgb` also hosts later transitions; its entire script is not a Midgar route. |
| Kalm, farm, marsh and mine | Kalm328–342; farm343–345/347, marsh348, mine349–352; reused Nibelheim fields | Kalm's flashback requires temporary party identities, battle/movie work, memory fields and the return. Marsh traversal and its world encounter policy require world data; catching a chocobo and fighting the marsh enemy must not be made unconditional. Mine dialogues and exits are separate authored gates. |
| Optional Fort Condor | fields353–358, including `convil_2`356 | Strategy module3, funding/rewards, battle-result alternatives and revisits. Its repeated story-dependent opportunities are not one mandatory fight or one once-only scene. |
| Junon, ship and Costa del Sol | lower Junon428–434, upper `junon`359/`junin1`386, dock382, ship436–440, Costa441–449 | Bottomswell480, CPR, dolphin positioning, disguise/parade and farewell scoring/rewards. These input games run in field scripts. Dock and ship use world transport entries `wm35`36 and `wm36`37; a direct field edge between them and Costa is unproven. Ship conversation/party gates precede Jenova-BIRTH488. |
| Corel, Gold Saucer and prison | Corel450 onward, `mtcrl_1`459, `gldst`496/`gldgate`497, prison fields, `dyne`480, race511/512 | Rail/bridge input and branching pickups, ropeway movies, gil/ticket/GP admission, Cait Sith recruitment, prison party restrictions and Dyne, race preparation and chocobo module1 results, then buggy/world handoff. Race completion is a distinct mode transaction. |
| Gongaga and Cosmo Canyon | optional Gongaga jungle/town; `cos_btm`525, `cos_top`540 and cave fields | Gongaga has optional encounters/rewards and later required return state. Cosmo requires buggy-related entry state, group placement, planetarium presentation, cave puzzles and Gi Nattak555, then the authored party/world return. |
| Nibelheim, mountain and Rocket Town | Nibelheim282 onward, mansion and Mt Nibel, Rocket557 onward, `rckt32`774 → `sky`87 | Separate current-time/flashback contexts; optional safe/Lost Number576/Vincent route; mountain switches/tubes, Materia Keeper595, Cid/party state, Palmer600, movies46/47 and Tiny Bronco/world transport. Noncontiguous field774 is required; a prefix/range-only catalog would omit it. |
| Optional recruitment and Wutai | forest recruitment `yougan2`; theft variants `yougan`/`yougan3`, Wutai town/mountain/pagoda | Preserve recruitment questions, money effects and membership. Materia theft/removal/restoration is an inventory/equipment ledger, not one flag. Chase switches and rescue fights differ from the Yuffie-only pagoda's battle-result/retry state. |
| Keystone and date | Saucer arena/inn/theater/gondola and `chorace2` | Dio's arena sequence, keystone state, GP/battle points, all affection-based partners and theater answers, gondola movies and Cait Sith's chase. Optional attractions are separate branches; a later-discs-only attraction cannot be enabled merely because its opcode exists here. |
| Temple of the Ancients | `jtempl`600 and associated temple fields; aftermath `jtemplc`775 | Maze/clock/door puzzles, ordinary and scripted encounters, Red Dragon652, Demons Gate644, key items, materia/party effects, movies and the collapse/aftermath return. A boss-only replay does not implement the temple. |
| Bone Village to Disc 1 ending | `bonevil`617/`bonevil2`772, forest618 onward, capital, `ancnt2`647/`ancnt3`648/`ancnt4`649, `blue_2`641, `losin2`632, `blackbgb`103 | Digging and the forest/key-item gate, rest/party changes, altar input, movie50, Jenova-LIFE671 with battle mode0x120, mourning/party-specific requests and movie51. Finish the save/disc-transition handshake described below. |

The field catalog also retains branches not yet promoted into a reviewed chapter recipe:
treasures, shops/inns, collectible quests, alternate recruitment outcomes, optional minigames
and revisits. Their static references are discoverable; their complete choice/reward matrix
is not yet established. This is an explicit remaining audit obligation, not permission to
omit them from the full-game objective.

## Exact Disc 1 boundary

The ending is not “story moment677 reached.” The original static sequence is:

1. `ancnt3`648 invokes formation671 at `0x025C`, writes673 at `0x0260`, then transfers
   to `ancnt4`649. That field transfers to `blue_2`641; its movie51 precedes674 and `losin2`632.
2. `losin2` writes677 before the final dialogue work completes. Cloud's slot sets bank3:136
   bit4 and transfers to `blackbgb`103 at `0x0979`.
3. The normal bit4 handler clears that flag and asks the save question (answers2/3).
   Its optional save uses `MENU14`, disc byte13:0 and a resume marker in3:134 bit2.
4. The normal path executes `DSKCG2` at `0x0476`; the saved-resume path has its own
   `DSKCG2` at `0x041F`. After the handshake, the script sets3:137 bit1 and transfers to
   `lost2`634 at `(-259,5042)`, triangle113, direction0.

This pins the boundary and both save paths without claiming the intervening scenes have
executed. The native handler stores the requested disc and waits for completion before
advancing. A PC port can implement an appropriate logical transition, but must preserve
the state/save/resume contract. [Pinned native DSKCG handler](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L2548)

## Modes that field scene registration cannot supply

* **World travel and transport:** maplist IDs1–64 (`wm0`–`wm63`) enter the native world
  module, rather than an ordinary field. Movement, encounters, Yuffie/chocobo conditions,
  ports, buggy and Tiny Bronco travel need world geometry/scripts and saved vehicle state.
  The inspected owned snapshot has six field/battle/kernel files and no world program/data;
  field departures therefore do not prove the next world arrival.
* **Minigame modules and return values:** native dispatch maps0 to motorcycle,1 to chocobo,
  2 to snowboard,3 to Fort Condor,4 to submarine,5 to jet/coaster and6 to the other snowboard
  mode. Their mere presence in shared fields does not establish Disc 1 availability. Each
  selected branch needs original entry state, actual play/results, rewards/health and durable
  return. CPR, squats, parade and other field-script games additionally need live key and
  geometry sampling; they cannot be found by scanning `MINIGAME` alone.
  [Native world/minigame dispatch](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_main.c#L264)
* **Canon party and ordinary resources:** recruited membership, active slots, temporary
  solo groups, affection, HP/MP, equipment/materia, key items, GP and battle points have
  distinct lifetimes. They cannot share the co-op roster or be collapsed into the current
  gil balance. `MENU15/16` dispatch the original stolen-materia and restoration operations;
  their ledger and re-equipping effects must survive saving and reconnecting.
  [Native menu commands](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/include/game.h#L243)
* **Battle outcomes and chains:** preserve authored modes, escape/defeat branches, canon
  starting parties, formation layouts, native chained encounters, rewards and ordinary-field
  health. Formation460→461 proves that one field `BATTLE` can require multiple formations.
  Metadata is not evidence that their AI, attacks or presentation are implemented.
* **Field runtime and presentation:** retain scratch for a visit, persistent writes across
  visits, parallel scripts, key edges, engine clocks, dynamic triangle/line/collision state,
  timers and menu results. Palette-script animation during dialogue and other ambient work
  need the correct scheduler, not only renderer support. Movies require media, camera/frame
  synchronization, concurrent actors/scripts and audio, including handoff/reconnect behavior.

The inspected source snapshot contains no movie, movie-camera or minigame media. Numeric
movie sites are original evidence; names such as `mainplr`, `earithdd` and `funeral` come
from a pinned editor mapping and do not verify an owned PC container or playback.
[Movie mapping provenance](https://github.com/myst6re/makoureactor/blob/2452025714c033d698d1754a776bdcd5713427bd/src/Data.cpp#L607)

## How a chapter becomes accepted

For each candidate, establish original entry roots and fully known inputs from actual
initialization/writers. Keep legacy/imported unknown values unknown. Exercise every authored
choice and reachable failure/retry path with explicit snapshots; retain optional branches
as separate tests rather than choosing a convenient canonical answer. Compare final party,
resources, world/vehicle state and placements as well as the story moment.

Then test the continuous predecessor→chapter→successor route, including interruption and
relogin at windows, battles, menus, movies, minigames and pending transfers. Acknowledgments
must apply economic/party effects once; saved moment changes must not bypass unfinished actor
or mode work. Verify actual original media and original-engine behavior where static evidence
leaves timing or geometry uncertain. Add production registrations only after these runtime
requirements are met.

The manifest contains 36 chapter candidates and 442 associated field/maplist candidates,
including optional and unresolved contexts. All 1,218 pinned instruction references resolve
against the owned archive; all recorded operands match. JSON parsing and maplist/field-ID
checks also pass. Those checks validate this evidence index only.
They are not gameplay tests. No Cargo build, owned-asset modification, deployment or live-save
conversion was performed for this audit.

## Reproduce the instruction check

Build the small checker example, then supply your own original `flevel.lgp`:

```sh
cargo build -p fmv-ff7 --example manifest_check
python3 scripts/validate-disc1-manifest.py /path/to/owned/flevel.lgp
```

The wrapper reads the JSON and invokes the existing LGP/LZSS/field/script decoders through
the example. It never invokes Cargo itself, extracts assets or writes files. Use `--probe`
to select an example binary from another target directory, and `--manifest` to check an
alternate manifest. It checks each reference's field/maplist ID, entity/slot, offset,
opcode and every recorded operand bank/value. Source hashes, derived annotations and route
reachability are outside this check. Exit status is 0 for matches, 1 for mismatches and 2
for malformed input or a tool/decoder error.

The preserved tool was compiled directly against an existing parser library into a
temporary directory, without Cargo or shared-target writes: all 1,218 references matched.
A temporary altered operand and maplist ID returned status1 with two mismatches; an
inconsistent reference count returned status2.
