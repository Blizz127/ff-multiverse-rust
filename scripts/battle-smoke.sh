#!/usr/bin/env bash
# scripts/battle-smoke.sh - shared-battle smoke test for fmv-server + fmv-client (protocol v3).
#
# Usage: scripts/battle-smoke.sh [--fixture|--random] [--release|--debug] [--bin-dir DIR] [--build]
#                                [--library STATE_DIR] [--duration SECS] [--formation ID]
#                                [--tick-rate TPS] [--battle-speed N] [--bot idle|circle]
#                                [--keep-logs] [-h|--help]
#
# DEFAULT MODE (real data): the server hosts the real FF7 field md1stin from the owner's own
# imported library (read-only) with --debug-encounters, and two headless bots form a party and
# fight ONE shared battle:
#   A: --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party 2
#   B: --party-accept --battle-auto
# A (the party leader) asks for the encounter once both are in the party; the server starts one
# battle for both, steps it, and each bot replays it from its OWN scene.bin and kernel.bin and
# attacks whenever its gauge is full. Expected: every bot exits 0 and prints
#   SMOKE ... party=2 battles=1 victories=1
# both entered the SAME battle (same battle#N), both saw it end in Victory and return to the field,
# nobody reported a BATTLE DIVERGENCE, and the server logged one battle with two members that ended
# in Victory. Two further checks keep the SMOKE line from passing on the server's word alone:
#   * each bot's mirror REPLAYED the battle from its own scene.bin/kernel.bin and matched: its end
#     note says "verified: the server's N message(s) replayed ..." (a machine without battle data
#     says "NOT verified"), and its closing `headless: battles:` line counts 1 verified by replay;
#   * both members attacked: the server's "battle ended" line carries commands=[a, b] (accepted
#     commands per actor, both at least 1) and each bot counts a command of its own applied.
#
# --random: the same two bots, but WITHOUT --debug-encounters and with random encounters on: the
# bots (--bot circle, party formed, both --battle-auto) walk md1stin and the server rolls the
# field's own encounter table (rate 128; formations 301, 302, 303) as they walk. Expected: every bot
# exits 0, entered at least one battle, won every battle it entered, nobody reported a BATTLE
# DIVERGENCE, every battle it entered was replayed from its own battle data and matched (the count
# of "verified by replay" equals the count of battles entered), and at least one battle (the same
# battle#N) was shared by both bots. A bot that is in a battle when its duration runs out plays it
# to the end first, so the counts are never cut off mid-battle.
#
# --fixture: the synthetic FIXTURE server (no battle data) with --debug-encounters and one
# --battle-auto --debug-encounters bot. Expected: the request is refused cleanly ("this server
# cannot run battles"), the bot exits 0 and prints battles=0 victories=0.
#
# Isolation (the owner's state directory is never touched, only its LIBRARY is read): the server and
# the clients run with XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR
# pointing into a throw-away directory, the server with --ephemeral, each client with its own
# --state-dir inside it. The library is reached through a symlink `<work>/lib-state/library ->
# <STATE_DIR>/library` passed as --ff7-library, so nothing under STATE_DIR can be created or
# written. The script lists STATE_DIR before and after and fails if it changed.
#
# --library STATE_DIR is the directory that CONTAINS `library/` (default: $FMV_FF7_LIBRARY, else
# the owner's default state directory ~/.local/share/ff-multiverse).
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.
#
# This script reuses the helpers of scripts/smoke.sh (it sources it; sourcing runs nothing).

set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=smoke.sh
source "$SELF_DIR/smoke.sh"

# The sourced helpers print "smoke.sh:"; this script says who it is.
say() { printf 'battle-smoke.sh: %s\n' "$*"; }
err() { printf 'battle-smoke.sh: error: %s\n' "$*" >&2; }

MODE=real
LIBRARY="${FMV_FF7_LIBRARY:-$HOME/.local/share/ff-multiverse}"
BATTLE_DURATION=40
# md1stin's first random-encounter formation in the Steam build (two enemies): a starting party wins
# it every time and both members attack in every fight (checked over 200 seeds by
# crates/fmv-server/tests/battle_real.rs, `the_smoke_formations_are_won_every_time_...`).
FORMATION=301
TICK_RATE=240
BATTLE_SPEED=0
BOT=idle
PARTY_SIZE=2

