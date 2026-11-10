#!/usr/bin/env bash
# scripts/slice-smoke.sh - the foundations slice over REAL processes and REAL data (protocol v10).
#
# Usage: scripts/slice-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
#                               [--duration SECS] [--formation ID] [--seed N] [--keep-logs] [-h|--help]
#
# WHAT THE SLICE PROVES (one playable FF7 slice of the owner's "foundations first" priority):
#   (1) a persistent ORIGINAL player character with a saved identity (name, colour, body choice);
#   (2) multiplayer party membership (two players party up and fight one shared battle);
#   (3) the canon party FIGHTS BESIDE the players and nobody is replaced (v9): every battle's party is the story's canon
#       party first, Cloud and Barret, commanded by the party leader's bot (the server's --companions-player-commanded, on by
#       default; by the SERVER with it off) with ordinary commands every client replays and verifies
#       (`companion acted battle=.. actor=.. name=Cloud command=..`), and then the players, each with the stat curve
#       (its "fighting style", here Barret's and Tifa's curves) of its character and its own body (a generic placeholder
#       body) in the field; the canon Barret is no longer a field NPC (he is in the party), and talking to the field's
#       characters is the canon dialogue, never name-swapped;
#   (4) per-player story progress (sw-story: the story blob of each account, and the phase it puts a player in);
#   (5) a world-specific appearance binding: how FF7 draws each character, stored per game and independent of the slot.
#
# WHAT IT DOES (real server and client processes, the owner's own library read-only, temp state only; the server
# runs with --no-random-encounters --debug-encounters and a pinned --battle-seed, so a battle happens when a bot asks):
#   Session 1, one server process: two headless players REGISTER with different identities and slots (the creation
#   choices go over the wire in Hello::character) on the real FF7 field md1stin:
#       A  "Aria Vale"  colour e8743c  body $BODY_A  slot Barret (1)   --story-advance, invites, asks for the battle
#       B  "Brom"       colour 3c9be8  body $BODY_B  slot Tifa (2)     accepts only after A's advance split them
#   A invites B while they still see each other, walks onto md1stin's story point and advances its story (the server
#   validates it from A's moves), which puts A in another phase instance than B; B then accepts and the party puts it
#   in A's instance; the party fights ONE shared battle (formation 12 of the game's own data, a debug request: md1stin's
#   own 301-303 end before both players have had a turn beside the canon pair) that
#   both win, each replaying it from its own scene.bin/kernel.bin ("verified by replay"). Both also talk to the
#   field's guard through the client's own conversation code. The run checks:
#     * the Welcome card of each player says what was asked (name, colour, body, binding `ff7/body/<body>`, slot);
#     * each player SEES the other's binding (the placeholder body) and nothing in it is a canon model;
#     * the battle roster is the canon party first and then the players: the server's "battle started" line says
#       names=[Cloud,Barret,Aria%20Vale,Brom] characters=[0, 1, 1, 2] companions=2 (the two canon members and the two
#       players' own names with their stat curves), the clients' own `battle: roster` lines say the same (each with its
#       controller, companion or player), and the log lines of what a player did carry name=<its own name>; the
#       companions acted (`companion acted` lines naming Cloud and Barret) through commands every client replayed, so
#       the battle verified by replay with 0 divergences, and the rewards went to the two players only;
#     * md1stin's field shows no canon Barret NPC for either player (he is in the party now), the guards are there,
#       and both players see the same NPCs; the guard's dialogue is the game's own text (origin=canon), identical for
#       both and without either player's name;
#     * story: exactly one advance (A's, 1->6), saved as 010600 in A's story blob, none stored for B; the view of each
#       goes together, apart, together again (the party follows its leader's phase) and the placements agree;
#     * the server saved progress for both under the stat curve's character (progression/<account>/Ff7.character.json
#       says character 1 and 2, never the look's character), and what the clients were told is what is stored (the
#       inventory included: the starting stock when nothing changed it, the stock plus the drops when something did).
#   Then the server is STOPPED (SIGTERM, exit 0) and every persisted record is snapshotted.
#   Session 2: a NEW server process on the same state directory and port. The same two players log in with their
#   saved tokens (and, on purpose, ask for another colour, body and slot: a token login ignores them). Before any
#   battle each must be told EXACTLY what it ended session 1 with: the same character card, the same level,
#   experience, gil and AP, the same inventory, A its story (`story: joined with progress 010600`) in the phase it
#   advanced to while B is in the default one, so they do NOT see each other; and after stopping that server every
#   persisted record (characters/, avatars/, accounts/, progression/) is byte for byte what it was after session 1:
#   a login rewrites nothing. Session 2b: B catches up (its own story advance) and the two fight again on the
#   restarted server: it fights them beside the canon Cloud and Barret (characters=[0, 1, 1, 2], companions first) with
#   their own names, at the levels and stored limit gauges session 1 left, and the identity, binding, slot and pointers of
#   both records are unchanged.
#
#   LEVEL AND STORED LIMIT GAUGE (the real-process carry-over check): a win pays about 8 of the ~250 experience a level
#   costs, so over real processes nobody would level up. Session 1's server therefore runs with its DEBUG flag
#   --debug-bonus-exp 900 (refused without --debug-encounters and on the default state directory, warned about at start-up,
#   logged on every use; it pays a made-up number through the ordinary save path), and the run checks that BOTH NEW
#   characters ended session 1 at a higher level than they joined at (told leveled_up=true, stored). Players have NO limit
#   break in protocol v9, so no battle fills or empties a player's limit gauge: what is proven instead is that a gauge an
#   account of before v9 holds is PRESERVED (the real accounts' are: the preview bot's is about 200). With the server
#   stopped the smoke writes a gauge into each stored record (200 and 120); the restarted server sends it in the roster (the
#   engine ignores it for a player), the battle leaves it alone (keep_limits saves a gauge only for a member that has a limit
#   break, so a stored one is never overwritten with the empty gauge the engine reports) and the stored numbers after the
#   battle and the restart are the same. A note in the output says so; if player limits are enabled later, the real
#   carry-over check returns. The restarted servers have no bonus, so a level that comes back can only be the saved one.
#
# EVERY PERSISTED FIELD that must survive the restart is checked in the stored files (read with jq) AND on the
# wire: identity (name, colour, body), binding, slot, the pointers to the progress records, level, experience, AP,
# gil, the limit gauge, the inventory, and the story blob.
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
SLICE_DURATION=60
SESSION2_DURATION=8
# Formation 12 of the game's own data (two enemies, escapable): the canon Cloud and Barret kill md1stin's own 301-303 in about
# a hundred ticks, before every player has had a turn, and this slice needs both players to act (see battle-smoke.sh).
FORMATION=12
TICK_RATE=240
BATTLE_SPEED=0
# The server's --battle-seed. What the formation drops under it depends on the formation: whenever a drop is made the chain
# "a drop is stored, survives the restart and is told again" runs for real, and when none is the inventory checks below
# hold with no drop (the stock told on joining).
SEED=5
AREA=Ff7/md1stin

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
# The made-up numbers of session 1 (the server's DEBUG flags --debug-bonus-exp and --debug-bonus-limit, which it only
# accepts with --debug-encounters and a state directory of its own): a win pays about 16 of the ~250 experience a level
# costs and these weak enemies rarely hurt anybody, so without them nobody levels up or fills a gauge over real
# processes and the carry-over of a LEVEL and of a LIMIT GAUGE across the restart would go unproven. Only session 1's
# server has them: the restarted servers do not, so what survives is what the first one SAVED.
BONUS_EXP=900
# The limit gauges the smoke WRITES into the stored records between session 1 and the restart (as an account that predates
# v9 and holds a gauge would have): players have no limit break in v9, so no battle can move them, and the check is that
# the stored gauge is preserved through a battle and a real restart.
SEEDED_LIMIT_A=200
SEEDED_LIMIT_B=120

