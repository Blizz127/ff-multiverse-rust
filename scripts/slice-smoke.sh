#!/usr/bin/env bash
# scripts/slice-smoke.sh - the foundations slice over REAL processes and REAL data (protocol v8).
#
# Usage: scripts/slice-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
#                               [--duration SECS] [--formation ID] [--keep-logs] [-h|--help]
#
# WHAT THE SLICE PROVES (one playable FF7 slice of the owner's "foundations first" priority):
#   (1) a persistent ORIGINAL player character with a saved identity (name, colour, body choice);
#   (2) multiplayer party membership (two players party up and fight one shared battle);
#   (3) a COMBAT-SLOT replacement separate from canon story roles: each player fights with a canon party
#       member's combat template (here Barret's and Tifa's) while its field look is a generic placeholder body
#       and the canon NPCs stay in the story (the canon Barret is still in the field the Barret-slot player walks);
#   (4) per-player story progress (the story blob of each account, see "STUBS" below for what is not on main yet);
#   (5) a world-specific appearance binding: how FF7 draws each character, stored per game and independent of the slot.
#
# WHAT IT DOES (real server and client processes, the owner's own library read-only, temp state only):
#   Session 1: the server hosts the real FF7 field md1stin. Two headless players REGISTER with different
#   identities and slots (the creation choices go over the wire in Hello::character):
#       A  "Aria Vale"  colour e8743c  body $BODY_A  slot Barret (1)   leader: invites, asks for the battle
#       B  "Brom"       colour 3c9be8  body $BODY_B  slot Tifa (2)     accepts
#   They form a party and fight ONE shared battle (formation 301 of md1stin's own table, a debug request) that
#   both win, each replaying it from its own scene.bin/kernel.bin ("verified by replay"). The run then checks:
#     * the Welcome card of each player says what was asked (name, colour, body, binding `ff7/body/<body>`, slot);
#     * each player SEES the other's binding (the placeholder body) and nothing in it is a canon model;
#     * the battle roster names the players, not the canon characters: the server's "battle started" line says
#       names=[Aria%20Vale,Brom] characters=[1, 2] (their names and their slot templates), the clients' own
#       `battle: roster` lines say the same, and the log lines of what they did carry name=<their own name>;
#     * the canon Barret NPC is still in the field for the Barret-slot player (the area's NPC list, from the
#       field data, has a character with Barret's skeleton: a canon character stays in the story as an NPC);
#     * the server saved progress for both under the SLOT's character (progression/<account>/Ff7.character.json
#       says character 1 and 2, never the look's character), and what the clients were told is what is stored.
#   Then the server is STOPPED (SIGTERM, exit 0) and every persisted record is snapshotted.
#   Session 2: a NEW server process on the same state directory and port. The same two players log in with their
#   saved tokens (and, on purpose, ask for another colour, body and slot: a token login ignores them). Before any
#   battle each must be told EXACTLY what it ended session 1 with: the same character card, the same level,
#   experience, gil and AP, the same inventory; and after stopping that server every persisted record
#   (characters/, avatars/, accounts/, progression/) is byte for byte what it was after session 1: a login
#   rewrites nothing. A second battle (same two players) then proves the restarted server fights them as their
#   slots (characters=[1, 2]) with their own names, at the levels and limit gauges session 1 saved, and the
#   record's identity, binding and slot are unchanged by it.
#
# EVERY PERSISTED FIELD that must survive the restart is checked in the stored files (read with jq) AND on the
# wire: identity (name, colour, body), binding, slot, the pointers to the progress records, level, experience, AP,
# gil, the limit gauge, the inventory, and the story blob.
#
# STUBS (clearly not proven by this script yet; they are printed as `STUB:` lines in the run):
#   * story advancement through sw-story's path: when the client has the headless flag --story-advance the players
#     advance the story once and the stored story blob must survive the restart too; until sw-story is merged
#     there is no such flag, the story blob of both accounts is only checked to be UNCHANGED (absent = new);
#   * an NPC talk through sw-dialog's path: TODO(sw-dialog) once its headless flag is on main: one player talks to
#     the canon Barret NPC and the dialogue must come out in Barret's own words, never with the player's name
#     swapped in. Not run now.
#
# Isolation (the owner's state directory is never touched, only its LIBRARY is read): the server and the clients
# run with XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR pointing into a
# throw-away directory, the server with a --state-dir of its own inside it (persistent across the restart; NOT
# --ephemeral), each client with its own --state-dir. The library is reached through a symlink
# `<work>/lib-state/library -> <STATE_DIR>/library` passed as --ff7-library, so nothing under STATE_DIR can be
# created or written. The script lists STATE_DIR before and after and fails if it changed. Needs jq.
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.
#
# This script reuses the helpers of scripts/smoke.sh and scripts/battle-smoke.sh (it sources them; sourcing
# runs nothing).

