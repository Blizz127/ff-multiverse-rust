#!/usr/bin/env bash
# scripts/travel-smoke.sh - area-travel smoke test for fmv-server + fmv-client.
#
# Usage: scripts/travel-smoke.sh [--fixture|--chain] [--release|--debug] [--bin-dir DIR] [--build]
#                                [--library STATE_DIR] [--duration SECS] [--keep-logs] [-h|--help]
#
# DEFAULT MODE (real data): the server hosts the real FF7 field md1stin from the owner's own imported
# library (read-only) and serves every other field its exits lead to (they are loaded when the first
# player walks there). Two headless bots form a party in md1stin and walk through the exits:
#   A: --travel md1_1,md1stin --party-invite-first-peer     (there, wait for B, and back again)
#   B: --travel md1_1 --party-accept                        (there, and stays)
# Both hold still until the party exists, walk to the gateway of md1stin that leads to md1_1 (the
# line the SERVER watches), cross it, and are moved to md1_1 (AreaChange; each client answers from its
# OWN copy of the field). In md1_1 each sees the other; A then walks back through md1_1's gateway to
# md1stin. Expected, every bot exits 0 and prints
#   A: SMOKE ... area=Ff7/md1stin ... party=2 ... area_changes=2
#   B: SMOKE ... area=Ff7/md1_1   ... party=2 ... area_changes=1 and peers_seen >= 1 (A, in md1_1)
# plus, in the logs:
#   * each bot's `travel:` lines say it entered md1_1, and A's say it saw B there (the peers counter
#     of the area it left) before going back to md1stin;
#   * the NPCs of md1stin were announced (`headless: N NPC(s) in Ff7/md1stin: ...`, N >= 1);
#   * the server logged the load of md1_1 ("area loaded") and no bot reported a BATTLE DIVERGENCE.
# Random encounters are off (--no-random-encounters): a bot without --battle-auto would stand frozen in
# the first battle for good, and this smoke is about walking.
#
# --fixture: the same story on the synthetic FIXTURE rooms (no owned data needed; NOT FF7): the server
# runs `--game fixture-rooms --ephemeral`, the bots `--game fixture-rooms`; A travels yard,hall and B
# travels yard, starting in the hall. The yard has one NPC (`guard`). Expected: A ends in Fixture/hall
# with area_changes=2, B in Fixture/yard with area_changes=1 and a peer seen, the yard's NPC announced.
#
# --chain (real data): the first reactor mission, from the first field down toward the reactor, with the REAL story
# deciding which exits are open and the story advancing along the way the field scripts advance it. Both bots form a
# party (A invites, B accepts) and walk the route
#   md1stin -> md1_1 -> md1_2 -> nrthmk -> nmkin_1 -> elevtr1 -> (the lift) -> nmkin_2 -> nmkin_3
# (seven area changes). Six of them are field gateways. The way out of elevtr1 is not: the elevator has no gateway,
# only one line whose four exits the story picks from (crates/fmv-ff7/src/exit_gate.rs), so the party has to RIDE the
# lift first (stand on the panel's triangle: the server turns the lift bit on and moves the story to 12) before the
# exit to nmkin_2 is the open one. The route stops at nmkin_3 because that is as far as the walkmesh goes: its way
# down to nmkin_4 is a scripted ladder (crates/fmv-ff7/tests/chain_real.rs, the_walkmesh_carries_the_party_down_to_nmkin_3).
#   A (the leader):  --story-advance --party-invite-first-peer --travel ...   walks to the story point of each field
#                    that moves ITS story (a triangle, a line it crosses; the fields that move it by themselves do it
#                    when it comes to stand there), then on to the next exit
#   B (the member):  --party-accept --travel ... --travel-follow             crosses an exit only after A has left the
#                    field (the exits are judged by the story of the PARTY LEADER, so B must not get to the lift's line
#                    before A has ridden)
# Expected, every bot exits 0 and prints SMOKE ... area=Ff7/nmkin_3 ... party=2 ... area_changes=7, its `headless: now
# in` lines name the seven fields in that order and the route was walked to the end; the server loaded the eight
# fields, moved both players through an exit seven times each (A and B both left elevtr1 for nmkin_2, never for
# nmkin_1) and said nothing about an exit it could not judge. And the STORY, read from the server's own log lines for
# A's entity and from A's client, is exactly
#   md1stin 1->6, md1_1 6->7, md1_2 7->8, nrthmk 8->10, nmkin_1 10->11, elevtr1 11->12 with var1.225 0->1, nmkin_2 12->13
# ending in the blob 020d000101e101 (moment 13, bit 0 of Var[1][225] set), which the server saved. Random encounters are
# off (this is about walking). The default duration is 330 s (the bots run for the whole of it; the walk takes about
# 180 s on a debug build; --duration changes it).
#
# Isolation (the owner's state directory is never touched, only its LIBRARY is read): the server and the
# clients run with XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR
# pointing into a throw-away directory, the server with --state-dir (or --ephemeral) inside it, each
# client with its own --state-dir. The library is reached through a symlink
# `<work>/lib-state/library -> <STATE_DIR>/library` passed as --ff7-library, so nothing under STATE_DIR
# can be created or written. The script lists STATE_DIR before and after and fails if it changed.
#
# --library STATE_DIR is the directory that CONTAINS `library/` (default: $FMV_FF7_LIBRARY, else the
# owner's default state directory ~/.local/share/ff-multiverse). Ignored with --fixture.
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.
#
# This script reuses the helpers of scripts/smoke.sh and scripts/battle-smoke.sh (it sources them;
# sourcing runs nothing).