usage() {
  cat <<'EOF'
Usage: scripts/battle-smoke.sh [--fixture|--random] [--release|--debug] [--bin-dir DIR] [--build]
                               [--library STATE_DIR] [--duration SECS] [--formation ID]
                               [--tick-rate TPS] [--battle-speed N] [--bot idle|circle]
                               [--keep-logs] [-h|--help]

Real-data mode (default): two headless bots form a party on the real FF7 field md1stin and fight
one shared battle started with --debug-encounters; passes when both report battles=1 victories=1
for the same battle, each replayed it from its own battle data ("verified by replay") and both
members attacked (the server's commands=[a, b] are both at least 1). --random: the same bots walk
md1stin without --debug-encounters and the server rolls the field's own encounter table (battles
start by themselves); every battle they enter must be verified by replay. --fixture: checks that
a FIXTURE server refuses battles cleanly.

  --library DIR     state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                     ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS    how long each bot runs (default 40)
  --formation ID     the enemy formation of the debug battle (default 301: a two-enemy formation of
                     md1stin's own encounter table in the Steam build)
  --tick-rate TPS    engine ticks per second of battle time (default 240)
  --battle-speed N   FF7 battle speed, 0 fastest (default 0)
  --bot KIND         idle (default) or circle: how the bots move while not in battle
EOF
}

# Locate the binaries the way smoke.sh does.
locate_binaries() {
  local repo_root target_dir
  repo_root=$(cd -- "$SELF_DIR/.." && pwd -P)
  if [[ -n $BIN_DIR ]]; then
    BIN_DIR=$(abs_path "$BIN_DIR")
  else
    if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
      target_dir=$(abs_path "$CARGO_TARGET_DIR")
      export CARGO_TARGET_DIR="$target_dir"
    else
      target_dir="$repo_root/target"
    fi
    BIN_DIR="$target_dir/$PROFILE"
  fi
  if ((DO_BUILD)); then
    local -a cargo_args=(build -p fmv-server -p fmv-client)
    if [[ $PROFILE == release ]]; then
      cargo_args+=(--release)
    fi
    say "building: cargo ${cargo_args[*]}"
    (cd -- "$repo_root" && cargo "${cargo_args[@]}") || {
      err "cargo build failed"
      exit 1
    }
  fi
  SERVER_BIN="$BIN_DIR/fmv-server"
  CLIENT_BIN="$BIN_DIR/fmv-client"
  local b
  for b in "$SERVER_BIN" "$CLIENT_BIN"; do
    if [[ ! -f $b || ! -x $b ]]; then
      err "executable not found: $b"
      err "build first (cargo build -p fmv-server -p fmv-client), pass --build, or point --bin-dir at both binaries"
      exit 1
    fi
  done
}

# wait_ready <log>: wait for the READY line and set READY_LINE / ADDR / CONNECT.
wait_ready() {
  local log="$1" deadline rc port
  deadline=$((SECONDS + READY_TIMEOUT))
  while :; do
    if READY_LINE=$(last_line_with "$log" "READY protocol="); then
      break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      rc=0
      wait "$SERVER_PID" || rc=$?
      SERVER_PID=""
      fail "the server exited (status $rc) before printing a READY line"
    fi
    if ((SECONDS >= deadline)); then
      fail "no READY line from the server within ${READY_TIMEOUT}s"
    fi
    sleep 0.1
  done
  READY_LINE=$(strip_ansi "$READY_LINE")
  ADDR=$(field "$READY_LINE" addr)
  if [[ ! $ADDR =~ ^(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9._-]+):([0-9]+)$ ]]; then
    fail "cannot parse addr='$ADDR' from the READY line: $READY_LINE"
  fi
  port="${BASH_REMATCH[2]}"
  [[ $port != 0 ]] || fail "the READY line reports port 0: $READY_LINE"
  case "$ADDR" in
    0.0.0.0:*) CONNECT="127.0.0.1:${ADDR#0.0.0.0:}" ;;
    "[::]:"*) CONNECT="[::1]:${ADDR#\[::\]:}" ;;
    *) CONNECT="$ADDR" ;;
  esac
}