set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=battle-smoke.sh
source "$SELF_DIR/battle-smoke.sh"

# The sourced helpers print "battle-smoke.sh:"; this script says who it is.
say() { printf 'slice-smoke.sh: %s\n' "$*"; }
err() { printf 'slice-smoke.sh: error: %s\n' "$*" >&2; }

# Debug binaries by default (the other smokes default to release): `--release` asks for those.
PROFILE=debug
LIBRARY="${FMV_FF7_LIBRARY:-$HOME/.local/share/ff-multiverse}"
SLICE_DURATION=40
SESSION2_DURATION=8
FORMATION=301
TICK_RATE=240
BATTLE_SPEED=0

# The two characters. Bodies are ids of the FF7 placeholder bodies the server offers (fmv-ff7's PLACEHOLDER_BODIES);
# overridable if the table changes. The slots are canon party members' ids: Barret 1, Tifa 2.
NAME_A="Aria Vale"
NAME_B="Brom"
TINT_A=e8743c
TINT_B=3c9be8
BODY_A="${FMV_SMOKE_BODY_A:-sturdy}"
BODY_B="${FMV_SMOKE_BODY_B:-slight}"
SLOT_A=1
SLOT_B=2
SLOT_NAME_A=Barret
SLOT_NAME_B=Tifa
# What session 2 asks for instead (ignored by a token login, which is the point).
OTHER_TINT=101010
OTHER_BODY=lean
OTHER_SLOT=cid

usage() {
  cat <<'EOF'
Usage: scripts/slice-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
                              [--duration SECS] [--formation ID] [--keep-logs] [-h|--help]

The foundations slice over real processes and the owner's real FF7 data (protocol v8): two headless
players with different identities and combat slots register, party up and fight one shared battle on
md1stin; the server is stopped and started again as a new process on the same state directory; the same
two players log in and must be told exactly what they ended with, every persisted record must be byte for
byte unchanged, and a second battle must use what was saved. See the header of this script for every check
and for what is still a stub (the story advance of sw-story and the NPC talk of sw-dialog).

  --library DIR    state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                   ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS  how long each player stays in session 1 (default 40)
  --formation ID   the enemy formation of the debug battles (default 301)
  --debug          use <target>/debug binaries (the default of this script; the other smokes default to release)
  --release        use <target>/release binaries
  --build          cargo build fmv-server and fmv-client first, with the chosen profile
  --keep-logs      keep the log directory even when the run passes
EOF
}