set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=battle-smoke.sh
source "$SELF_DIR/battle-smoke.sh"

# The sourced helpers print "battle-smoke.sh:"; this script says who it is.
say() { printf 'travel-smoke.sh: %s\n' "$*"; }
err() { printf 'travel-smoke.sh: error: %s\n' "$*" >&2; }

LIBRARY="${FMV_FF7_LIBRARY:-$HOME/.local/share/ff-multiverse}"
TRAVEL_DURATION=45
DURATION_GIVEN=0
CHAIN_DURATION=330
# The route of --chain: the fields after md1stin, in order (see the header).
CHAIN_ROUTE=(md1_1 md1_2 nrthmk nmkin_1 elevtr1 nmkin_2 nmkin_3)
# The story A makes on the way, as `area|point|what the server logged`, in the order of the route.
CHAIN_STORY=(
  "Ff7/md1stin|0|moment=1->6"
  "Ff7/md1_1|0|moment=6->7"
  "Ff7/md1_2|0|moment=7->8"
  "Ff7/nrthmk|0|moment=8->10"
  "Ff7/nmkin_1|0|moment=10->11"
  "Ff7/elevtr1|0|moment=11->12 var1.225=0->1"
  "Ff7/nmkin_2|0|moment=12->13"
)
# What A's story blob is at the end (moment 13, the lift bit on).
CHAIN_FINAL_BLOB=020d000101e101
MODE=real

usage() {
  cat <<'EOF'
Usage: scripts/travel-smoke.sh [--fixture|--chain] [--release|--debug] [--bin-dir DIR] [--build]
                               [--library STATE_DIR] [--duration SECS] [--keep-logs] [-h|--help]

Two headless bots form a party on the real FF7 field md1stin, both walk through its gateway to md1_1
(the server watches the exit line and moves them; each client loads md1_1 from its own copy), see each
other there, and one walks back to md1stin. Passes when both exit 0 with area_changes >= 1 (A ends in
md1stin with area_changes 2, B in md1_1 with area_changes 1 and a peer seen), both were in a party of
two, the NPCs of the fields were announced and the server loaded md1_1. --fixture does the same on the
synthetic FIXTURE rooms (hall and yard) and needs no owned data.

  --fixture          use the synthetic FIXTURE rooms instead of the real FF7 fields
  --chain            both bots walk md1stin -> md1_1 -> md1_2 -> nrthmk -> nmkin_1 -> elevtr1 -> (the lift) ->
                      nmkin_2 -> nmkin_3 (real data; the first reactor mission with its real story, as far as the
                      walkmesh goes): the leader advances the story on the way and rides the lift, the member follows
  --library DIR      state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                      ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS    how long each bot runs (default 45; 330 with --chain)
EOF
}

# area_changes_of <SMOKE line>: the number in area_changes=N (empty if absent).
area_changes_of() {
  field "$1" area_changes
}