usage() {
  cat <<'EOF'
Usage: scripts/slice-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
                              [--duration SECS] [--formation ID] [--seed N] [--keep-logs] [-h|--help]

The foundations slice over real processes and the owner's real FF7 data (protocol v10): two headless
players with different identities and fighting styles register, party up, advance one story and fight one shared
battle on md1stin beside the canon Cloud and Barret, whom the leader's bot commands (session 1's server runs with the DEBUG
bonus --debug-bonus-exp, so that both new characters level up); the server is stopped and started again as a new
process (without the bonus) on the same state directory; the same two players log in and must be told exactly
what they ended with, every persisted record must be byte for byte unchanged, and a second battle must use what
was saved, levels and stored limit gauges included (players have no limit break in v9: a stored gauge must be
preserved by the battle and the restart). See the header of this script for every check.

  --library DIR    state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                   ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS  how long each player stays in session 1 (default 60)
  --formation ID   the enemy formation of the debug battles (default 12: two enemies the canon party does not kill before
                   every player has acted)
  --seed N         the server's --battle-seed (default 5)
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

# percent_decode <text>: the inverse.
percent_decode() {
  local text="$1"
  printf '%b' "${text//%/\\x}"
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

# account_of <name>: the account id of the character called <name> (the file name of its record).
account_of() {
  local file
  file=$(character_file "$WORK/server-state" "$1") || return 0
  basename -- "$file" .json
}

# snapshot_records <server state dir> <dest>: copy every record directory (not index.log, not LOCK) and list the
# sha256 of each file in <dest>/SUMS, sorted by path.
snapshot_records() {
  local src="$1" dest="$2" d
  local -a present=()
  mkdir -p -- "$dest"
  for d in accounts avatars characters progression; do
    if [[ -d $src/$d ]]; then
      cp -a -- "$src/$d" "$dest/$d"
      present+=("$d")
    fi
  done
  : >"$dest/SUMS"
  if ((${#present[@]})); then
    (cd -- "$dest" && find "${present[@]}" -type f | LC_ALL=C sort | while IFS= read -r f; do sha256sum -- "$f"; done) >"$dest/SUMS"
  fi
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

# start_slice_server <port> <log> [extra server args...]: start the server on 127.0.0.1:<port> (0: any) with the
# persistent state directory and wait for READY (ADDR, CONNECT and SERVER_PID are set). The extra arguments are
# session 1's debug bonuses.
start_slice_server() {
  local port="$1" log="$2"
  shift 2
  local -a args=(--bind "127.0.0.1:$port" --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state"
    --debug-encounters --no-random-encounters --battle-speed "$BATTLE_SPEED" --battle-tick-rate "$TICK_RATE"
    --debug-formation "$FORMATION" --battle-seed "$SEED" "$@")
  say "starting server: $SERVER_BIN ${args[*]}"
  "$SERVER_BIN" "${args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == "$AREA" && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"
}

# start_player <label A|B> <run> <duration> <extra client args...>: a headless player with its own state dir; its log is
# $WORK/<run>-client-<label>.log.
start_player() {
  local label="$1" run="$2" duration="$3"
  shift 3
  local name
  if [[ $label == A ]]; then
    name="$NAME_A"
  else
    name="$NAME_B"
  fi
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration "$duration" --bot idle --ff7-library "$WORK/lib-state" \
    --name "$name" --state-dir "$WORK/client-$label" "$@" >"$WORK/$run-client-$label.log" 2>&1 &
  CPID[$label]=$!
}

# judge_exit <run> <label>: record a problem unless the client ended with status 0 (not killed).
judge_exit() {
  local run="$1" n="$2"
  if [[ ${CRC[$n]:-} == timeout ]]; then
    problem "$run: client $n was still running after its duration plus ${CLIENT_GRACE}s and was killed"
  elif [[ ${CRC[$n]:-} != 0 ]]; then
    problem "$run: client $n exited with status ${CRC[$n]:-unknown}"
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

# check_card <run> <label> <log> <name> <tint> <body> <slot> <slot name>: the Welcome card says what is stored: the
# identity that was asked for, the binding of the body (a generic placeholder, never a canon model) and the slot.
check_card() {
  local run="$1" n="$2" log="$3" name="$4" tint="$5" body="$6" slot="$7" slot_name="$8"
  local card tag="$run client $n"
  card=$(card_of "$log")
  if [[ -z $card ]]; then
    problem "$tag printed no 'character:' line (its Welcome card)"
    return 0
  fi
  expect_equal "$tag card name" "$(field "$card" name)" "$(percent_encode "$name")"
  expect_equal "$tag card tint" "$(field "$card" tint)" "$tint"
  expect_equal "$tag card body" "$(field "$card" body)" "$body"
  expect_equal "$tag card binding" "$(field "$card" binding)" "$(percent_encode "ff7/body/$body")"
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

# progress_numbers <client log> <first|last>: "level exp gil ap" from that `progress:` note ("" if none).
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

# stored_numbers <name>: "level exp gil ap limit" read from the stored progress record.
stored_numbers() {
  local name="$1" account progress
  account=$(account_of "$name")
  [[ -n $account ]] || return 0
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  [[ -f $progress ]] || return 0
  jq -r '[.level, .exp, .gil, .ap, (.limit // 0)] | map(tostring) | join(" ")' "$progress"
}

# reward_of <server log> <account>: "exp gil ap" the server's `battle reward battle=1 account=<n> exp=.. gil=.. ap=..`
# line says <account> was paid (nothing if there is no such line; a server process numbers its battles from 1).
reward_of() {
  local line
  line=$(first_line_with "$1" "battle reward battle=1 account=$2 ") || return 0
  line=$(strip_ansi "$line")
  printf '%s %s %s' "$(field "$line" exp)" "$(field "$line" gil)" "$(field "$line" ap)"
}

# check_paid <what> <server log> <account> <before> <after>: <after> is <before> plus what the battle paid. Both are
# "level exp gil ap ..." (the experience, gil and AP are the 2nd to 4th numbers): the arithmetic is exact, so a server
# that dropped or reset a saved record (or paid twice) cannot pass.
check_paid() {
  local what="$1" log="$2" account="$3" reward
  local -a b=() a=() r=()
  reward=$(reward_of "$log" "$account")
  if [[ -z $reward ]]; then
    problem "$what: the server logged no 'battle reward' line for account $account"
    return 0
  fi
  read -r -a b <<<"$4"
  read -r -a a <<<"$5"
  read -r -a r <<<"$reward"
  expect_equal "$what: experience, gil and AP are what the player had plus the battle's reward ($reward)" \
    "${a[1]:-?} ${a[2]:-?} ${a[3]:-?}" "$((${b[1]:-0} + ${r[0]:-0})) $((${b[2]:-0} + ${r[1]:-0})) $((${b[3]:-0} + ${r[2]:-0}))"
}

# stored_items <name>: the stored inventory as "item:count,..." ("-" when empty), or "none" when the record has no
# inventory key (nothing ever changed it: the account still carries the game's starting stock, which is not stored).
stored_items() {
  local name="$1" account progress
  account=$(account_of "$name")
  [[ -n $account ]] || return 0
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  [[ -f $progress ]] || return 0
  jq -r 'if has("inventory") then ((.inventory) | if length == 0 then "-" else map("\(.item):\(.count)") | join(",") end) else "none" end' "$progress"
}

# stored_story <name>: the hex of the story blob stored for the account ("" if there is none).
stored_story() {
  local name="$1" account file
  account=$(account_of "$name")
  [[ -n $account ]] || return 0
  file="$WORK/server-state/progression/$account/Ff7.json"
  [[ -f $file ]] || return 0
  jq -r '.blob' "$file"
}

# sorted_words <words...>: the numbers, ascending, space separated (the order the smoke helpers print levels and
# limits in: the two players may be seated in either order).
sorted_words() {
  local sorted
  sorted=$(printf '%s\n' "$@" | sort -n | tr '\n' ' ')
  printf '%s' "${sorted% }"
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

# text_digest <text>: "<characters> characters, sha256 <12 hex>" of the text. The game's own dialogue is game data: the
# smoke compares it and describes it this way, and never prints it.
text_digest() {
  local text="$1" sum
  sum=$(printf '%s' "$text" | sha256sum)
  printf '%d characters, sha256 %s' "${#text}" "${sum:0:12}"
}

# talk_text <client log> <npc>: the percent-decoded text of every window of the client's conversation with <npc>
# (`talk: npc=<npc> ... window=<n> origin=<canon|added> text=<enc>`), joined with " | " (empty if there was none).
talk_text() {
  local log="$1" npc="$2" line out="" text
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line == *"talk: npc=$npc "* && $line == *" text="* ]] || continue
    line=$(strip_ansi "$line")
    text=$(percent_decode "$(field "$line" text)")
    out+="${out:+ | }$text"
  done <"$log"
  printf '%s' "$out"
}

# roster_pairs <server "battle started" line>: the members' `name:slot` pairs from `names=[..]` and `characters=[..]`,
# sorted and space separated. The server seats the two players in an order of its own (the party's), so what is checked
# is which name goes with which slot template, never the seat.
roster_pairs() {
  local line names characters i
  local -a n=() c=() out=()
  line=$(strip_ansi "$1")
  [[ $line =~ names=\[([^]]*)\] ]] || return 0
  names="${BASH_REMATCH[1]}"
  [[ $line =~ characters=\[([0-9,\ ]*)\] ]] || return 0
  characters="${BASH_REMATCH[1]}"
  IFS=',' read -r -a n <<<"$names"
  IFS=', ' read -r -a c <<<"$characters"
  for ((i = 0; i < ${#n[@]}; i++)); do
    out+=("${n[$i]}:${c[$i]:-?}")
  done
  sorted_words_text "${out[@]}"
}

# seed_stored_gauge <name> <gauge>: write `"limit": <gauge>` into the stored progress record of <name>, as a record of an
# account that predates v9 and holds a gauge would be (a server stopped: nothing else touches the file).
seed_stored_gauge() {
  local name="$1" gauge="$2" account progress
  account=$(account_of "$name")
  [[ -n $account ]] || return 1
  progress="$WORK/server-state/progression/$account/Ff7.character.json"
  [[ -f $progress ]] || return 1
  jq --argjson g "$gauge" '.limit = $g' "$progress" >"$progress.seed" && mv -- "$progress.seed" "$progress"
}

# roster_controllers <client log>: the `name:controller` of every `battle: roster actor=<i>` line of the log (every battle
# it fought), sorted and space separated.
roster_controllers() {
  local log="$1" line
  local -a rows=()
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line == *"battle: roster actor="* ]] || continue
    line=$(strip_ansi "$line")
    rows+=("$(field "$line" name):$(field "$line" controller)")
  done <"$log"
  ((${#rows[@]})) || return 0
  sorted_words_text "${rows[@]}"
}

# roster_triples <client log>: the `name:slot:slot_name` of every `battle: roster actor=<i>` line of the log (every
# battle it fought), sorted and space separated.
roster_triples() {
  local log="$1" line out=""
  local -a rows=()
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line == *"battle: roster actor="* ]] || continue
    line=$(strip_ansi "$line")
    rows+=("$(field "$line" name):$(field "$line" slot):$(field "$line" slot_name)")
  done <"$log"
  ((${#rows[@]})) || return 0
  sorted_words_text "${rows[@]}"
}

# sorted_words_text <words...>: the words in byte order, space separated.
sorted_words_text() {
  local sorted
  sorted=$(printf '%s\n' "$@" | LC_ALL=C sort | tr '\n' ' ')
  printf '%s' "${sorted% }"
}

# last_items <client log>: the items= of the client's last `inventory:` note; first_items: of its first.
last_items() {
  line_field "$1" "inventory: stacks=" items
}
first_items() {
  first_field "$1" "inventory: stacks=" items
}

# in_view_of <client log>: the numbers of its `headless: peers in view: N` lines, space separated, in order.
in_view_of() {
  local log="$1" line seq=""
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"headless: peers in view: "* ]]; then
      line=$(strip_ansi "$line")
      seq+="${line##*peers in view: } "
    fi
  done <"$log"
  printf '%s' "${seq% }"
}

# has_in_order <sequence> <value>...: do the values occur in the sequence in this order (not necessarily adjacent)?
has_in_order() {
  local seq="$1" want
  shift
  local -a rest=()
  read -r -a rest <<<"$seq"
  local i=0
  for want in "$@"; do
    while ((i < ${#rest[@]})) && [[ ${rest[$i]} != "$want" ]]; do i=$((i + 1)); done
    ((i < ${#rest[@]})) || return 1
    i=$((i + 1))
  done
  return 0
}

# need_flag <client help> <flag>: this client build must have the flag (sw-story's --story-advance, sw-dialog's talk
# through --talk): the slice is not proven without them.
need_flag() {
  [[ $1 == *"$2"* ]] || fail "this fmv-client has no $2 flag: build from a tree that has sw-story and sw-dialog merged ($3)"
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

  local client_help
  client_help=$("$CLIENT_BIN" --help 2>&1 || true)
  need_flag "$client_help" "--story-advance" "sw-story"
  need_flag "$client_help" "--talk" "the headless talk of the slice"
  need_flag "$client_help" "--party-accept-after-split" "sw-story"

  local enc_a enc_b n CARRIED=""
  local -A LEVEL_UPS=()
  enc_a=$(percent_encode "$NAME_A")
  enc_b=$(percent_encode "$NAME_B")

  # ------------------------------------------------------------------------------------------------------------
  # Session 1: register, party up, advance one story, talk, fight one shared battle.
  # ------------------------------------------------------------------------------------------------------------
  start_slice_server 0 "$WORK/s1-server.log" --debug-bonus-exp "$BONUS_EXP"
  local port="${ADDR##*:}"
  say "session 1 (the server has the DEBUG experience bonus: +$BONUS_EXP experience per win): players A ($NAME_A, body $BODY_A, stat curve $SLOT_NAME_A's) and B ($NAME_B, body $BODY_B, stat curve $SLOT_NAME_B's) register, party up, advance a story and fight beside the canon Cloud and Barret (${SLICE_DURATION}s)"
  start_player A r1 "$SLICE_DURATION" --tint "$TINT_A" --body "$BODY_A" --slot "$SLOT_A" --talk gu0 \
    --story-advance --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party 2
  start_player B r1 "$SLICE_DURATION" --tint "$TINT_B" --body "$BODY_B" --slot "$SLOT_B" --talk gu0 \
    --party-accept --party-accept-after-split --battle-auto
  wait_clients $((SLICE_DURATION + CLIENT_GRACE))

  local la="$WORK/r1-client-A.log" lb="$WORK/r1-client-B.log" sl="$WORK/s1-server.log"
  for n in A B; do
    judge_exit r1 "$n"
  done
  # What the Welcome said, and what is stored.
  check_card r1 A "$la" "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A" "$SLOT_NAME_A"
  check_card r1 B "$lb" "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B" "$SLOT_NAME_B"
  log_has "$sl" "slot=$SLOT_A binding=ff7/body/$BODY_A migrated=false" || problem "the server never logged player A joining with slot $SLOT_A and binding ff7/body/$BODY_A"
  log_has "$sl" "slot=$SLOT_B binding=ff7/body/$BODY_B migrated=false" || problem "the server never logged player B joining with slot $SLOT_B and binding ff7/body/$BODY_B"

  # Each player sees the OTHER's binding: a placeholder body in the other's colour, never a canon model.
  local peer
  peer=$(last_line_with "$la" "headless: peer " || true)
  peer=$(strip_ansi "$peer")
  expect_equal "A sees B's name" "$(field "$peer" name)" "$enc_b"
  expect_equal "A sees B's binding" "$(field "$peer" binding)" "$(percent_encode "ff7/body/$BODY_B")"
  expect_equal "A sees B's colour" "$(field "$peer" tint)" "$TINT_B"
  peer=$(last_line_with "$lb" "headless: peer " || true)
  peer=$(strip_ansi "$peer")
  expect_equal "B sees A's name" "$(field "$peer" name)" "$enc_a"
  expect_equal "B sees A's binding" "$(field "$peer" binding)" "$(percent_encode "ff7/body/$BODY_A")"
  expect_equal "B sees A's colour" "$(field "$peer" tint)" "$TINT_A"
  local look
  for look in "$(line_field "$la" 'headless: peer ' binding)" "$(line_field "$lb" 'headless: peer ' binding)"; do
    [[ $(percent_decode "$look") == ff7/body/* ]] || problem "a player was shown to the other as '$look', which is not a placeholder body (a canon look would make everybody a canon character)"
  done

  # The story (sw-story's path): one advance, A's, saved; the phases split the players and the party joins them again.
  local advanced story_line account_a account_b
  account_a=$(account_of "$NAME_A")
  account_b=$(account_of "$NAME_B")
  advanced=$(count_lines_with "$sl" "story advanced ")
  [[ $advanced == 1 ]] || problem "the server logged $advanced 'story advanced' lines in session 1 (expected exactly 1: A's)"
  story_line=$(last_line_with "$sl" "story advanced " || true)
  story_line=$(strip_ansi "$story_line")
  expect_equal "the story advance is A's, at md1stin point 0, moment 1->6" "$(field "$story_line" account)|$(field "$story_line" area)|$(field "$story_line" point)|$(field "$story_line" moment)" "$account_a|$AREA|0|1->6"
  log_has "$la" "story: advanced point=0 moment=1->6" || problem "client A never logged 'story: advanced point=0 moment=1->6'"
  if log_has "$lb" "story: advanced"; then
    problem "client B logged a story advance (only A advances in session 1)"
  fi
  local seq_a seq_b
  seq_a=$(in_view_of "$la")
  seq_b=$(in_view_of "$lb")
  say "A: peers in view over time: $seq_a; B: $seq_b"
  has_in_order "$seq_a" 1 0 1 || problem "A's view should go 1, 0, 1 (together, apart after its advance, together in the party): '$seq_a'"
  has_in_order "$seq_b" 1 0 1 || problem "B's view should go 1, 0, 1: '$seq_b'"

  # The talk (sw-dialog's path, client-local): the guard says the game's own words, the same to both players and
  # without either player's name.
  local text_a text_b
  text_a=$(talk_text "$la" gu0)
  text_b=$(talk_text "$lb" gu0)
  local windows_a
  windows_a=$(count_lines_with "$la" "talk: npc=gu0 story=1 window=")
  say "A talks to gu0 (the game's own words, not printed): $windows_a window(s), $(text_digest "$text_a")"
  [[ -n $text_a ]] || problem "client A had no conversation with the guard gu0 ('talk: npc=gu0 ... text=' lines)"
  [[ $text_a == *Potion* ]] || problem "the guard's first words do not name the Potion it gives at the start of the game ($(text_digest "$text_a"))"
  [[ $text_a == "$text_b" ]] || problem "the guard says something else to the second player than to the first (A: $(text_digest "$text_a"); B: $(text_digest "$text_b"))"
  for n in "$NAME_A" "$NAME_B" Aria Brom Vale; do
    [[ $text_a != *"$n"* && $text_b != *"$n"* ]] || problem "the dialogue contains the player's name '$n' (dialogue is never name-swapped)"
  done
  # (a failure prints the line without its text: the game's own words are game data)
  local talk_line
  talk_line=$(first_line_with "$la" "talk: npc=gu0 " || echo none)
  [[ $talk_line == *"origin=canon"* ]] || problem "the guard's words are not the game's own (origin=canon): ${talk_line%% text=*}"

  # The shared battle: both, the same one, won, verified by replay.
  local value ids=() smoke_line
  SMOKE_LINES=()
  for n in A B; do
    local log="$WORK/r1-client-$n.log"
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

  # The roster: the canon party (Cloud and Barret, commanded by the leader's bot) first, then the two players with their own
  # names and their stat curves, on the server and in each client: nobody is replaced.
  local started
  started=$(first_line_with "$sl" "battle started" || true)
  started=$(strip_ansi "$started")
  [[ $started == *"members=2"* && $started == *"companions=2"* ]] || problem "the server's battle did not have two players and the two canon companions: $started"
  [[ $started == *"names=[Cloud,Barret,$enc_a,$enc_b]"* || $started == *"names=[Cloud,Barret,$enc_b,$enc_a]"* ]] || problem "the server's roster is not Cloud, Barret and then the two players: $started"
  [[ $started == *"characters=[0, 1, "* ]] || problem "the canon pair is not first in the server's roster (characters=[0, 1, ..]): $started"
  local want_pairs want_triples want_controllers
  want_pairs=$(sorted_words_text "Cloud:0" "Barret:1" "$enc_a:$SLOT_A" "$enc_b:$SLOT_B")
  want_triples=$(sorted_words_text "Cloud:0:Cloud" "Barret:1:Barret" "$enc_a:$SLOT_A:$SLOT_NAME_A" "$enc_b:$SLOT_B:$SLOT_NAME_B")
  want_controllers=$(sorted_words_text "Cloud:companion" "Barret:companion" "$enc_a:player" "$enc_b:player")
  expect_equal "the server's roster (battle started: Cloud and Barret, then each player's own name with its stat curve)" "$(roster_pairs "$started")" "$want_pairs"
  for n in A B; do
    expect_equal "client $n's roster lines (name, stat curve, its name, of every fighter)" "$(roster_triples "$WORK/r1-client-$n.log")" "$want_triples"
    expect_equal "client $n's roster lines (who commands each fighter)" "$(roster_controllers "$WORK/r1-client-$n.log")" "$want_controllers"
  done
  # The canon companions really fought, through commands the server applied and every client replayed (the battle is
  # "verified by replay" with 0 divergences above): both gave commands under their own names.
  local companion_name
  for companion_name in Cloud Barret; do
    (($(count_lines_with "$sl" "companion acted battle=1 actor=") >= 1)) && log_has "$sl" " name=$companion_name command=" || problem "the canon $companion_name never acted ('companion acted ... name=$companion_name command=..' in the server log)"
  done
  local ended_line
  ended_line=$(first_line_with "$sl" "battle ended" || true)
  ended_line=$(strip_ansi "$ended_line")
  [[ $(companions_of "$ended_line") == 2 ]] || problem "the server's battle-end line does not say companions=2: $ended_line"
  local -a companion_counts=()
  read -r -a companion_counts <<<"$(companion_commands_of "$ended_line")"
  ((${#companion_counts[@]} == 2 && companion_counts[0] + companion_counts[1] >= 2)) || problem "the canon companions gave fewer than two commands in all (commands=[..] of the end line: $ended_line)"
  local -a player_counts=()
  read -r -a player_counts <<<"$(commands_of "$ended_line")"
  ((${#player_counts[@]} == 2 && player_counts[0] >= 1 && player_counts[1] >= 1)) || problem "both players should have given a command in a fight beside the canon party (commands per player: ${player_counts[*]:-none}): $ended_line"
  # What a player did is logged under its own name, whatever template it fights with (the bots only attack, and a
  # defeated enemy needs no more: the lines exist when a bot defended, cast or used a limit, so this is a soft check).
  local acted="" what
  for what in "defend used" "magic used" "limit used"; do
    acted=$(last_line_with "$sl" "$what" || true)
    [[ -z $acted ]] && continue
    acted=$(strip_ansi "$acted")
    [[ $acted == *" name=$enc_a"* || $acted == *" name=$enc_b"* ]] || problem "a battle action line does not name the player: $acted"
  done

  # The canon Barret is in the PARTY now and no longer a character of the field (the field shows only the leader the player
  # controls): md1stin's NPCs are exactly the data's, the guards, the same for both players, with no Barret among them.
  local npcs_a npcs_b
  npcs_a=$(npc_lines "$la" "$AREA")
  npcs_b=$(npc_lines "$lb" "$AREA")
  if [[ -z $npcs_a ]]; then
    problem "client A reported no NPC of md1stin at all (it prints 'headless: npc area=$AREA ...' lines)"
  else
    [[ $npcs_a != *"name=Barret"* && $npcs_a != *"model=acgd"* ]] || problem "md1stin still shows a canon Barret NPC (he is in the battle party now): $npcs_a"
  fi
  expect_equal "A and B see the same NPCs in md1stin" "$npcs_a" "$npcs_b"
  [[ $npcs_a == *"name=gu0 "* && $npcs_a == *"name=gu1 "* ]] || problem "the guards of md1stin are not in the NPC list any more: $npcs_a"

  stop_server
  # The DEBUG experience bonus was loud (a warning at start-up, and every payment says so), and only this server had it.
  log_has "$sl" "DEBUG: --debug-bonus-exp $BONUS_EXP is on" || problem "session 1's server never warned about --debug-bonus-exp"
  (($(count_lines_with "$sl" "debug_bonus_exp=$BONUS_EXP") >= 2)) || problem "the server's 'battle reward' lines do not all say debug_bonus_exp=$BONUS_EXP (one per paid member)"
  # Players have no limit break (v9): the battle kept no gauge for anybody (a companion's is never stored).
  ! log_has "$sl" "limit kept battle=" || problem "the server kept a limit gauge although players have none and a companion's is never stored: $(last_line_with "$sl" "limit kept battle=")"
  # Stopping the server saved everything queued: both payments and A's story are in the store.
  (($(count_lines_with "$sl" "progress saved account=") >= 2)) || problem "the server did not save the progress of both members"
  if log_has "$sl" "progress saved account="; then
    log_has "$sl" "character=$SLOT_A level=" || problem "no 'progress saved' line carries character=$SLOT_A (Barret's slot)"
    log_has "$sl" "character=$SLOT_B level=" || problem "no 'progress saved' line carries character=$SLOT_B (Tifa's slot)"
  fi
  log_has "$sl" "story saved account=$account_a bytes=3 progress=010600" || problem "the server never logged 'story saved account=$account_a bytes=3 progress=010600'"

  # What is stored is what was asked for, and the progress the clients were told is what is stored.
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  expect_equal "A's stored story (the blob sw-story's advance wrote)" "$(stored_story "$NAME_A")" "010600"
  expect_equal "B's stored story (it never advanced: nothing is stored)" "$(stored_story "$NAME_B")" ""
  local told stored stock stored_inv
  for n in A B; do
    local name="$NAME_A"
    [[ $n == B ]] && name="$NAME_B"
    told=$(progress_numbers "$WORK/r1-client-$n.log" last)
    stored=$(stored_numbers "$name")
    # told is "level exp gil ap", stored is "level exp gil ap limit": compare the first four.
    expect_equal "session 1 $n: what the server last told it (level exp gil ap) is what is stored" "$told" "${stored% *}"
    # And it is exactly what the player started with (told on joining) plus what the battle paid.
    check_paid "session 1 $n" "$sl" "$(account_of "$name")" "$(progress_numbers "$WORK/r1-client-$n.log" first)" "$stored"
    # The DEBUG experience bonus made this NEW character level up in this one session: it joined at the level its stat curve
    # starts at, and what is stored now is higher (the carry-over of the level across the restart is checked below, against
    # exactly these stored numbers). Its stored limit gauge is empty: players have no limit break, nothing fills one.
    local join_numbers join_level stored_level stored_limit
    join_numbers=$(progress_numbers "$WORK/r1-client-$n.log" first)
    join_level=${join_numbers%% *}
    stored_level=${stored%% *}
    ((stored_level > join_level)) || problem "session 1 $n: no level up (told level $join_level on joining, stored level $stored_level): the DEBUG experience bonus did not reach the record"
    log_has "$WORK/r1-client-$n.log" "leveled_up=true" || problem "session 1 $n: the server never told it leveled_up=true"
    stored_limit=${stored##* }
    expect_equal "session 1 $n: the stored limit gauge (players have no limit break: nothing fills one)" "$stored_limit" "0"
    LEVEL_UPS[$n]="level $join_level -> $stored_level"
    # The inventory the server told is what is stored when the record holds one (a drop or an item used wrote it);
    # a record without the key means nothing changed it, so the player still carries the starting stock it was told
    # at its first join.
    stored_inv=$(stored_items "$name")
    if [[ $stored_inv == none ]]; then
      stock=$(first_items "$WORK/r1-client-$n.log")
      expect_equal "session 1 $n: no inventory stored, so the items told last are the starting stock told on joining" "$(last_items "$WORK/r1-client-$n.log")" "$stock"
    else
      expect_equal "session 1 $n: the items it was told it carries are the stored ones" "$(last_items "$WORK/r1-client-$n.log")" "$stored_inv"
    fi
  done

  # Players have no limit break in v9, so no battle can move a player's limit gauge and "a gauge filled by a battle carries
  # over a restart" cannot be shown over real processes while that stays so (owner default, pending his answer). What can
  # be shown is that a STORED gauge, one an account of before v9 holds, is preserved by a battle and by a real restart:
  # the server is stopped, so a gauge is written into both stored records (as such an account would have it), and the
  # restarted servers must tell it, fight with it on the wire and leave it exactly as it is.
  seed_stored_gauge "$NAME_A" "$SEEDED_LIMIT_A" || problem "could not write a stored limit gauge into A's progress record"
  seed_stored_gauge "$NAME_B" "$SEEDED_LIMIT_B" || problem "could not write a stored limit gauge into B's progress record"
  say "note: limit carry-over for players is moot while players have no Limit Break (owner default, pending his answer), so the check is 'stored gauge preserved': gauges $SEEDED_LIMIT_A and $SEEDED_LIMIT_B were written into the stored records and must come out of the battle and the real restart unchanged. If player limits are enabled later the real carry-over check returns."

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
  start_player A r2 "$SESSION2_DURATION" --tint "$OTHER_TINT" --body "$OTHER_BODY" --slot "$OTHER_SLOT"
  start_player B r2 "$SESSION2_DURATION" --tint "$OTHER_TINT" --body "$OTHER_BODY" --slot "$OTHER_SLOT"
  wait_clients $((SESSION2_DURATION + CLIENT_GRACE))
  local l2a="$WORK/r2-client-A.log" l2b="$WORK/r2-client-B.log" s2="$WORK/s2-server.log"
  for n in A B; do
    judge_exit r2 "$n"
  done
  # The card is the same as session 1's, field for field (and so is what was asked for at registration).
  expect_equal "A's card after the restart" "$(card_of "$l2a")" "$(card_of "$la")"
  expect_equal "B's card after the restart" "$(card_of "$l2b")" "$(card_of "$lb")"
  check_card r2 A "$l2a" "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A" "$SLOT_NAME_A"
  check_card r2 B "$l2b" "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B" "$SLOT_NAME_B"
  # Told on joining exactly what session 1 ended with.
  for n in A B; do
    local joined last
    joined=$(progress_numbers "$WORK/r2-client-$n.log" first)
    last=$(progress_numbers "$WORK/r1-client-$n.log" last)
    expect_equal "session 2 $n: told on joining (level exp gil ap) what session 1 ended with" "$joined" "$last"
    expect_equal "session 2 $n: told on joining the inventory session 1 ended with" "$(first_items "$WORK/r2-client-$n.log")" "$(last_items "$WORK/r1-client-$n.log")"
  done
  # The story persisted: A is told its story and is in the phase it advanced to, B is in the default phase, and they do
  # not see each other although both are in md1stin (the phases split them again).
  log_has "$l2a" "story: joined with progress 010600" || problem "client A was not told its story (progress 010600) when it logged in again"
  if log_has "$l2b" "story: "; then
    problem "client B was told a story: it never advanced: $(last_line_with "$l2b" "story: " || true)"
  fi
  for n in A B; do
    local sl2
    sl2=$(last_line_with "$WORK/r2-client-$n.log" "SMOKE name=" || true)
    sl2=$(strip_ansi "$sl2")
    expect_equal "session 2 $n: peers_seen (A is in the advanced phase, B in the default one: apart)" "$(field "$sl2" peers_seen)" "0"
  done
  stop_server
  # A login rewrites nothing: every persisted record is byte for byte what session 1 left.
  snapshot_records "$WORK/server-state" "$WORK/snap2"
  if ! diff -u -- "$WORK/snap1/SUMS" "$WORK/snap2/SUMS" >"$WORK/records.diff"; then
    problem "the persisted records changed during a login-only session (see $WORK/records.diff): $(head -c 600 "$WORK/records.diff" | tr '\n' ' ')"
  fi
  # Nothing of the identity, binding, slot, progress or story changed on disk either (read, not just hashed).
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  expect_equal "A's stored numbers after the restart (level exp gil ap limit)" "$(stored_numbers "$NAME_A")" "$stored_a"
  expect_equal "B's stored numbers after the restart (level exp gil ap limit)" "$(stored_numbers "$NAME_B")" "$stored_b"
  expect_equal "A's stored items after the restart" "$(stored_items "$NAME_A")" "$items_a"
  expect_equal "B's stored items after the restart" "$(stored_items "$NAME_B")" "$items_b"
  expect_equal "A's stored story after the restart" "$(stored_story "$NAME_A")" "010600"
  say "session 2: login-only records identical to session 1 ($(wc -l <"$WORK/snap2/SUMS") file(s)); character cards, progress, inventory and story told on joining equal the ones session 1 ended with"

  # Session 2b: B catches up with its own story advance (so the phases meet again and the two can party), and the
  # restarted server fights them with what was saved.
  CRC=()
  start_slice_server "$port" "$WORK/s3-server.log"
  say "session 2b: B advances its story, the party forms and the restarted server fights the same two players again (${SLICE_DURATION}s)"
  start_player A r3 "$SLICE_DURATION" --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party 2
  start_player B r3 "$SLICE_DURATION" --party-accept --battle-auto --story-advance
  wait_clients $((SLICE_DURATION + CLIENT_GRACE))
  local s3="$WORK/s3-server.log"
  for n in A B; do
    judge_exit r3 "$n"
    check_mirror "$n" "$WORK/r3-client-$n.log" 1
    log_has "$WORK/r3-client-$n.log" "ended in Victory" || problem "session 2b client $n never saw the battle end in Victory"
  done
  local started2
  started2=$(first_line_with "$s3" "battle started" || true)
  started2=$(strip_ansi "$started2")
  expect_equal "the restarted server's roster (Cloud and Barret, then each player's own name with its stat curve)" "$(roster_pairs "$started2")" "$want_pairs"
  [[ $started2 == *"members=2"* && $started2 == *"companions=2"* ]] || problem "the restarted server's battle did not have two players and the two canon companions: $started2"
  for n in A B; do
    expect_equal "after the restart client $n's roster lines" "$(roster_triples "$WORK/r3-client-$n.log")" "$want_triples"
    expect_equal "after the restart client $n's roster lines (who commands each fighter)" "$(roster_controllers "$WORK/r3-client-$n.log")" "$want_controllers"
  done
  # The levels and limit gauges the players ended session 1 with (the stored record: level is the first number, the
  # limit gauge the fifth) are what the restarted server fights them with.
  local -a sa sb
  read -r -a sa <<<"$stored_a"
  read -r -a sb <<<"$stored_b"
  expect_equal "the restarted server fights them at the levels session 1 saved" "$(levels_of "$started2")" "$(sorted_words "${sa[0]}" "${sb[0]}")"
  expect_equal "the restarted server starts them with the limit gauges they have stored (sent on the wire, ignored by the engine)" "$(limits_of "$started2")" "$(sorted_words "${sa[4]}" "${sb[4]}")"
  expect_equal "the stored gauges are the ones written between the sessions" "$(sorted_words "${sa[4]}" "${sb[4]}")" "$(sorted_words "$SEEDED_LIMIT_A" "$SEEDED_LIMIT_B")"
  # What that proves, this time: session 1's server (with the DEBUG experience bonus) made both characters LEVEL UP, checked
  # above against the stored record, and the restarted servers have no bonus: they cannot invent a level. So a restarted
  # server that ignored a saved level would fight them at their stat curve's starting level and fail the check above, and
  # this one says that the level that survived is not the starting one. The gauge was written between the sessions.
  local join_level_a join_level_b
  join_level_a=$(progress_numbers "$WORK/r1-client-A.log" first)
  join_level_b=$(progress_numbers "$WORK/r1-client-B.log" first)
  ((${sa[0]} > ${join_level_a%% *} && ${sb[0]} > ${join_level_b%% *})) ||
    problem "session 1 raised no level for both players (A ${join_level_a%% *} -> ${sa[0]}, B ${join_level_b%% *} -> ${sb[0]}): a restarted server that forgot the level could not be told apart"
  ((${sa[4]} > 0 && ${sb[4]} > 0)) || problem "no stored gauge for both players (A ${sa[4]}, B ${sb[4]}): a restarted server that forgot or rewrote the gauge could not be told apart"
  local server_log
  for server_log in "$WORK/s2-server.log" "$s3"; do
    ! log_has "$server_log" "DEBUG: --debug-bonus" || problem "a restarted server ran with a DEBUG bonus ($server_log): what survived the restart would not be proven"
  done
  CARRIED="A level ${join_level_a%% *} -> ${sa[0]} and stored gauge ${sa[4]} preserved, B level ${join_level_b%% *} -> ${sb[0]} and stored gauge ${sb[4]} preserved"
  say "carried over a REAL server restart (told and fought again by servers without the bonus): $CARRIED"
  stop_server
  # What the second battle changed is progress (and B's story); identity, binding, slot and pointers are unchanged.
  check_stored_character A "$NAME_A" "$TINT_A" "$BODY_A" "$SLOT_A"
  check_stored_character B "$NAME_B" "$TINT_B" "$BODY_B" "$SLOT_B"
  expect_equal "A's story is untouched by B's advance" "$(stored_story "$NAME_A")" "010600"
  expect_equal "B's story after its own advance" "$(stored_story "$NAME_B")" "010600"
  for n in A B; do
    local name="$NAME_A" was now
    [[ $n == B ]] && name="$NAME_B"
    if [[ $n == A ]]; then
      was="$stored_a"
    else
      was="$stored_b"
    fi
    now=$(stored_numbers "$name")
    # The experience went up (a win pays) and the level never went down.
    local -a was_a now_a
    read -r -a was_a <<<"$was"
    read -r -a now_a <<<"$now"
    ((${now_a[0]:-0} >= ${was_a[0]:-1})) || problem "session 2b $n: the level went DOWN ($was -> $now)"
    # The stored limit gauge (the fifth number) is untouched by the battle: players have no limit break, so nothing may
    # overwrite it with the empty gauge the engine reports.
    expect_equal "session 2b $n: the stored limit gauge is preserved by the battle" "${now_a[4]:-?}" "${was_a[4]:-?}"
    ((${now_a[1]:-0} > ${was_a[1]:-0})) || problem "session 2b $n: a won battle paid no experience ($was -> $now)"
    # Exactly what session 1 saved plus what the second battle paid: the restarted server started from the saved record.
    check_paid "session 2b $n" "$s3" "$(account_of "$name")" "$was" "$now"
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
    say "session 1 client $n: $(card_of "$WORK/r1-client-$n.log")"
    say "session 2 client $n: $(card_of "$WORK/r2-client-$n.log")"
  done
  say "session 1 NPCs of md1stin (the first player's list; the second player's is identical; no canon Barret among them): $npcs_a"
  say "session 1 guard talk: $windows_a window(s), origin=canon, the same text for both players ($(text_digest "$text_a"))"
  say "session 1 server end: $ended_line"
  say "session 2b server: $started2"
  for n in A B; do
    say "session 1 -> restart, player $n: ${LEVEL_UPS[$n]}"
  done
  RESULT=pass
  say "PASS: two original characters (distinct identities, bodies and stat curves) partied up, advanced a story and won a shared battle BESIDE the canon Cloud and Barret, whom the leader's bot commanded and every client replayed (0 divergences): the roster was Cloud, Barret and then the two players by their own names, nobody replaced, the rewards went to the players only and no canon Barret stands in the field any more; the dialogue was the game's own; session 1's server (with the DEBUG experience bonus) made both LEVEL UP and after a REAL server restart every card, record, level, experience, gil, AP, stored limit gauge, inventory and story was exactly what it was ($CARRIED), the records were byte for byte unchanged by a login, a second battle (on servers without the bonus) paid on top of what was saved to the point and started with the saved levels, and the restarted server fought them as their stat curves; $state_real unchanged ($after)."
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
      --seed)
        (($# >= 2)) || usage_error "--seed needs a whole number"
        SEED="$2"
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
  [[ $SEED =~ ^[0-9]+$ ]] || usage_error "--seed must be a whole number"
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