# percent_encode <text>: the encoding of the clients' `name=` fields (letters, digits and - . _ ~ kept).
percent_encode() {
  local text="$1" out="" i ch
  for ((i = 0; i < ${#text}; i++)); do
    ch="${text:i:1}"
    case "$ch" in
      [A-Za-z0-9._~-]) out+="$ch" ;;
      *) out+=$(printf '%%%02X' "'$ch") ;;
    esac
  done
  printf '%s' "$out"
}

# rgb_of <rrggbb>: "r,g,b" as decimal numbers (what the record stores).
rgb_of() {
  printf '%d,%d,%d' "0x${1:0:2}" "0x${1:2:2}" "0x${1:4:2}"
}

# need_jq: the persisted-field checks read the records with jq.
need_jq() {
  command -v jq >/dev/null 2>&1 || {
    err "jq is needed to read the persisted records (apt install jq)"
    exit 1
  }
}

# character_file <server state dir> <name>: the path of the player-character record whose identity.name is <name>.
character_file() {
  local dir="$1" name="$2" f
  for f in "$dir"/characters/*.json; do
    [[ -f $f ]] || continue
    if [[ $(jq -r '.identity.name' "$f") == "$name" ]]; then
      printf '%s' "$f"
      return 0
    fi
  done
  return 1
}

# snapshot_records <server state dir> <dest>: copy every record directory (not index.log, not LOCK) and list the
# sha256 of each file in <dest>/SUMS, sorted by path.
snapshot_records() {
  local src="$1" dest="$2" d
  mkdir -p -- "$dest"
  for d in accounts avatars characters progression; do
    [[ -d $src/$d ]] && cp -a -- "$src/$d" "$dest/$d"
  done
  (cd -- "$dest" && find accounts avatars characters progression -type f 2>/dev/null | LC_ALL=C sort | while IFS= read -r f; do sha256sum -- "$f"; done) >"$dest/SUMS"
}

# line_field <log> <prefix> <key>: the key=value of the LAST line of <log> that contains <prefix>.
line_field() {
  local line
  line=$(last_line_with "$1" "$2") || return 0
  field "$(strip_ansi "$line")" "$3"
}

# first_field <log> <prefix> <key>: the same for the FIRST such line.
first_field() {
  local line
  line=$(first_line_with "$1" "$2") || return 0
  field "$(strip_ansi "$line")" "$3"
}

# start_slice_server <port> <log>: start the server on 127.0.0.1:<port> (0: any) with the persistent state directory
# and wait for READY (ADDR, CONNECT and SERVER_PID are set).
start_slice_server() {
  local port="$1" log="$2"
  local -a args=(--bind "127.0.0.1:$port" --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state"
    --debug-encounters --no-random-encounters --battle-speed "$BATTLE_SPEED" --battle-tick-rate "$TICK_RATE"
    --debug-formation "$FORMATION")
  say "starting server: $SERVER_BIN ${args[*]}"
  "$SERVER_BIN" "${args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"
}

# start_player <label A|B> <session> <duration> <extra client args...>: a headless player with its own state dir.
start_player() {
  local label="$1" session="$2" duration="$3"
  shift 3
  local name
  if [[ $label == A ]]; then
    name="$NAME_A"
  else
    name="$NAME_B"
  fi
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration "$duration" --bot idle --ff7-library "$WORK/lib-state" \
    --name "$name" --state-dir "$WORK/client-$label" "$@" >"$WORK/s$session-client-$label.log" 2>&1 &
  CPID[$label]=$!
}

# judge_exit <session> <label>: record a problem unless the client ended with status 0 (not killed).
judge_exit() {
  local session="$1" n="$2"
  if [[ ${CRC[$n]:-} == timeout ]]; then
    problem "session $session: client $n was still running after its duration plus ${CLIENT_GRACE}s and was killed"
  elif [[ ${CRC[$n]:-} != 0 ]]; then
    problem "session $session: client $n exited with status ${CRC[$n]:-unknown}"
  fi
}

# expect_equal <what> <got> <want>: a problem unless they are equal.
expect_equal() {
  [[ $2 == "$3" ]] || problem "$1: got '$2', expected '$3'"
}

# card_of <client log>: the `character:` line of a client (its Welcome card), ANSI stripped.
card_of() {
  local line
  line=$(first_line_with "$1" "character: name=") || return 0
  strip_ansi "$line"
}

# check_card <session> <label> <log> <name> <tint> <body> <slot> <slot name> <migrated>: the Welcome card says what is
# stored: the identity that was asked for, the binding of the body (a generic placeholder, never a canon model) and
# the slot.
check_card() {
  local session="$1" n="$2" log="$3" name="$4" tint="$5" body="$6" slot="$7" slot_name="$8"
  local card tag="session $session client $n"
  card=$(card_of "$log")
  if [[ -z $card ]]; then
    problem "$tag printed no 'character:' line (its Welcome card)"
    return 0
  fi
  expect_equal "$tag card name" "$(field "$card" name)" "$(percent_encode "$name")"
  expect_equal "$tag card tint" "$(field "$card" tint)" "$tint"
  expect_equal "$tag card body" "$(field "$card" body)" "$body"
  expect_equal "$tag card binding" "$(field "$card" binding)" "ff7/body/$body"
  expect_equal "$tag card slot" "$(field "$card" slot)" "$slot"
  expect_equal "$tag card slot name" "$(field "$card" slot_name)" "$slot_name"
  expect_equal "$tag card migrated" "$(field "$card" migrated)" "false"
}

# check_stored_character <label> <name> <tint> <body> <slot>: the player-character record on disk says what was asked
# for, with the pointers to the progress records, and the progress record is filed under the slot's character.
check_stored_character() {
  local n="$1" name="$2" tint="$3" body="$4" slot="$5" file rgb
  file=$(character_file "$WORK/server-state" "$name") || {
    problem "no player-character record for '$name' in $WORK/server-state/characters"
    return 0
  }
  rgb=$(rgb_of "$tint")
  expect_equal "stored $n identity.tint" "$(jq -c '.identity.tint' "$file")" "[$rgb]"
  expect_equal "stored $n identity.body" "$(jq -r '.identity.body' "$file")" "$body"
  expect_equal "stored $n origin" "$(jq -r '.origin' "$file")" "created"
  expect_equal "stored $n games.Ff7.slot" "$(jq -r '.games.Ff7.slot' "$file")" "$slot"
  expect_equal "stored $n games.Ff7.binding" "$(jq -r '.games.Ff7.binding' "$file")" "ff7/body/$body"
  expect_equal "stored $n pointer to the progress record" \
    "$(jq -c '.games.Ff7.progress[] | select(.record=="character")' "$file")" \
    "{\"record\":\"character\",\"game\":\"Ff7\",\"character\":$slot}"
  expect_equal "stored $n pointer to the story" "$(jq -c '.games.Ff7.progress[] | select(.record=="story")' "$file")" '{"record":"story","game":"Ff7"}'
  # The progress record of the account: filed where it always was, and naming the SLOT's character.
  local account progress
  account=$(basename -- "$file" .json)
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  if [[ ! -f $progress ]]; then
    problem "stored $n: no progress record $progress (the battle won must have paid it)"
    return 0
  fi
  expect_equal "stored $n progress character (the slot, never the look's character)" "$(jq -r '.character' "$progress")" "$slot"
}

# progress_of <client log> <first|last>: "level exp gil ap" from that `progress:` note ("" if none).
progress_numbers() {
  local log="$1" which="$2" line
  if [[ $which == first ]]; then
    line=$(first_line_with "$log" "progress: ") || return 0
  else
    line=$(last_line_with "$log" "progress: ") || return 0
  fi
  line=$(strip_ansi "$line")
  printf '%s %s %s %s' "$(field "$line" level)" "$(field "$line" exp)" "$(field "$line" gil)" "$(field "$line" ap)"
}

# stored_numbers <label> <name>: "level exp gil ap limit" read from the stored progress record.
stored_numbers() {
  local name="$1" file account progress
  file=$(character_file "$WORK/server-state" "$name") || return 0
  account=$(basename -- "$file" .json)
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  [[ -f $progress ]] || return 0
  jq -r '[.level, .exp, .gil, .ap, (.limit // 0)] | map(tostring) | join(" ")' "$progress"
}

# sorted_words <words...>: the numbers, ascending, space separated (the order the smoke helpers print levels and
# limits in: the two players may be seated in either order).
sorted_words() {
  local sorted
  sorted=$(printf '%s\n' "$@" | sort -n | tr '\n' ' ')
  printf '%s' "${sorted% }"
}

# stored_items <name>: the stored inventory as "item:count,..." in the order stored ("-" when empty).
stored_items() {
  local name="$1" file account progress
  file=$(character_file "$WORK/server-state" "$name") || return 0
  account=$(basename -- "$file" .json)
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  [[ -f $progress ]] || return 0
  jq -r '(.inventory // []) | if length == 0 then "-" else map("\(.item):\(.count)") | join(",") end' "$progress"
}

# npc_lines <client log> <area>: the NPCs a client reported for <area> (`headless: npc area=<area> id=<n> name=<enc>
# model=<stem>`), one `name=.. model=..` per NPC in id order, joined with `;` (empty if none): the last report of each id
# (an NPC set is reported again whenever it changes).
npc_lines() {
  local log="$1" area="$2" line id out="" row
  local -A seen=()
  local -a ids=()
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line == *"headless: npc area=$area "* ]] || continue
    line=$(strip_ansi "$line")
    id=$(field "$line" id)
    [[ -n $id ]] || continue
    [[ -n ${seen[$id]:-} ]] || ids+=("$id")
    seen[$id]="name=$(field "$line" name) model=$(field "$line" model)"
  done <"$log"
  for id in "${ids[@]}"; do
    row="${seen[$id]}"
    out+="$row;"
  done
  printf '%s' "$out"
}

# last_items <client log>: the items= of the client's last `inventory:` note.
last_items() {
  line_field "$1" "inventory: stacks=" items
}

run_slice() {
  need_jq
  local state_real="$LIBRARY"
  [[ -d $state_real/library/sets ]] || {
    err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
    exit 1
  }
  local before after
  before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')

  mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A" "$WORK/client-B"
  ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
  export FMV_STATE_DIR="$WORK/state-env"

  local enc_a enc_b n
  enc_a=$(percent_encode "$NAME_A")
  enc_b=$(percent_encode "$NAME_B")

  # ------------------------------------------------------------------------------------------------------------
  # Session 1: register, party up, fight one shared battle.
  # ------------------------------------------------------------------------------------------------------------
  start_slice_server 0 "$WORK/s1-server.log"
  local port="${ADDR##*:}"
  say "session 1: players A ($NAME_A, body $BODY_A, slot $SLOT_NAME_A) and B ($NAME_B, body $BODY_B, slot $SLOT_NAME_B) register and fight (${SLICE_DURATION}s)"
  local -a story_args=()
  local client_help
  client_help=$("$CLIENT_BIN" --help 2>&1 || true)
  if [[ $client_help == *"--story-advance"* ]]; then
    story_args=(--story-advance)
    say "the client has --story-advance: both players advance the story once (sw-story's path)"
  else
    say "STUB: story advancement through sw-story's path is not on this branch (no --story-advance flag): the story blob is only checked for being unchanged"
  fi
  say "STUB: NPC talk through sw-dialog's path: TODO(sw-dialog), not run until its headless flag is on main"
  start_player A 1 "$SLICE_DURATION" --tint "$TINT_A" --body "$BODY_A" --slot "$SLOT_A" \
    --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party 2 "${story_args[@]}"
  start_player B 1 "$SLICE_DURATION" --tint "$TINT_B" --body "$BODY_B" --slot "$SLOT_B" \
    --party-accept --battle-auto "${story_args[@]}"
  wait_clients $((SLICE_DURATION + CLIENT_GRACE))

  local la="$WORK/s1-client-A.log" lb="$WORK/s1-client-B.log" sl="$WORK/s1-server.log"
  for n in A B; do
    judge_exit 1 "$n"
  done
  # What the Welcome said, and what is stored.
  check_card 1 A "$la" "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A" "$SLOT_NAME_A"
  check_card 1 B "$lb" "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B" "$SLOT_NAME_B"
  log_has "$sl" "slot=$SLOT_A binding=ff7/body/$BODY_A migrated=false" || problem "the server never logged player A joining with slot $SLOT_A and binding ff7/body/$BODY_A"
  log_has "$sl" "slot=$SLOT_B binding=ff7/body/$BODY_B migrated=false" || problem "the server never logged player B joining with slot $SLOT_B and binding ff7/body/$BODY_B"

  # Each player sees the OTHER's binding: a placeholder body in the other's colour, never a canon model.
  local peer
  peer=$(last_line_with "$la" "headless: peer " || true)
  peer=$(strip_ansi "$peer")
  expect_equal "A sees B's name" "$(field "$peer" name)" "$enc_b"
  expect_equal "A sees B's binding" "$(field "$peer" binding)" "ff7/body/$BODY_B"
  expect_equal "A sees B's colour" "$(field "$peer" tint)" "$TINT_B"
  peer=$(last_line_with "$lb" "headless: peer " || true)
  peer=$(strip_ansi "$peer")
  expect_equal "B sees A's name" "$(field "$peer" name)" "$enc_a"
  expect_equal "B sees A's binding" "$(field "$peer" binding)" "ff7/body/$BODY_A"
  expect_equal "B sees A's colour" "$(field "$peer" tint)" "$TINT_A"
  local look
  for look in "$(line_field "$la" 'headless: peer ' binding)" "$(line_field "$lb" 'headless: peer ' binding)"; do
    [[ $look == ff7/body/* ]] || problem "a player was shown to the other as '$look', which is not a placeholder body (a canon look would make everybody a canon character)"
  done

  # The shared battle: both, the same one, won, verified by replay.
  local value ids=() smoke_line
  SMOKE_LINES=()
  for n in A B; do
    local log="$WORK/s1-client-$n.log"
    if smoke_line=$(last_line_with "$log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      for value in "party=2" "battles=1" "victories=1"; do
        [[ $smoke_line == *" $value"* ]] || problem "session 1 client $n: the SMOKE line lacks $value: $smoke_line"
      done
    else
      problem "session 1 client $n printed no SMOKE line"
    fi
    log_has "$log" "ended in Victory" || problem "session 1 client $n never saw the battle end in Victory"
    if log_has "$log" "DIVERGENCE"; then
      problem "session 1 client $n reported a BATTLE DIVERGENCE"
    fi
    check_mirror "$n" "$log" 1
    ids+=("$(battle_id_of "$log" || true)")
  done
  [[ -n ${ids[0]} && ${ids[0]} == "${ids[1]}" ]] || problem "the players did not enter the same battle (A: '${ids[0]}', B: '${ids[1]}')"

  # The roster: their own names and their slot templates, on the server and in each client.
  local started
  started=$(first_line_with "$sl" "battle started" || true)
  started=$(strip_ansi "$started")
  [[ $started == *"members=2"* ]] || problem "the server's battle did not have two members: $started"
  expect_equal "the server's roster names (battle started names=[..])" "$(field "$started" names)" "[$enc_a,$enc_b]"
  [[ $started == *"characters=[$SLOT_A, $SLOT_B]"* ]] || problem "the server's battle did not use the slot templates [$SLOT_A, $SLOT_B] (Barret, Tifa): $started"
  for n in A B; do
    local roster
    roster=$(first_line_with "$WORK/s1-client-$n.log" "battle: roster actor=0" || true)
    [[ $roster == *"name=$enc_a"* && $roster == *"slot=$SLOT_A"* && $roster == *"slot_name=$SLOT_NAME_A"* ]] || problem "client $n's roster line for actor 0 does not say $NAME_A / slot $SLOT_A: $roster"
    roster=$(first_line_with "$WORK/s1-client-$n.log" "battle: roster actor=1" || true)
    [[ $roster == *"name=$enc_b"* && $roster == *"slot=$SLOT_B"* && $roster == *"slot_name=$SLOT_NAME_B"* ]] || problem "client $n's roster line for actor 1 does not say $NAME_B / slot $SLOT_B: $roster"
  done
  # What a player did is logged under its own name, whatever template it fights with (the bots only attack, and a
  # defeated enemy needs no more: the lines exist when a bot defended, cast or used a limit, so this is a soft check).
  local acted="" what
  for what in "defend used" "magic used" "limit used"; do
    acted=$(last_line_with "$sl" "$what" || true)
    [[ -z $acted ]] && continue
    acted=$(strip_ansi "$acted")
    [[ $acted == *" name=$enc_a"* || $acted == *" name=$enc_b"* ]] || problem "a battle action line does not name the player: $acted"
  done

  # The canon Barret is still a character of the field (an NPC: the story keeps its canon characters), seen with
  # Barret's own skeleton, for the player who holds Barret's slot AND for the one who holds Tifa's: everybody sees
  # the same canon cast, and nobody is drawn as the canon character whose slot they hold.
  local npcs_a npcs_b
  npcs_a=$(npc_lines "$la" Ff7/md1stin)
  npcs_b=$(npc_lines "$lb" Ff7/md1stin)
  if [[ -z $npcs_a ]]; then
    problem "client A (Barret's slot) reported no NPC of md1stin at all (it prints 'headless: npc area=Ff7/md1stin ...' lines)"
  else
    [[ $npcs_a == *"name=Barret model=acgd"* ]] || problem "the Barret-slot player's md1stin has no canon Barret NPC (name=Barret model=acgd): $npcs_a"
  fi
  expect_equal "A (Barret's slot) and B (Tifa's slot) see the same NPCs in md1stin" "$npcs_a" "$npcs_b"
  [[ $npcs_a == *"name=gu0 "* && $npcs_a == *"name=gu1 "* ]] || problem "the guards of md1stin are not in the NPC list any more: $npcs_a"

  stop_server
  # Stopping the server saved everything queued: both payments are in the store.
  (($(count_lines_with "$sl" "progress saved account=") >= 2)) || problem "the server did not save the progress of both members"
  log_has "$sl" "progress saved account=" && {
    log_has "$sl" "character=$SLOT_A level=" || problem "no 'progress saved' line carries character=$SLOT_A (Barret's slot)"
    log_has "$sl" "character=$SLOT_B level=" || problem "no 'progress saved' line carries character=$SLOT_B (Tifa's slot)"
  }

  # What is stored is what was asked for, and the progress the clients were told is what is stored.
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  local told stored
  for n in A B; do
    local name="$NAME_A"
    [[ $n == B ]] && name="$NAME_B"
    told=$(progress_numbers "$WORK/s1-client-$n.log" last)
    stored=$(stored_numbers "$name")
    # told is "level exp gil ap", stored is "level exp gil ap limit": compare the first four.
    expect_equal "session 1 $n: what the server last told it (level exp gil ap) is what is stored" "$told" "${stored% *}"
    expect_equal "session 1 $n: the items it was told it carries are the stored ones" "$(last_items "$WORK/s1-client-$n.log")" "$(stored_items "$name")"
  done
  # Snapshot every persisted record (not the index log: it records each claim, which a login may add).
  snapshot_records "$WORK/server-state" "$WORK/snap1"
  say "snapshotted $(wc -l <"$WORK/snap1/SUMS") persisted file(s) after session 1"
  local stored_a stored_b items_a items_b
  stored_a=$(stored_numbers "$NAME_A")
  stored_b=$(stored_numbers "$NAME_B")
  items_a=$(stored_items "$NAME_A")
  items_b=$(stored_items "$NAME_B")

  # ------------------------------------------------------------------------------------------------------------
  # Session 2: a NEW server process on the same state directory and port; login only, then one more battle.
  # ------------------------------------------------------------------------------------------------------------
  CRC=()
  start_slice_server "$port" "$WORK/s2-server.log"
  [[ ${ADDR##*:} == "$port" ]] || fail "the restarted server is not on port $port (the clients' saved tokens are keyed by the address): $READY_LINE"
  say "session 2 (login only, ${SESSION2_DURATION}s): the same players, asking for another colour, body and slot (a token login ignores them)"
  start_player A 2 "$SESSION2_DURATION" --tint "$OTHER_TINT" --body "$OTHER_BODY" --slot "$OTHER_SLOT" --party-invite-first-peer
  start_player B 2 "$SESSION2_DURATION" --tint "$OTHER_TINT" --body "$OTHER_BODY" --slot "$OTHER_SLOT" --party-accept
  wait_clients $((SESSION2_DURATION + CLIENT_GRACE))
  local l2a="$WORK/s2-client-A.log" l2b="$WORK/s2-client-B.log" s2="$WORK/s2-server.log"
  for n in A B; do
    judge_exit 2 "$n"
  done
  # The card is the same as session 1's, field for field (and so is what was asked for at registration).
  expect_equal "A's card after the restart" "$(card_of "$l2a")" "$(card_of "$la")"
  expect_equal "B's card after the restart" "$(card_of "$l2b")" "$(card_of "$lb")"
  check_card 2 A "$l2a" "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A" "$SLOT_NAME_A"
  check_card 2 B "$l2b" "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B" "$SLOT_NAME_B"
  # Told on joining exactly what session 1 ended with.
  for n in A B; do
    local joined last name="$NAME_A"
    [[ $n == B ]] && name="$NAME_B"
    joined=$(progress_numbers "$WORK/s2-client-$n.log" first)
    last=$(progress_numbers "$WORK/s1-client-$n.log" last)
    expect_equal "session 2 $n: told on joining (level exp gil ap) what session 1 ended with" "$joined" "$last"
    expect_equal "session 2 $n: told on joining the inventory session 1 ended with" "$(first_field "$WORK/s2-client-$n.log" "inventory: stacks=" items)" "$(last_items "$WORK/s1-client-$n.log")"
  done
  stop_server
  # A login rewrites nothing: every persisted record is byte for byte what session 1 left.
  snapshot_records "$WORK/server-state" "$WORK/snap2"
  if ! diff -u -- "$WORK/snap1/SUMS" "$WORK/snap2/SUMS" >"$WORK/records.diff"; then
    problem "the persisted records changed during a login-only session (see $WORK/records.diff): $(head -c 600 "$WORK/records.diff" | tr '\n' ' ')"
  fi
  # Nothing of the identity, binding, slot or progress changed on disk either (read, not just hashed).
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  expect_equal "A's stored numbers after the restart (level exp gil ap limit)" "$(stored_numbers "$NAME_A")" "$stored_a"
  expect_equal "B's stored numbers after the restart (level exp gil ap limit)" "$(stored_numbers "$NAME_B")" "$stored_b"
  expect_equal "A's stored items after the restart" "$(stored_items "$NAME_A")" "$items_a"
  expect_equal "B's stored items after the restart" "$(stored_items "$NAME_B")" "$items_b"
  say "session 2: login-only records identical to session 1 ($(wc -l <"$WORK/snap2/SUMS") file(s)); character cards, progress and inventory told on joining equal the ones session 1 ended with"

  # Session 2b: the restarted server fights them with what was saved.
  CRC=()
  start_slice_server "$port" "$WORK/s3-server.log"
  say "session 2b: the restarted server fights the same two players again (${SLICE_DURATION}s)"
  start_player A 3 "$SLICE_DURATION" --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party 2
  start_player B 3 "$SLICE_DURATION" --party-accept --battle-auto
  wait_clients $((SLICE_DURATION + CLIENT_GRACE))
  local l3a="$WORK/s3-client-A.log" l3b="$WORK/s3-client-B.log" s3="$WORK/s3-server.log"
  for n in A B; do
    judge_exit 3 "$n"
    check_mirror "$n" "$WORK/s3-client-$n.log" 1
    log_has "$WORK/s3-client-$n.log" "ended in Victory" || problem "session 2b client $n never saw the battle end in Victory"
  done
  local started2
  started2=$(first_line_with "$s3" "battle started" || true)
  started2=$(strip_ansi "$started2")
  expect_equal "the restarted server's roster names" "$(field "$started2" names)" "[$enc_a,$enc_b]"
  [[ $started2 == *"characters=[$SLOT_A, $SLOT_B]"* ]] || problem "after the restart the battle did not use the slot templates [$SLOT_A, $SLOT_B]: $started2"
  # The levels and limit gauges the players ended session 1 with (the stored record: level is the first number, the
  # limit gauge the fifth) are what the restarted server fights them with.
  local -a sa sb
  read -r -a sa <<<"$stored_a"
  read -r -a sb <<<"$stored_b"
  expect_equal "the restarted server fights them at the levels session 1 saved" "$(levels_of "$started2")" "$(sorted_words "${sa[0]}" "${sb[0]}")"
  expect_equal "the restarted server starts them with the limit gauges session 1 saved" "$(limits_of "$started2")" "$(sorted_words "${sa[4]}" "${sb[4]}")"
  stop_server
  # What the second battle changed is progress only: identity, binding, slot and the pointers are unchanged.
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  for n in A B; do
    local name="$NAME_A"
    [[ $n == B ]] && name="$NAME_B"
    local was now
    if [[ $n == A ]]; then
      was="$stored_a"
    else
      was="$stored_b"
    fi
    now=$(stored_numbers "$name")
    # The experience went up (a win pays) and the level never went down.
    read -r -a was_a <<<"$was"
    read -r -a now_a <<<"$now"
    ((${now_a[0]:-0} >= ${was_a[0]:-1})) || problem "session 2b $n: the level went DOWN ($was -> $now)"
    ((${now_a[1]:-0} > ${was_a[1]:-0})) || problem "session 2b $n: a won battle paid no experience ($was -> $now)"
  done

  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "session 1 client: $l"
  done
  say "session 1 server: $started"
  for n in A B; do
    say "session 1 client $n: $(card_of "$WORK/s1-client-$n.log")"
    say "session 2 client $n: $(card_of "$WORK/s2-client-$n.log")"
  done
  say "session 2b server: $started2"
  RESULT=pass
  say "PASS: two original characters (distinct identities, bodies and slots) partied up and won a shared battle; the roster named them, not their slot's canon characters; the canon Barret NPC was still in the field; after a REAL server restart every card, record, level, experience, gil, AP, inventory and limit gauge was exactly what it was, the records were byte for byte unchanged by a login, and the restarted server fought them as their slots; $state_real unchanged ($after). STUBs: story advance (sw-story) and NPC talk (sw-dialog) are not proven here."
}

main() {
  while (($#)); do
    case "$1" in
      --release) PROFILE=release ;;
      --debug) PROFILE=debug ;;
      --bin-dir)
        (($# >= 2)) || usage_error "--bin-dir needs a directory"
        BIN_DIR="$2"
        shift
        ;;
      --build) DO_BUILD=1 ;;
      --library)
        (($# >= 2)) || usage_error "--library needs a directory"
        LIBRARY="$2"
        shift
        ;;
      --duration)
        (($# >= 2)) || usage_error "--duration needs a number of seconds"
        SLICE_DURATION="$2"
        shift
        ;;
      --formation)
        (($# >= 2)) || usage_error "--formation needs a formation id"
        FORMATION="$2"
        shift
        ;;
      --keep-logs) KEEP_LOGS=1 ;;
      -h | --help)
        usage
        return 0
        ;;
      *) usage_error "unknown argument: $1" ;;
    esac
    shift
  done
  [[ $SLICE_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  [[ $FORMATION =~ ^[0-9]+$ ]] || usage_error "--formation must be a whole number"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-slice-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  run_slice
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