run_travel() {
  # The two areas of the story: START is where both bots join and A returns to, FAR is the one both
  # walk to (md1stin -> md1_1 of FF7, or hall -> yard of the FIXTURE rooms); NPC_AREA is the area whose
  # snapshot must announce NPCs.
  local start_key far_key start_area far_area npc_area fixture_expected state_real="" before="" after=""
  local -a server_args client_args
  if [[ $MODE == fixture ]]; then
    start_key=hall far_key=yard start_area=Fixture/hall far_area=Fixture/yard npc_area=Fixture/yard fixture_expected=true
    mkdir -p -- "$WORK/state-env" "$WORK/client-A" "$WORK/client-B"
    export FMV_STATE_DIR="$WORK/state-env"
    server_args=(--bind 127.0.0.1:0 --game fixture-rooms --ephemeral)
    client_args=(--game fixture-rooms)
  else
    start_key=md1stin far_key=md1_1 start_area=Ff7/md1stin far_area=Ff7/md1_1 npc_area=Ff7/md1stin fixture_expected=false
    state_real="$LIBRARY"
    [[ -d $state_real/library/sets ]] || {
      err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
      exit 1
    }
    before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
    mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A" "$WORK/client-B"
    ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
    export FMV_STATE_DIR="$WORK/state-env"
    server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters)
    client_args=(--ff7-library "$WORK/lib-state")
  fi

  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  local area fixture
  area=$(field "$READY_LINE" area)
  fixture=$(field "$READY_LINE" fixture)
  [[ $area == "$start_area" && $fixture == "$fixture_expected" ]] || fail "the server is not hosting $start_area (fixture=$fixture_expected): $READY_LINE"

  say "starting bots A and B for ${TRAVEL_DURATION}s against $CONNECT"
  local common=(--server "$CONNECT" --headless --duration "$TRAVEL_DURATION" --bot circle "${client_args[@]}")
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" \
    --party-invite-first-peer --travel "$far_key,$start_key" \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" \
    --party-accept --travel "$far_key" \
    >"$WORK/client-B.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((TRAVEL_DURATION + CLIENT_GRACE))

  local n smoke_line value
  declare -A LINE=()
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    local log="$WORK/client-$n.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "client $n was still running ${CLIENT_GRACE}s after its ${TRAVEL_DURATION}s duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if smoke_line=$(last_line_with "$log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      LINE[$n]="$smoke_line"
      [[ $(field "$smoke_line" fixture) == "$fixture_expected" ]] || problem "client $n: fixture is not $fixture_expected: $smoke_line"
      value=$(area_changes_of "$smoke_line")
      if [[ ! $value =~ ^[0-9]+$ ]]; then
        problem "client $n: the SMOKE line has no numeric area_changes: $smoke_line"
      elif ((value < 1)); then
        problem "client $n never changed area (area_changes=$value): $smoke_line"
      fi
      value=$(field "$smoke_line" party)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 2)) || problem "client $n was never in a party of two (party=${value:-?}): $smoke_line"
    else
      problem "client $n printed no SMOKE line"
    fi
    log_has "$log" "travel: entering" || problem "client $n never logged a 'travel: entering' line"
    log_has "$log" "headless: travel: the route" || problem "client $n did not finish its route: $(last_line_with "$log" "headless: travel:" || echo 'no travel summary')"
    if log_has "$log" "DIVERGENCE"; then
      problem "client $n reported a BATTLE DIVERGENCE"
    fi
    # The NPCs of the area were announced (the snapshot of an area carries them).
    log_has "$log" "NPC(s) in $npc_area" || problem "client $n was never told of any NPC in $npc_area"
  done

  # A: back in the start area after two changes, and it saw B in the far area before leaving it.
  if [[ -n ${LINE[A]:-} ]]; then
    [[ $(field "${LINE[A]}" area) == "$start_area" ]] || problem "A should end in $start_area: ${LINE[A]}"
    [[ $(area_changes_of "${LINE[A]}") == 2 ]] || problem "A should have changed area twice (there and back): ${LINE[A]}"
    local leaving
    leaving=$(last_line_with "$WORK/client-A.log" "peer(s) were seen in \`$far_area\`" || true)
    if [[ -z $leaving ]]; then
      problem "A never logged leaving $far_key"
    elif [[ $(strip_ansi "$leaving") =~ ([0-9]+)\ peer\(s\)\ were\ seen\ in\ \`$far_area\` ]] && ((BASH_REMATCH[1] >= 1)); then
      say "A saw ${BASH_REMATCH[1]} peer(s) in $far_key before it went back"
    else
      problem "A saw nobody in $far_key: $leaving"
    fi
  fi
  # B: stays in the far area and saw A there.
  if [[ -n ${LINE[B]:-} ]]; then
    [[ $(field "${LINE[B]}" area) == "$far_area" ]] || problem "B should end in $far_area: ${LINE[B]}"
    [[ $(area_changes_of "${LINE[B]}") == 1 ]] || problem "B should have changed area once: ${LINE[B]}"
    value=$(field "${LINE[B]}" peers_seen)
    [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "B saw nobody in $far_key (peers_seen=${value:-?}): ${LINE[B]}"
  fi

  log_has "$WORK/server.log" "area loaded" || problem "the server never loaded another area"
  log_has "$WORK/server.log" "area=$far_area" || problem "the server never loaded $far_key"
  log_has "$WORK/server.log" "player leaves through an exit" || problem "the server never moved a player through an exit"

  stop_server
  if [[ $MODE == real ]]; then
    after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
    [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"
  fi

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "client: $l"
  done
  for n in "${CLIENT_NAMES[@]}"; do
    say "client $n: $(last_line_with "$WORK/client-$n.log" "NPC(s) in $npc_area" || true)"
    say "client $n: $(last_line_with "$WORK/client-$n.log" "travel: entering" || true)"
  done
  say "server: $(last_line_with "$WORK/server.log" "area loaded" || true)"
  RESULT=pass
  if [[ $MODE == real ]]; then
    say "PASS: two bots in a party walked $start_key -> $far_key through the gateway, saw each other there, and one walked back; $state_real unchanged ($after)"
  else
    say "PASS: two bots in a party walked $start_key -> $far_key through the exit on the FIXTURE rooms, saw each other there, and one walked back"
  fi
}

# entered_areas <log>: the areas a client says it entered, in order (its `headless: now in <area>` lines).
entered_areas() {
  local log="$1" line rest
  ENTERED=()
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"headless: now in "* ]]; then
      rest="${line#*headless: now in }"
      ENTERED+=("${rest%% *}")
    fi
  done <"$log"
}

# story_steps_of <server log> <entity>: the story steps the server logged for one player's entity, one per line as
# `area|point|what` (the words after `point=N`), in the order they happened.
story_steps_of() {
  local log="$1" entity="$2" line
  [[ -r $log ]] || return 0
  local re='story advanced account=([0-9]+) entity=([0-9]+) area=([^[:space:]]+) point=([0-9]+) (.*)$'
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line =~ $re && ${BASH_REMATCH[2]} == "$entity" ]]; then
      printf '%s|%s|%s\n' "${BASH_REMATCH[3]}" "${BASH_REMATCH[4]}" "${BASH_REMATCH[5]%"${BASH_REMATCH[5]##*[![:space:]]}"}"
    fi
  done <"$log"
}

# client_story_of <client log>: the `story: advanced point=N <what> (progress <hex>)` notes of a client, one per line
# as `point|what|progress`, in order.
client_story_of() {
  local log="$1" line
  [[ -r $log ]] || return 0
  local re='story: advanced point=([0-9]+) (.*) \(progress ([0-9a-f]+)\)'
  while IFS= read -r line || [[ -n $line ]]; do
    line=$(strip_ansi "$line")
    if [[ $line =~ $re ]]; then
      printf '%s|%s|%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}"
    fi
  done <"$log"
}

# run_chain: both bots walk the first reactor mission with its real story (see the header, "--chain").
run_chain() {
  local state_real="$LIBRARY" before="" after="" route_csv="" expected="" key n smoke_line value
  local -a server_args client_args
  route_csv=$(
    IFS=,
    printf '%s' "${CHAIN_ROUTE[*]}"
  )
  for key in "${CHAIN_ROUTE[@]}"; do
    expected+="Ff7/$key "
  done
  expected="${expected% }"
  [[ -d $state_real/library/sets ]] || {
    err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
    exit 1
  }
  before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A" "$WORK/client-B"
  ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
  export FMV_STATE_DIR="$WORK/state-env"
  server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters)
  client_args=(--ff7-library "$WORK/lib-state")

  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting Ff7/md1stin: $READY_LINE"

  say "starting bots A (the leader, advances the story) and B (follows it) for ${TRAVEL_DURATION}s against $CONNECT; route md1stin,$route_csv"
  local common=(--server "$CONNECT" --headless --duration "$TRAVEL_DURATION" --bot circle "${client_args[@]}")
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" --party-invite-first-peer --story-advance --travel "$route_csv" \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" --party-accept --travel "$route_csv" --travel-follow \
    >"$WORK/client-B.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((TRAVEL_DURATION + CLIENT_GRACE))

  declare -A LINE=()
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    local log="$WORK/client-$n.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "client $n was still running ${CLIENT_GRACE}s after its ${TRAVEL_DURATION}s duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if smoke_line=$(last_line_with "$log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      LINE[$n]="$smoke_line"
      [[ $(field "$smoke_line" fixture) == false ]] || problem "client $n: fixture is not false: $smoke_line"
      [[ $(field "$smoke_line" area) == "Ff7/${CHAIN_ROUTE[${#CHAIN_ROUTE[@]} - 1]}" ]] ||
        problem "client $n should end in Ff7/${CHAIN_ROUTE[${#CHAIN_ROUTE[@]} - 1]}: $smoke_line"
      value=$(area_changes_of "$smoke_line")
      [[ $value == "${#CHAIN_ROUTE[@]}" ]] || problem "client $n should have changed area ${#CHAIN_ROUTE[@]} times (area_changes=${value:-?}): $smoke_line"
      value=$(field "$smoke_line" party)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 2)) || problem "client $n was never in a party of two (party=${value:-?}): $smoke_line"
    else
      problem "client $n printed no SMOKE line"
    fi
    entered_areas "$log"
    [[ "${ENTERED[*]:-}" == "$expected" ]] || problem "client $n entered [${ENTERED[*]:-nothing}], expected [$expected]"
    log_has "$log" "headless: travel: the route" || problem "client $n did not finish its route: $(last_line_with "$log" "headless: travel:" || echo 'no travel summary')"
    if log_has "$log" "DIVERGENCE"; then
      problem "client $n reported a BATTLE DIVERGENCE"
    fi
    # The fields that have characters of their own announced them (the snapshot of an area carries them).
    # The client logs its NPC list only when the list CHANGES, and nrthmk lists the same names as md1_2,
    # so it can print nothing there: nrthmk is checked on the server's side below instead. The fields below
    # the lift have none that the data places for certain.
    for key in md1stin md1_1 md1_2 nmkin_1 elevtr1; do
      log_has "$log" "NPC(s) in Ff7/$key" || problem "client $n was never told of any NPC in Ff7/$key"
    done
  done

  local loaded
  for key in "${CHAIN_ROUTE[@]}"; do
    if loaded=$(last_line_with "$WORK/server.log" "area loaded area=Ff7/$key "); then
      case $key in
        nmkin_2 | nmkin_3) ;; # their characters are hidden by their own scripts or placed only in some stories
        *)
          # every other field of the route has characters of its own: the server loaded its NPCs with it
          value=$(field "$loaded" npcs)
          [[ $value =~ ^[1-9][0-9]*$ ]] || problem "the server loaded $key without NPCs (npcs=${value:-?}): $loaded"
          ;;
      esac
    else
      problem "the server never loaded $key"
    fi
  done
  value=$(count_lines_with "$WORK/server.log" "player leaves through an exit")
  [[ $value == $((2 * ${#CHAIN_ROUTE[@]})) ]] ||
    problem "the server should have moved the two players through an exit ${#CHAIN_ROUTE[@]} times each (it logged $value moves)"
  # The lift: both players left elevtr1 for nmkin_2 (the exit the story opened for the leader), none for nmkin_1.
  value=$(count_lines_with "$WORK/server.log" "from=elevtr1 to=nmkin_2")
  [[ $value == 2 ]] || problem "both players should have left elevtr1 for nmkin_2 (the server logged $value such moves)"
  value=$(count_lines_with "$WORK/server.log" "from=elevtr1 to=nmkin_1")
  [[ $value == 0 ]] || problem "a player left elevtr1 for nmkin_1 ($value moves): it crossed the lift's line before the leader had ridden"
  # Nothing on the route has an exit the server cannot judge.
  value=$(count_lines_with "$WORK/server.log" "exit closed: its conditions cannot be judged")
  [[ $value == 0 ]] || problem "the server reported an exit it could not judge on the route ($value time(s)): $(last_line_with "$WORK/server.log" 'exit closed: its conditions' || true)"

  # The story: A's entity (its client says which) made exactly the steps of the route, in order, and its client was told
  # the same ones; the server saved the story it ended with.
  local a_entity="" a_account="" joined re_entity='as `A` \(entity ([0-9]+)\)'
  joined=$(strip_ansi "$(last_line_with "$WORK/client-A.log" "headless: joined " || true)")
  [[ $joined =~ $re_entity ]] && a_entity=${BASH_REMATCH[1]}
  if [[ -z $a_entity ]]; then
    problem "could not read A's entity from its client log ($joined)"
  else
    local -a seen_story=() client_steps=()
    local step want rest i
    while IFS= read -r step; do
      [[ -n $step ]] && seen_story+=("$step")
    done < <(story_steps_of "$WORK/server.log" "$a_entity")
    if [[ "${seen_story[*]:-}" != "${CHAIN_STORY[*]}" ]]; then
      problem "A's story steps (server log) were [$(
        IFS=';'
        printf '%s' "${seen_story[*]:-none}"
      )], expected [$(
        IFS=';'
        printf '%s' "${CHAIN_STORY[*]}"
      )]"
    fi
    while IFS= read -r step; do
      [[ -n $step ]] && client_steps+=("$step")
    done < <(client_story_of "$WORK/client-A.log")
    if ((${#client_steps[@]} != ${#CHAIN_STORY[@]})); then
      problem "A's client was told ${#client_steps[@]} story steps, expected ${#CHAIN_STORY[@]}: [$(
        IFS=';'
        printf '%s' "${client_steps[*]:-none}"
      )]"
    else
      i=0
      for step in "${client_steps[@]}"; do
        want=${CHAIN_STORY[$i]}
        rest=${want#*|} # point|what
        [[ "${step%|*}" == "$rest" ]] || problem "A's client step $((i + 1)) was [${step%|*}], expected [$rest]"
        i=$((i + 1))
      done
      [[ "${client_steps[$((${#client_steps[@]} - 1))]##*|}" == "$CHAIN_FINAL_BLOB" ]] ||
        problem "A's story ended as ${client_steps[$((${#client_steps[@]} - 1))]##*|}, expected $CHAIN_FINAL_BLOB"
    fi
  fi

  stop_server
  # The saver writes what it still holds when the server stops: the story A ended with.
  if [[ -n $a_entity ]]; then
    local last_step re_account='story advanced account=([0-9]+) entity='"$a_entity"' '
    last_step=$(last_line_with "$WORK/server.log" "entity=$a_entity area=Ff7/nmkin_2 " || true)
    [[ $last_step =~ $re_account ]] && a_account=${BASH_REMATCH[1]}
    if [[ -z $a_account ]]; then
      problem "could not read A's account from the server log"
    else
      log_has "$WORK/server.log" "story saved account=$a_account bytes=7 progress=$CHAIN_FINAL_BLOB" ||
        problem "the server never saved A's story as $CHAIN_FINAL_BLOB: $(last_line_with "$WORK/server.log" "story saved account=$a_account" || echo 'no story saved line')"
    fi
  fi
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "client: $l"
  done
  for n in "${CLIENT_NAMES[@]}"; do
    entered_areas "$WORK/client-$n.log"
    say "client $n entered: ${ENTERED[*]}"
  done
  say "A's story, as the server logged it (entity $a_entity):"
  while IFS= read -r l; do
    say "  $l"
  done < <(story_steps_of "$WORK/server.log" "$a_entity")
  say "server: $(last_line_with "$WORK/server.log" "area loaded area=Ff7/${CHAIN_ROUTE[${#CHAIN_ROUTE[@]} - 1]} " || true)"
  say "server: $(last_line_with "$WORK/server.log" "story saved account=$a_account" || true)"
  RESULT=pass
  say "PASS: two bots in a party walked md1stin -> ${route_csv//,/ -> } with the real story: the leader moved it 1 -> 13 and rode the lift, the exit to nmkin_2 was the one the story opened, the member followed; $state_real unchanged ($after)"
}

main() {
  while (($#)); do
    case "$1" in
      --fixture) MODE=fixture ;;
      --chain) MODE=chain ;;
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
        TRAVEL_DURATION="$2"
        DURATION_GIVEN=1
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
  [[ $TRAVEL_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  if [[ $MODE == chain && $DURATION_GIVEN == 0 ]]; then
    TRAVEL_DURATION=$CHAIN_DURATION
  fi
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-travel-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  if [[ $MODE == chain ]]; then
    run_chain
  else
    run_travel
  fi
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
