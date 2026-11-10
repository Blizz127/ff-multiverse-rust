#!/usr/bin/env bash
# scripts/travel-smoke.sh - area-travel smoke test for fmv-server + fmv-client.
#
# Usage: scripts/travel-smoke.sh [--fixture|--chain|--escape] [--release|--debug] [--bin-dir DIR] [--build]
#                                [--story-extended-save] [--library STATE_DIR] [--duration SECS] [--keep-logs] [-h|--help]
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
# --chain (real data): the first reactor mission, from the first field down to the reactor's core and the Guard Scorpion, with
# the REAL story deciding which exits are open and the story advancing along the way the field scripts advance it. Both bots
# form a party (A invites, B accepts) and walk the route
#   md1stin -> md1_1 -> md1_2 -> nrthmk -> nmkin_1 -> elevtr1 -> (the lift) -> nmkin_2 -> nmkin_3 -> nmkin_4 -> nmkin_5
# (nine area changes). Six of them are field gateways and two more are walks the walkmesh alone cannot make: the way out of
# elevtr1 is the lift (no gateway, one line whose four exits the story picks from, crates/fmv-ff7/src/exit_gate.rs: the party
# has to RIDE it first, by pressing OK on the panel's triangle, so that the server turns the lift bit on and moves the story to
# 12), and the way down nmkin_3 and nmkin_4 is scripted (a jump over a gap and two ladders in nmkin_3, a ladder in nmkin_4:
# crates/fmv-ff7/src/links.rs reads them from the lines' scripts, the server puts a player where a link leads and tells it with
# MoveRejected, the bots plan over the links: crates/fmv-client/src/route.rs). In nmkin_3 the leader talks to the woman (13->14),
# in nmkin_5 to the materia on the floor (14->15), and the triangle of the core starts the scripted fight (the Guard Scorpion,
# formation 324, 15->27 when it is WON).
#   A (the leader):  --story-advance --party-invite-first-peer --travel ... --battle-auto   walks to the story point of each
#                    field that moves ITS story (a triangle, a line it crosses, a press of OK, a character it talks to, a battle;
#                    the fields that move it by themselves do it when it comes to stand there), then on to the next exit
#   B (the member):  --party-accept --travel ... --travel-follow --battle-auto   crosses an exit only after A has left the field
#                    (the exits are judged by the story of the PARTY LEADER, so B must not get to the lift's line before A has
#                    ridden)
# Expected, every bot exits 0 and prints SMOKE ... area=Ff7/nmkin_5 ... party=2 battles=1 victories=1 ... area_changes=9, its
# `headless: now in` lines name the nine fields in that order and the route was walked to the end; the server loaded the ten
# fields, moved both players through an exit nine times each (A and B both left elevtr1 for nmkin_2, never for nmkin_1), put
# each of them through at least three links (it logs each one) and said nothing about an exit it could not judge; it started
# exactly one battle, asked for by a story point (`story battle started`), formation 324 with one enemy, the canon pair and both
# bots at the pinned level (--battle-level 30: this smoke is about the walk and the fight starting where the script says, not
# about the boss, which battle-smoke.sh --boss fights), and it ended in Victory; both bots entered that same battle, replayed it
# from their own data and matched with 0 divergences. And the STORY, read from the server's own log lines for A's entity and
# from A's client, is exactly
#   md1stin 1->6, md1_1 6->7, md1_2 7->8, nrthmk 8->10, nmkin_1 10->11, elevtr1 11->12 with var1.225 0->1, nmkin_2 12->13,
#   nmkin_3 13->14, nmkin_5 14->15 (point 3, the materia), nmkin_5 15->27 (point 0, the fight won)
# ending in the blob 021b000101e101 (moment 27, bit 0 of Var[1][225] set), which the server saved. Random encounters are off
# (this is about walking). The default duration is 300 s (the bots run for the whole of it; the walk takes about 130 s on a
# debug build, measured; --duration changes it).
#
# --escape (real data): the same walk, and then the way OUT: after the Guard Scorpion is won (moment 27) the party turns round
# and climbs back up through the reactor, the countdown running, to the way out of it, in ONE continuous run (a party that is
# put at the saved moment 27 cannot be put inside the reactor: a re-login always joins md1stin, and the walk from there to the
# reactor goes through nrthmk, where the finale moves the story on to 33 before the party is back inside). The route is
#   md1stin -> ... -> nmkin_5 (the fight) -> nmkin_4 -> nmkin_3 -> nmkin_2 -> elevtr1 -> (the lift, up) -> nmkin_1 -> nrthmk
# (fifteen area changes). What the leader's story does after 27 (all read from the field scripts, see the READMEs "After the
# Guard Scorpion"): the lift is ridden BACK UP by pressing OK on the panel again (the panel's own script clears the lift bit:
# a step that is not forward, which the leader's bot pursues because it opens the way it is going), and coming to stand in
# nrthmk with the story at 27 or later is the finale of the mission (the explosion's cutscene), which moves it to 33. On the
# way the leader also talks to the woman of nmkin_3, who is stuck (her talk script turns bit 5 of the reactor byte on from
# moment 27 on: a talk that moves the story, which the leader's bot pursues like any other). Exactly the steps of --chain, then
#   nmkin_3 27->27 with var1.225=1->33 (point 0, the woman), elevtr1 27->27 with var1.225=33->32 (point 1, the ride back),
#   nrthmk 27->33 with var1.225=32->33 (point 1, the finale)
# and the blob the server saved is 0221000101e121 (moment 33, bits 0 and 5 of Var[1][225]). The COUNTDOWN: the server logged exactly
# one `countdown started account=<A> seconds=600` (the step that won the fight, from the STTIM of the field's script) and one
# `countdown over account=<A> reason=cleared` (the finale), never `time-up`, and both bots' clients said so (`countdown:
# running: ...`, `countdown: over: cleared`). 0 battle divergences, the one battle the chain has and no other.
# Then the server is restarted on the same state directory and A logs in again at the saved moment 33. A story at 33 stands
# outside the reactor (`fmv_ff7::script::scene::STORY_ENTRIES`), so A joins like everyone, is told its story (`joined with
# progress 0221000101e121`) and is sent on to md8_4 at once (the server logs `story entry ... to=md8_4`). There the first
# scene after the escape starts by itself and A's headless client plays it through: it changes nothing that is saved and
# ends in the field's own jump to md8_1, where the second starts by itself, takes Barret out of the party and moves the
# story 33 -> 36, once. The server saves it (0224000101e121: moment 36, the reactor byte as it was), no countdown is started,
# and A ends in md8_1. (`scripts/story-smoke.sh` is the same pair of scenes on a debug-started account, with a re-login.)
#
# --story-extended-save (with --chain/--escape): both bots register new accounts with the server's
# source-backed v3 first-control profile. No debug story or existing save is supplied. The opening
# fight (300) and Guard Scorpion (324) both run, with their durable outcomes and the ordinary story
# transactions. Canonical v3 snapshots, known-bit retention, the complete story sequence, and exact
# wire/store/relogin equality are checked by travel-extended-check.py. The level-30 battle modifier
# remains a route-reliability aid; this does not prove the route's native combat difficulty.
# This required route omits the optional Restore materia pickup: its SMTRA 53/AP 0 grant and
# bank13:31 result are not supported atomically. No partial moment15 is saved. The authored boss
# accepts moment14 directly and writes reactor bytes225=65/226=4; subsequent exact bits are checked.
# The missed pickup disappears after the boss (hidden at moment>=15), so this is not all-choice parity.
# In extended --escape the finale's authored MAPJUMP continues through md8_4 to md8_1, saving moment
# 36 during the first run. The restart then checks that story/resources survive without replay.
# The owner must complete all 17 transfers. The follower completes the 15 reactor transfers and
# joins the final two only if the finale actually selected it as a watcher; its personal story stays 1.
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
STORY_EXTENDED_SAVE=0
CHAIN_DURATION=300
# The route of --chain: the fields after md1stin, in order (see the header).
CHAIN_ROUTE=(md1_1 md1_2 nrthmk nmkin_1 elevtr1 nmkin_2 nmkin_3 nmkin_4 nmkin_5)
# The story A makes on the way, as `area|point|what the server logged`, in the order of the route.
CHAIN_STORY=(
  "Ff7/md1stin|0|moment=1->6"
  "Ff7/md1_1|0|moment=6->7"
  "Ff7/md1_2|0|moment=7->8"
  "Ff7/nrthmk|0|moment=8->10"
  "Ff7/nmkin_1|0|moment=10->11"
  "Ff7/elevtr1|0|moment=11->12 var1.225=0->1"
  "Ff7/nmkin_2|0|moment=12->13"
  "Ff7/nmkin_3|0|moment=13->14"
  "Ff7/nmkin_5|3|moment=14->15"
  "Ff7/nmkin_5|0|moment=15->27"
)
# What A's story blob is at the end (moment 27, the lift bit on).
CHAIN_FINAL_BLOB=021b000101e101
# The scripted fight of nmkin_5 (the Guard Scorpion, formation 324), fought at this level at the engine's own speed limit
# (--battle-speed 0) by bots that play as --battle-auto does. The level is high on purpose: this smoke is about the walk, the
# ladders, the talks and the fight starting where the script starts it and paying its step on the win, not about the
# difficulty of the boss (the starting level 6 loses about one fight in six, level 8 with defending bots wins nearly all;
# `battle-smoke.sh --boss` is where the boss itself is fought and checked).
CHAIN_BATTLE_FORMATION=324
CHAIN_BATTLE_LEVEL=30
# --escape: the way back out after the fight (see the header). The fields after nmkin_5, in order; the two steps the leader makes
# on the way (the lift's ride back is the panel's second point, the finale is nrthmk's second); the blob the server saves at
# the end (moment 33, bit 0 of Var[1][225]); the length of the bomb's countdown (the STTIM of the field's script, seconds).
# (the walk takes about 200 s on a debug build, measured: 130 s down and 70 s back up; --duration changes it)
ESCAPE_DURATION=330
ESCAPE_ROUTE_TAIL=(nmkin_4 nmkin_3 nmkin_2 elevtr1 nmkin_1 nrthmk)
ESCAPE_STORY_TAIL=(
  "Ff7/nmkin_3|0|var1.225=1->33"
  "Ff7/elevtr1|1|var1.225=33->32"
  "Ff7/nrthmk|1|moment=27->33 var1.225=32->33"
)
ESCAPE_FINAL_BLOB=0221000101e121
ESCAPE_COUNTDOWN_SECS=600
# After the server restarts, A logs in again at the saved moment 33: it is sent to md8_4, plays the two scenes the escape ends
# in and is saved at 36 in md8_1 (moment 36, the reactor byte of the first run).
RELOGIN_DURATION=40
RELOGIN_AREAS=(md8_4 md8_1)
RELOGIN_FINAL_BLOB=0224000101e121
MODE=real