# wait_clients <seconds>: wait for every client in CLIENT_NAMES (killing stragglers) and fill CRC.
wait_clients() {
  local limit="$1" pending=${#CLIENT_NAMES[@]} deadline n rc
  deadline=$((SECONDS + limit))
  while ((pending > 0)); do
    for n in "${CLIENT_NAMES[@]}"; do
      if [[ -n ${CPID[$n]:-} ]] && ! kill -0 "${CPID[$n]}" 2>/dev/null; then
        rc=0
        wait "${CPID[$n]}" || rc=$?
        CRC[$n]=$rc
        CPID[$n]=""
        pending=$((pending - 1))
      fi
    done
    ((pending > 0)) || break
    if ((SECONDS >= deadline)); then
      for n in "${CLIENT_NAMES[@]}"; do
        if [[ -n ${CPID[$n]:-} ]]; then
          stop_process "${CPID[$n]}" 3 || true
          CRC[$n]=timeout
          CPID[$n]=""
        fi
      done
      break
    fi
    sleep 0.2
  done
}

# log_has <file> <needle>: does any line contain needle?
log_has() {
  last_line_with "$1" "$2" >/dev/null 2>&1
}

# count_lines_with <file> <needle>: how many lines contain needle (pure bash, like last_line_with).
count_lines_with() {
  local file="$1" needle="$2" line n=0
  [[ -r $file ]] || {
    printf '0'
    return 0
  }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$needle"* ]]; then
      n=$((n + 1))
    fi
  done <"$file"
  printf '%s' "$n"
}

# stop_server: SIGTERM the server and record a problem if it does not exit 0.
stop_server() {
  local spid="$SERVER_PID" graceful=0 server_rc=0
  if kill -0 "$spid" 2>/dev/null; then
    say "stopping the server (SIGTERM)"
    if stop_process "$spid" "$SHUTDOWN_TIMEOUT"; then
      graceful=1
    fi
    server_rc=$STOP_RC
    SERVER_PID=""
    if [[ $graceful == 0 ]]; then
      problem "the server did not exit within ${SHUTDOWN_TIMEOUT}s of SIGTERM and had to be killed"
    elif [[ $server_rc != 0 ]]; then
      problem "the server exited with status $server_rc after SIGTERM (expected 0)"
    fi
  else
    wait "$spid" || server_rc=$?
    SERVER_PID=""
    problem "the server had already exited (status $server_rc) before the clients finished"
  fi
}

# battle_id_of <client log>: the battle#N a client says it started.
battle_id_of() {
  local line
  line=$(last_line_with "$1" "started: formation") || return 1
  line=$(strip_ansi "$line")
  if [[ $line =~ (battle#[0-9]+)\ started ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
}

# summary_number <client log> <regex>: the integer right in front of <regex> on the client's last
# `headless: battles:` line, e.g. summary_number log 'verified by replay' (empty if there is none).
summary_number() {
  local line
  line=$(last_line_with "$1" "headless: battles:") || return 0
  line=$(strip_ansi "$line")
  if [[ $line =~ ([0-9]+)\ $2 ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
}

# commands_of <server log line>: the per-actor counts of accepted commands the server logged when a
# battle ended (`commands=[2, 3]`), as "2 3" (empty if the line has none).
commands_of() {
  local line
  line=$(strip_ansi "$1")
  if [[ $line =~ commands=\[([0-9,\ ]*)\] ]]; then
    local counts="${BASH_REMATCH[1]//,/}"
    printf '%s' "$counts"
  fi
}

# check_mirror <name> <client log> <expected battles>: the proof that the client really REPLAYED the
# battles from its own scene.bin/kernel.bin and the replay matched (a mirror without battle data
# also ends battles, but says "NOT verified" and counts none as verified; the SMOKE line alone could
# not tell the two apart). <expected battles> is "any" (at least one) or a number.
check_mirror() {
  local n="$1" log="$2" want="$3" entered verified divergences
  entered=$(summary_number "$log" 'battle\(s\) entered')
  verified=$(summary_number "$log" 'verified by replay')
  divergences=$(summary_number "$log" 'divergence\(s\)')
  if [[ -z $entered || -z $verified || -z $divergences ]]; then
    problem "client $n printed no 'headless: battles:' summary with its verified-by-replay count"
    return 0
  fi
  if [[ $want == any ]]; then
    ((entered >= 1)) || problem "client $n entered no battle"
  elif ((entered != want)); then
    problem "client $n entered $entered battle(s), expected $want"
  fi
  ((verified == entered)) || problem "client $n: $entered battle(s) entered but only $verified verified by replay from its own battle data"
  ((divergences == 0)) || problem "client $n: $divergences divergence(s)"
  if log_has "$log" "NOT verified" || log_has "$log" "no battle data"; then
    problem "client $n says a battle was NOT verified or that it has no battle data: $(last_line_with "$log" "NOT verified" || last_line_with "$log" "no battle data")"
  fi
  log_has "$log" "; verified: the server's " || problem "client $n never logged a 'verified:' end note"
}

run_real() {
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

  local -a server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state"
    --debug-encounters --no-random-encounters --battle-speed "$BATTLE_SPEED" --battle-tick-rate "$TICK_RATE")
  server_args+=(--debug-formation "$FORMATION")
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  local area fixture
  area=$(field "$READY_LINE" area)
  fixture=$(field "$READY_LINE" fixture)
  [[ $area == Ff7/md1stin && $fixture == false ]] || fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"

  say "starting bots A and B for ${BATTLE_DURATION}s against $CONNECT (bots: $BOT)"
  local common=(--server "$CONNECT" --headless --duration "$BATTLE_DURATION" --bot "$BOT" --ff7-library "$WORK/lib-state")
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" \
    --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party "$PARTY_SIZE" \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" \
    --party-accept --battle-auto \
    >"$WORK/client-B.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((BATTLE_DURATION + CLIENT_GRACE))

  local n smoke_line value ids=()
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    local log="$WORK/client-$n.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "client $n was still running ${CLIENT_GRACE}s after its ${BATTLE_DURATION}s duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if smoke_line=$(last_line_with "$log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      for value in "party=2" "battles=1" "victories=1"; do
        [[ $smoke_line == *" $value"* ]] || problem "client $n: the SMOKE line lacks $value: $smoke_line"
      done
      [[ $(field "$smoke_line" fixture) == false ]] || problem "client $n: fixture is not false: $smoke_line"
    else
      problem "client $n printed no SMOKE line"
    fi
    log_has "$log" "ended in Victory" || problem "client $n never saw the battle end in Victory"
    log_has "$log" "back in the field" || problem "client $n never reported being back in the field"
    log_has "$log" "after the last battle:" || problem "client $n printed no evidence of play after the battle"
    if log_has "$log" "DIVERGENCE"; then
      problem "client $n reported a BATTLE DIVERGENCE (its replay disagreed with the server)"
    fi
    # The mirror replayed the battle from the client's OWN data (not merely "no divergence"), and
    # the client's own character really attacked: the server applied at least one command of its.
    check_mirror "$n" "$log" 1
    value=$(summary_number "$log" 'command\(s\) of ours applied')
    [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "client $n: the server applied none of its commands (command(s) of ours applied: '${value}')"
    ids+=("$(battle_id_of "$log" || true)")
  done
  if [[ -z ${ids[0]} || ${ids[0]} != "${ids[1]}" ]]; then
    problem "the bots did not enter the same battle (A: '${ids[0]}', B: '${ids[1]}')"
  fi

  log_has "$WORK/server.log" "battle started" || problem "the server never logged a battle start"
  log_has "$WORK/server.log" "members=2" || problem "the server's battle did not have two members"
  if ! log_has "$WORK/server.log" "battle ended"; then
    problem "the server never logged the end of a battle"
  else
    local ended
    ended=$(last_line_with "$WORK/server.log" "battle ended" || true)
    [[ $ended == *Victory* ]] || problem "the server's battle did not end in Victory: $ended"
    # Both members attacked: the server's own count of accepted commands per actor, e.g. [2, 3].
    local counts actor_counts=() c
    counts=$(commands_of "$ended")
    read -r -a actor_counts <<<"$counts"
    ((${#actor_counts[@]} == 2)) || problem "the server's battle-end line has no two-member commands=[..]: $ended"
    for c in "${actor_counts[@]}"; do
      ((c >= 1)) || problem "a member of the battle never attacked (server: commands per actor [$counts])"
    done
  fi
  (($(count_lines_with "$WORK/server.log" "battle started") <= 1)) || problem "the server started more than one battle"

  stop_server
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "client: $l"
  done
  say "server: $(last_line_with "$WORK/server.log" "battle started" || true)"
  say "server: $(last_line_with "$WORK/server.log" "battle ended" || true)"
  for n in "${CLIENT_NAMES[@]}"; do
    say "client $n: $(last_line_with "$WORK/client-$n.log" "started: formation" || true)"
    say "client $n: $(last_line_with "$WORK/client-$n.log" "ended in Victory" || true)"
    say "client $n: $(last_line_with "$WORK/client-$n.log" "headless: battles:" || true)"
    say "client $n: $(last_line_with "$WORK/client-$n.log" "after the last battle:" || true)"
  done
  RESULT=pass
  say "PASS: two bots fought one shared battle on the real field, both attacked, each replayed it from its own battle data and won it; $state_real unchanged ($after)"
}

# all_battle_ids_of <client log>: every distinct battle#N a client says it started, one per line.
all_battle_ids_of() {
  local line seen=" "
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"started: formation"* && $line =~ (battle#[0-9]+)\ started ]]; then
      if [[ $seen != *" ${BASH_REMATCH[1]} "* ]]; then
        seen+="${BASH_REMATCH[1]} "
        printf '%s\n' "${BASH_REMATCH[1]}"
      fi
    fi
  done <"$1"
}

run_random() {
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

  # No --debug-encounters and no --no-random-encounters: only the field's own table starts battles.
  local -a server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state"
    --battle-speed "$BATTLE_SPEED" --battle-tick-rate "$TICK_RATE")
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin ]] || fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"

  say "starting bots A and B for ${BATTLE_DURATION}s against $CONNECT (they walk; battles come from the field's encounter table)"
  local common=(--server "$CONNECT" --headless --duration "$BATTLE_DURATION" --bot circle --ff7-library "$WORK/lib-state" --battle-auto)
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" --party-invite-first-peer \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" --party-accept \
    >"$WORK/client-B.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((BATTLE_DURATION + CLIENT_GRACE))

  local n smoke_line battles victories
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    local log="$WORK/client-$n.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "client $n was still running ${CLIENT_GRACE}s after its ${BATTLE_DURATION}s duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if smoke_line=$(last_line_with "$log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      battles=$(field "$smoke_line" battles)
      victories=$(field "$smoke_line" victories)
      if [[ ! $battles =~ ^[0-9]+$ || ! $victories =~ ^[0-9]+$ ]]; then
        problem "client $n: the SMOKE line has no numeric battles/victories: $smoke_line"
      elif ((battles < 1)); then
        problem "client $n entered no battle in ${BATTLE_DURATION}s of walking (battles=$battles): $smoke_line"
      elif ((victories != battles)); then
        problem "client $n won only $victories of $battles battles: $smoke_line"
      fi
    else
      problem "client $n printed no SMOKE line"
    fi
    if log_has "$log" "DIVERGENCE"; then
      problem "client $n reported a BATTLE DIVERGENCE (its replay disagreed with the server)"
    fi
    # Every battle it entered was replayed from its own battle data and matched.
    check_mirror "$n" "$log" any
  done
  # At least one battle was the SAME battle for both (the party fought together).
  local shared=0 id b_ids
  b_ids=" $(all_battle_ids_of "$WORK/client-B.log" | tr '\n' ' ')"
  while IFS= read -r id; do
    if [[ -n $id && $b_ids == *" $id "* ]]; then
      shared=$((shared + 1))
    fi
  done < <(all_battle_ids_of "$WORK/client-A.log")
  ((shared >= 1)) || problem "the bots never shared a battle"
  log_has "$WORK/server.log" "members=2" || problem "the server never started a battle for the whole party"

  stop_server
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"
  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "client: $l"
  done
  say "server: $(count_lines_with "$WORK/server.log" "battle started") battle(s) started, $(count_lines_with "$WORK/server.log" "battle ended") ended; $shared shared by both bots"
  for n in "${CLIENT_NAMES[@]}"; do
    say "client $n: $(last_line_with "$WORK/client-$n.log" "headless: battles:" || true)"
  done
  RESULT=pass
  say "PASS: random encounters from the real field's table started battles that the walking party won; $state_real unchanged ($after)"
}

run_fixture() {
  mkdir -p -- "$WORK/state-env" "$WORK/client-A"
  export FMV_STATE_DIR="$WORK/state-env"
  say "starting FIXTURE server: $SERVER_BIN --bind 127.0.0.1:0 --ephemeral --debug-encounters"
  "$SERVER_BIN" --bind 127.0.0.1:0 --ephemeral --debug-encounters >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" fixture) == true ]] || fail "the server is not the FIXTURE server: $READY_LINE"
  CLIENT_NAMES=(A)
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration 6 --name A --state-dir "$WORK/client-A" \
    --battle-auto --debug-encounters >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  wait_clients $((6 + CLIENT_GRACE))
  [[ ${CRC[A]:-} == 0 ]] || problem "the bot exited with status ${CRC[A]:-unknown} (a refused battle must not fail it)"
  local smoke_line
  if smoke_line=$(last_line_with "$WORK/client-A.log" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
    [[ $smoke_line == *" battles=0 victories=0" ]] || problem "the SMOKE line does not end in battles=0 victories=0: $smoke_line"
    SMOKE_LINES=("$smoke_line")
  else
    problem "the bot printed no SMOKE line"
  fi
  log_has "$WORK/client-A.log" "request refused: this server cannot run battles" || problem "the bot did not log the clear refusal"
  log_has "$WORK/server.log" "no effect" || problem "the server did not warn that --debug-encounters has no effect without battle data"
  stop_server
  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  say "client: ${SMOKE_LINES[0]}"
  say "client A: $(last_line_with "$WORK/client-A.log" "request refused" || true)"
  RESULT=pass
  say "PASS: the FIXTURE server refused the battle request cleanly; the bot kept going and exited 0"
}

main() {
  while (($#)); do
    case "$1" in
      --fixture) MODE=fixture ;;
      --random) MODE=random ;;
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
        BATTLE_DURATION="$2"
        shift
        ;;
      --formation)
        (($# >= 2)) || usage_error "--formation needs a formation id"
        FORMATION="$2"
        shift
        ;;
      --tick-rate)
        (($# >= 2)) || usage_error "--tick-rate needs a number"
        TICK_RATE="$2"
        shift
        ;;
      --battle-speed)
        (($# >= 2)) || usage_error "--battle-speed needs a number"
        BATTLE_SPEED="$2"
        shift
        ;;
      --bot)
        (($# >= 2)) || usage_error "--bot needs idle or circle"
        BOT="$2"
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
  [[ $BATTLE_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  [[ $TICK_RATE =~ ^[1-9][0-9]*$ ]] || usage_error "--tick-rate must be a positive whole number"
  [[ $BATTLE_SPEED =~ ^[0-9]+$ ]] || usage_error "--battle-speed must be a whole number"
  [[ $FORMATION =~ ^[0-9]+$ ]] || usage_error "--formation must be a whole number"
  [[ $BOT == idle || $BOT == circle ]] || usage_error "--bot must be idle or circle"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-battle-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  case "$MODE" in
    fixture) run_fixture ;;
    random) run_random ;;
    *) run_real ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