usage() {
  cat <<'EOF'
Usage: scripts/travel-smoke.sh [--fixture|--chain|--escape] [--release|--debug] [--bin-dir DIR] [--build]
                               [--story-extended-save] [--library STATE_DIR] [--duration SECS] [--keep-logs] [-h|--help]

Two headless bots form a party on the real FF7 field md1stin, both walk through its gateway to md1_1
(the server watches the exit line and moves them; each client loads md1_1 from its own copy), see each
other there, and one walks back to md1stin. Passes when both exit 0 with area_changes >= 1 (A ends in
md1stin with area_changes 2, B in md1_1 with area_changes 1 and a peer seen), both were in a party of
two, the NPCs of the fields were announced and the server loaded md1_1. --fixture does the same on the
synthetic FIXTURE rooms (hall and yard) and needs no owned data.

  --fixture          use the synthetic FIXTURE rooms instead of the real FF7 fields
  --chain            both bots walk md1stin -> md1_1 -> md1_2 -> nrthmk -> nmkin_1 -> elevtr1 -> (the lift) ->
                      nmkin_2 -> nmkin_3 -> nmkin_4 -> nmkin_5 (real data; the first reactor mission with its real story,
                      down to the Guard Scorpion): the leader advances the story on the way (rides the lift, talks to the
                      woman and the materia, wins the scripted fight), both climb the ladders and jump the gap, the member
                      follows
  --escape           the same walk and then the way back out of the reactor with the bomb's countdown running (real data):
                      up the ladders, the lift ridden back up with OK, out to nrthmk, where the finale moves the story to 33
                      (one continuous run), then the server restarts and the leader logs in again at 33: it is sent
                      to md8_4, the two scenes after the escape play by themselves and the story is saved at 36
  --story-extended-save  register both bots with fresh v3 story state (--chain/--escape only); checks opening fight 300,
                      boss 324, durable progression and exact saved/relogin bytes (requires python3). Extended --escape
                      follows the finale's MAPJUMP through md8_4 to md8_1/36, then reconnects without replay.
                      Optional Restore materia pickup remains unsupported; this checks the required route only.
  --library DIR      state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                      ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS    how long each bot runs (default 45; 300 with --chain; 330 with --escape)
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
  local state_real="$LIBRARY" before="" after="" route_csv="" expected="" member_route_csv="" member_expected="" key n smoke_line value
  local -a server_args client_args
  # The route, the story and the final blob of this walk: --chain's, and --escape's after them (see the header).
  local -a ROUTE=("${CHAIN_ROUTE[@]}") STORY=("${CHAIN_STORY[@]}")
  local FINAL_BLOB=$CHAIN_FINAL_BLOB last_story_area=Ff7/nmkin_5 min_links=6
  if [[ $MODE == escape ]]; then
    ROUTE+=("${ESCAPE_ROUTE_TAIL[@]}")
    STORY+=("${ESCAPE_STORY_TAIL[@]}")
    FINAL_BLOB=$ESCAPE_FINAL_BLOB
    last_story_area=Ff7/nrthmk
    min_links=16 # (measured: 16 = two players, four links down and four back up)
    if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
      ROUTE+=(md8_4 md8_1)
      last_story_area=Ff7/md8_1
    fi
  fi
  route_csv=$(
    IFS=,
    printf '%s' "${ROUTE[*]}"
  )
  for key in "${ROUTE[@]}"; do
    expected+="Ff7/$key "
  done
  expected="${expected% }"
  member_route_csv=$route_csv
  member_expected=$expected
  if [[ $STORY_EXTENDED_SAVE == 1 && $MODE == escape ]]; then
    # A late follower may reach nrthmk after the entry finale selected its audience. It still
    # completes the reactor route; the verifier requires the extra pair iff it was a watcher.
    member_route_csv=${route_csv%,md8_4,md8_1}
    member_expected=${expected% Ff7/md8_4 Ff7/md8_1}
  fi
  [[ -d $state_real/library/sets ]] || {
    err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
    exit 1
  }
  before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A" "$WORK/client-B"
  ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
  export FMV_STATE_DIR="$WORK/state-env"
  server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters
    --battle-speed 0 --battle-tick-rate 240 --battle-level "$CHAIN_BATTLE_LEVEL")
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    server_args+=(--story-extended-save)
  fi
  client_args=(--ff7-library "$WORK/lib-state")

  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting Ff7/md1stin: $READY_LINE"
  # (a client keeps the token of an account per server ADDRESS, so --escape's second run must come back to this port)
  local first_addr="$ADDR"

  say "starting bots A (the leader, advances the story) and B (follows it) for ${TRAVEL_DURATION}s against $CONNECT; route md1stin,$route_csv"
  local common=(--server "$CONNECT" --headless --duration "$TRAVEL_DURATION" --bot circle --battle-auto "${client_args[@]}")
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    common+=(--mods off)
  fi
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" --party-invite-first-peer --story-advance --travel "$route_csv" \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" --party-accept --travel "$member_route_csv" --travel-follow \
    >"$WORK/client-B.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((TRAVEL_DURATION + CLIENT_GRACE))

  declare -A LINE=()
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    local log="$WORK/client-$n.log"
    local expected_client="$expected" expected_count=${#ROUTE[@]} expected_final="${ROUTE[${#ROUTE[@]} - 1]}"
    if [[ $STORY_EXTENDED_SAVE == 1 && $MODE == escape && $n == B ]]; then
      # Presence in the finale cohort is independently checked against the actual SceneStart.
      if [[ $(area_changes_of "$(last_line_with "$log" "SMOKE name=" || true)") != "${#ROUTE[@]}" ]]; then
        expected_client=$member_expected
        expected_count=$((${#ROUTE[@]} - 2))
        expected_final=nrthmk
      fi
    fi
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
      [[ $(field "$smoke_line" area) == "Ff7/$expected_final" ]] ||
        problem "client $n should end in Ff7/$expected_final: $smoke_line"
      value=$(area_changes_of "$smoke_line")
      [[ $value == "$expected_count" ]] || problem "client $n should have changed area $expected_count times (area_changes=${value:-?}): $smoke_line"
      value=$(field "$smoke_line" party)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 2)) || problem "client $n was never in a party of two (party=${value:-?}): $smoke_line"
    else
      problem "client $n printed no SMOKE line"
    fi
    entered_areas "$log"
    [[ "${ENTERED[*]:-}" == "$expected_client" ]] || problem "client $n entered [${ENTERED[*]:-nothing}], expected [$expected_client]"
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
  for key in "${ROUTE[@]}"; do
    if loaded=$(last_line_with "$WORK/server.log" "area loaded area=Ff7/$key "); then
      case $key in
        nmkin_2 | nmkin_3 | nmkin_4 | nmkin_5 | md8_4 | md8_1) ;; # hidden/script-placed cast, no static NPC requirement
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
  local expected_exits=$((2 * ${#ROUTE[@]}))
  if [[ $STORY_EXTENDED_SAVE == 1 && $MODE == escape && $(area_changes_of "${LINE[B]:-}") == 15 ]]; then
    expected_exits=$((expected_exits - 2))
  fi
  [[ $value == "$expected_exits" ]] ||
    problem "the server should have logged $expected_exits authored field transfers for the two players (it logged $value moves)"
  # The lift: both players left elevtr1 for nmkin_2 (the exit the story opened for the leader). With --chain none went back up
  # (and none crossed the lift's line toward nmkin_1 before the leader had ridden); with --escape both did, after the leader's
  # ride back up opened that exit.
  value=$(count_lines_with "$WORK/server.log" "from=elevtr1 to=nmkin_2")
  [[ $value == 2 ]] || problem "both players should have left elevtr1 for nmkin_2 (the server logged $value such moves)"
  value=$(count_lines_with "$WORK/server.log" "from=elevtr1 to=nmkin_1")
  if [[ $MODE == escape ]]; then
    [[ $value == 2 ]] || problem "both players should have left elevtr1 for nmkin_1 after the leader rode back up (the server logged $value such moves)"
  else
    [[ $value == 0 ]] || problem "a player left elevtr1 for nmkin_1 ($value moves): it crossed the lift's line before the leader had ridden"
  fi
  # A member that comes to the lift only after its leader's finale (some seconds behind: the story is at 33 by then, where the
  # lift leads to the second reactor) is taken through the exit the leader took (`player follows its leader's way out`). How
  # often that happens depends on how far behind the member is, so it is only reported; where it happens it must be the leader's
  # way up and never anywhere else (no `smkin_*` appears in any move: the two moves per player above are the whole story).
  value=$(count_lines_with "$WORK/server.log" "player follows its leader's way out")
  if ((value > 0)); then
    say "server: the member followed its leader's way out of the lift $value time(s), the story having moved on: $(strip_ansi "$(last_line_with "$WORK/server.log" "player follows its leader's way out" || true)")"
    [[ $(count_lines_with "$WORK/server.log" "area=elevtr1 follows=nmkin_1 story_says=") == "$value" ]] ||
      problem "a member followed its leader's way out somewhere that is not the lift's way up: $(last_line_with "$WORK/server.log" "player follows its leader's way out" || true)"
  fi
  value=$(count_lines_with "$WORK/server.log" "smkin")
  [[ $value == 0 ]] || problem "the server mentions smkin ($value lines): somebody was sent, or offered a way, into the second reactor: $(last_line_with "$WORK/server.log" "smkin" || true)"
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    # Extended battle logs and the complete ID/formation sequences are checked after the store closes.
    for n in "${CLIENT_NAMES[@]}"; do
      check_mirror "$n" "$WORK/client-$n.log" 2
    done
  else
    # The Guard Scorpion: nmkin_5's triangle started the scripted fight (formation 324, one enemy) for the party, the canon pair
    # and both bots at the pinned level, and the story step came after the win. Every bot played the whole battle from its own
    # data and matched the server (0 divergences, every battle verified by replay).
    local started ended outcome="" battle_ids=()
    # ("world: battle started battle=" is the battle's own start line; the story's "story battle started battle=" says who asked for it)
    value=$(count_lines_with "$WORK/server.log" "world: battle started battle=")
    [[ $value == 1 ]] || problem "the server should have started exactly one battle (the scripted one), it logged $value"
    value=$(count_lines_with "$WORK/server.log" "story battle started battle=")
    [[ $value == 1 ]] || problem "the one battle should have been started by a story point (the server logged $value 'story battle started' lines)"
    started=$(strip_ansi "$(first_line_with "$WORK/server.log" "world: battle started battle=" || true)")
    ended=$(strip_ansi "$(last_line_with "$WORK/server.log" "battle ended battle=" || true)")
    if [[ -z $started ]]; then
      problem "the server never started the scripted battle"
    else
      [[ $(field "$started" formation) == "$CHAIN_BATTLE_FORMATION" ]] || problem "the scripted battle was not formation $CHAIN_BATTLE_FORMATION: $started"
      [[ $(field "$started" enemies) == 1 ]] || problem "the Guard Scorpion's formation has one enemy: $started"
      [[ $started == *"members=2"* && $(companions_of "$started") == 2 ]] || problem "the scripted battle did not have the canon pair and both bots: $started"
      [[ $started == *"levels=[$CHAIN_BATTLE_LEVEL, $CHAIN_BATTLE_LEVEL, $CHAIN_BATTLE_LEVEL, $CHAIN_BATTLE_LEVEL]"* ]] ||
        problem "the fight was not pinned to level $CHAIN_BATTLE_LEVEL (--battle-level): $started"
    fi
    if [[ -z $ended ]]; then
      problem "the server never logged the end of the scripted battle"
    else
      outcome=$(field "$ended" outcome)
      [[ $outcome == Victory ]] || problem "the scripted battle was not won (outcome '$outcome'): the story step after it cannot have been paid: $ended"
    fi
    for n in "${CLIENT_NAMES[@]}"; do
      check_mirror "$n" "$WORK/client-$n.log" 1
      battle_ids+=("$(battle_id_of "$WORK/client-$n.log" || true)")
      log_has "$WORK/client-$n.log" "started: formation $CHAIN_BATTLE_FORMATION," || problem "client $n did not start a battle of formation $CHAIN_BATTLE_FORMATION"
    done
    [[ -n ${battle_ids[0]:-} && ${battle_ids[0]} == "${battle_ids[1]:-}" ]] || problem "the bots did not enter the same battle (A: '${battle_ids[0]:-}', B: '${battle_ids[1]:-}')"
  fi
  # Ladders and jumps: the server moved each player inside nmkin_3 and nmkin_4 by their lines (it logs each one).
  value=$(count_lines_with "$WORK/server.log" "player used a link entity=")
  ((value >= min_links)) ||
    problem "the server logged only $value link use(s), expected at least $min_links: the party cannot have climbed nmkin_3 and nmkin_4 (two players, at least three links each way)"
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
  elif [[ $STORY_EXTENDED_SAVE == 0 ]]; then
    local -a seen_story=() client_steps=()
    local step want rest i
    while IFS= read -r step; do
      [[ -n $step ]] && seen_story+=("$step")
    done < <(story_steps_of "$WORK/server.log" "$a_entity")
    if [[ "${seen_story[*]:-}" != "${STORY[*]}" ]]; then
      problem "A's story steps (server log) were [$(
        IFS=';'
        printf '%s' "${seen_story[*]:-none}"
      )], expected [$(
        IFS=';'
        printf '%s' "${STORY[*]}"
      )]"
    fi
    while IFS= read -r step; do
      [[ -n $step ]] && client_steps+=("$step")
    done < <(client_story_of "$WORK/client-A.log")
    if ((${#client_steps[@]} != ${#STORY[@]})); then
      problem "A's client was told ${#client_steps[@]} story steps, expected ${#STORY[@]}: [$(
        IFS=';'
        printf '%s' "${client_steps[*]:-none}"
      )]"
    else
      i=0
      for step in "${client_steps[@]}"; do
        want=${STORY[$i]}
        rest=${want#*|} # point|what
        [[ "${step%|*}" == "$rest" ]] || problem "A's client step $((i + 1)) was [${step%|*}], expected [$rest]"
        i=$((i + 1))
      done
      [[ "${client_steps[$((${#client_steps[@]} - 1))]##*|}" == "$FINAL_BLOB" ]] ||
        problem "A's story ended as ${client_steps[$((${#client_steps[@]} - 1))]##*|}, expected $FINAL_BLOB"
    fi
  fi

  stop_server
  # The saver writes what it still holds when the server stops: the story A ended with.
  if [[ -n $a_entity ]]; then
    local last_step re_account='story advanced account=([0-9]+) entity='"$a_entity"' '
    # (the story line of the last step is not the last line that names the entity and the area: a move may follow it,
    # and that line has no account)
    last_step=$(last_line_with "$WORK/server.log" "entity=$a_entity area=$last_story_area point=" || true)
    [[ $last_step =~ $re_account ]] && a_account=${BASH_REMATCH[1]}
    if [[ -z $a_account ]]; then
      problem "could not read A's account from the server log"
    elif [[ $STORY_EXTENDED_SAVE == 0 ]]; then
      log_has "$WORK/server.log" "story saved account=$a_account bytes=7 progress=$FINAL_BLOB" ||
        problem "the server never saved A's story as $FINAL_BLOB: $(last_line_with "$WORK/server.log" "story saved account=$a_account" || echo 'no story saved line')"
    fi
  fi
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    python3 "$SELF_DIR/travel-extended-check.py" route "$WORK" "$MODE" "$a_account" ||
      problem "fresh v3 route/state checks failed (see verifier output above)"
    FINAL_BLOB=$(stored_blob "$WORK/server-state/progression/$a_account/Ff7.json")
  fi
  if [[ $MODE == escape ]]; then
    check_countdown "$a_account"
    # Only a walk that held together is worth a second run.
    if ((${#PROBLEMS[@]} == 0)); then
      relogin_after_escape "$a_account" "$first_addr" "${server_args[@]}"
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
  say "server: $(last_line_with "$WORK/server.log" "area loaded area=Ff7/${ROUTE[${#ROUTE[@]} - 1]} " || true)"
  say "server: $(last_line_with "$WORK/server.log" "story saved account=$a_account" || true)"
  RESULT=pass
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    say "PASS: fresh v3 owner completed the $MODE route and the follower completed the reactor route; fights 300 and 324 were won and replay-verified, authored progression and both personal saved stories passed canonical v3/wire checks; level $CHAIN_BATTLE_LEVEL is a route test modifier; owned library state unchanged."
    say "COVERAGE GAP: optional Restore materia pickup is unsupported; its grant/result/moment15 are not saved and the pickup is unavailable after the boss."
    [[ $MODE != escape ]] || say "PASS: the authored escape continued through moment 33 to md8_1/36; relogin retained exact story/resources without replay; the follower kept its own fresh story."
  elif [[ $MODE == escape ]]; then
    say "PASS: two bots in a party walked md1stin -> ${route_csv//,/ -> } with the real story: the leader moved it 1 -> 27 (the Guard Scorpion fought and won, which started the bomb's ${ESCAPE_COUNTDOWN_SECS} s countdown), the lift was ridden back up with OK, the ladders and jumps were climbed in both directions, the finale in nrthmk moved the story to 33 (blob $FINAL_BLOB saved) and ended the countdown (cleared, never time-up), the member followed; after a restart the leader logged in at 33, was sent to md8_4, played the two scenes after the escape and was saved at 36 ($RELOGIN_FINAL_BLOB) in md8_1; $state_real unchanged ($after)"
  else
    say "PASS: two bots in a party walked md1stin -> ${route_csv//,/ -> } with the real story: the leader moved it 1 -> 27 (the lift ridden with OK, the woman and the materia talked to, the Guard Scorpion fought and won), the ladders and jumps of nmkin_3 and nmkin_4 were climbed, the exit to nmkin_2 was the one the story opened, the member followed; $state_real unchanged ($after)"
  fi
}

# stored_blob <progression file>: the hex of the progression blob a store record holds, or empty.
stored_blob() {
  local file="$1" text
  [[ -r $file ]] || return 0
  text=$(<"$file")
  if [[ $text =~ \"blob\"[[:space:]]*:[[:space:]]*\"([0-9a-f]*)\" ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
}

# check_countdown <account of the leader>: what --escape's first run says of the bomb's countdown (header): the server started
# exactly one, from the field's own timer (STTIM), ended it exactly once because the story moved past it (the finale), never
# because the time ran out; and both clients said so.
check_countdown() {
  local account="$1" n value
  value=$(count_lines_with "$WORK/server.log" "countdown started ")
  [[ $value == 1 ]] || problem "the server should have started exactly one countdown (it logged $value)"
  value=$(count_lines_with "$WORK/server.log" "countdown started account=$account seconds=$ESCAPE_COUNTDOWN_SECS")
  [[ $value == 1 ]] || problem "the server should have started the leader's countdown of $ESCAPE_COUNTDOWN_SECS s once (it logged $value such lines): $(last_line_with "$WORK/server.log" 'countdown started' || echo 'none')"
  value=$(count_lines_with "$WORK/server.log" "countdown over account=$account reason=cleared")
  [[ $value == 1 ]] || problem "the server should have ended the countdown once because the story moved past it (it logged $value such lines): $(last_line_with "$WORK/server.log" 'countdown over' || echo 'none')"
  value=$(count_lines_with "$WORK/server.log" "reason=time-up")
  [[ $value == 0 ]] || problem "the countdown ran out ($value time-up lines): the party was too slow, or the walk was"
  for n in "${CLIENT_NAMES[@]}"; do
    log_has "$WORK/client-$n.log" "countdown: running: ${ESCAPE_COUNTDOWN_SECS}.0 s left of ${ESCAPE_COUNTDOWN_SECS}.0 s" ||
      problem "client $n was never told the countdown started ($(last_line_with "$WORK/client-$n.log" 'countdown:' || echo 'no countdown line'))"
    log_has "$WORK/client-$n.log" "countdown: over: cleared" || problem "client $n was never told the countdown was cleared"
    if log_has "$WORK/client-$n.log" "countdown: over: time is up"; then
      problem "client $n was told the time was up"
    fi
  done
}

# relogin_after_escape <account of the leader> <address of the first server> <the first server's arguments>...: --escape's second run
# (header): the server restarts on the same state directory and A logs in again at the saved moment 33. It is told its story,
# sent to md8_4, the two scenes after the escape start by themselves and are played through, the story moves 33 -> 36 once and
# is saved, no countdown is started, and A ends in md8_1. Extended mode already reached 36; its restart must replay nothing.
relogin_after_escape() {
  local account="$1" first_addr="$2" line value stored expected_areas=""
  shift 2
  local -a args=("$@") client_args=(--ff7-library "$WORK/lib-state")
  local -a login_areas=("${RELOGIN_AREAS[@]}")
  [[ $STORY_EXTENDED_SAVE == 0 ]] || login_areas=(md8_1)
  # (the first server argument is --bind, its value the next one)
  args[1]="$first_addr"
  for n in "${login_areas[@]}"; do
    expected_areas+="Ff7/$n "
  done
  expected_areas="${expected_areas% }"
  say "run 2: restarting the server on the same state directory ($first_addr)"
  "$SERVER_BIN" "${args[@]}" >"$WORK/server-2.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server-2.log"
  say "server READY: $READY_LINE"
  CPID=()
  CRC=()
  CLIENT_NAMES=(A)
  say "run 2: bot A logs in again for ${RELOGIN_DURATION}s against $CONNECT (it stands still: where it goes is the story's doing)"
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration "$RELOGIN_DURATION" --bot idle --mods off "${client_args[@]}" \
    --name A --state-dir "$WORK/client-A" >"$WORK/client-A-2.log" 2>&1 &
  CPID[A]=$!
  wait_clients $((RELOGIN_DURATION + CLIENT_GRACE))
  local log="$WORK/client-A-2.log" sl="$WORK/server-2.log"
  if [[ ${CRC[A]:-} == timeout ]]; then
    problem "run 2: client A was still running ${CLIENT_GRACE}s after its ${RELOGIN_DURATION}s duration and was killed"
  elif [[ ${CRC[A]:-} != 0 ]]; then
    problem "run 2: client A exited with status ${CRC[A]:-unknown}"
  fi
  if line=$(last_line_with "$log" "SMOKE name="); then
    line=$(strip_ansi "$line")
    [[ $(field "$line" area) == "Ff7/${login_areas[${#login_areas[@]} - 1]}" ]] ||
      problem "run 2: A should end in Ff7/${login_areas[${#login_areas[@]} - 1]}: $line"
    value=$(area_changes_of "$line")
    [[ $value == "${#login_areas[@]}" ]] || problem "run 2: A should have changed area ${#login_areas[@]} times (area_changes=${value:-?}): $line"
    say "run 2 client: $line"
  else
    problem "run 2: client A printed no SMOKE line"
  fi
  entered_areas "$log"
  [[ "${ENTERED[*]:-}" == "$expected_areas" ]] || problem "run 2: A entered [${ENTERED[*]:-nothing}], expected [$expected_areas]"
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    stop_server
    python3 "$SELF_DIR/travel-extended-check.py" relogin "$WORK" "$account" ||
      problem "fresh v3 relogin/state checks failed (see verifier output above)"
    return
  fi
  log_has "$log" "story: joined with progress $ESCAPE_FINAL_BLOB" ||
    problem "run 2: A was not told its story ($ESCAPE_FINAL_BLOB) when it logged in again: $(last_line_with "$log" 'story:' || echo 'no story line')"
  value=$(count_lines_with "$log" "scene: ended run=")
  [[ $value == 2 ]] || problem "run 2: A should have played two scenes (it played $value)"
  value=$(count_lines_with "$log" "outcome=Finished")
  [[ $value == 2 ]] || problem "run 2: both scenes should end Finished on the client ($value did)"
  if log_has "$log" "countdown:"; then
    problem "run 2: A was told of a countdown after logging in (a countdown is not stored): $(last_line_with "$log" 'countdown:' || true)"
  fi
  stop_server
  log_has "$sl" "to=md8_4 (where its story stands)" || problem "run 2: the server did not send A to md8_4 at login: $(last_line_with "$sl" 'story entry' || echo 'no story entry line')"
  value=$(count_lines_with "$sl" "scene ended run=")
  [[ $value == 2 ]] || problem "run 2: the server should have ended two scenes ($value)"
  log_has "$sl" "scene=ff7/md8_4/post-escape outcome=Finished answers=0 story=unchanged" || problem "run 2: the first scene should end Finished and change no story: $(last_line_with "$sl" 'scene ended run=1' || true)"
  log_has "$sl" "scene=ff7/md8_1/post-escape-station outcome=Finished answers=0 story=applied" || problem "run 2: the second scene should end Finished and apply its story: $(last_line_with "$sl" 'scene ended run=2' || true)"
  value=$(count_lines_with "$sl" "story advanced ")
  [[ $value == 1 ]] || problem "run 2: the server should have moved A's story once ($value steps)"
  log_has "$sl" "moment=33->36" || problem "run 2: the story should move 33 -> 36: $(last_line_with "$sl" 'story advanced' || true)"
  value=$(count_lines_with "$sl" "countdown ")
  [[ $value == 0 ]] || problem "run 2: the server said something about a countdown ($value lines)"
  value=$(count_lines_with "$sl" "story saved ")
  [[ $value == 1 ]] || problem "run 2: the server should have saved A's story once ($value lines)"
  stored=$(stored_blob "$WORK/server-state/progression/$account/Ff7.json")
  [[ $stored == "$RELOGIN_FINAL_BLOB" ]] || problem "run 2: the stored story is '$stored', expected $RELOGIN_FINAL_BLOB (moment 36, the reactor byte of the first run)"
}

main() {
  while (($#)); do
    case "$1" in
      --fixture) MODE=fixture ;;
      --chain) MODE=chain ;;
      --escape) MODE=escape ;;
      --story-extended-save) STORY_EXTENDED_SAVE=1 ;;
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
  if [[ $STORY_EXTENDED_SAVE == 1 ]]; then
    [[ $MODE == chain || $MODE == escape ]] || usage_error "--story-extended-save requires --chain or --escape"
    command -v python3 >/dev/null || usage_error "--story-extended-save requires python3"
  fi
  if [[ $MODE == chain && $DURATION_GIVEN == 0 ]]; then
    TRAVEL_DURATION=$CHAIN_DURATION
  fi
  if [[ $MODE == escape && $DURATION_GIVEN == 0 ]]; then
    TRAVEL_DURATION=$ESCAPE_DURATION
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

  if [[ $MODE == chain || $MODE == escape ]]; then
    run_chain
  else
    run_travel
  fi
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
