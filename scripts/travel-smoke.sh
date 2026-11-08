#!/usr/bin/env bash
# scripts/travel-smoke.sh - area-travel smoke test for fmv-server + fmv-client (real FF7 data).
#
# Usage: scripts/travel-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
#                                [--duration SECS] [--keep-logs] [-h|--help]
#
# The server hosts the real FF7 field md1stin from the owner's own imported library (read-only) and
# serves every other field its exits lead to (they are loaded when the first player walks there).
# Two headless bots form a party in md1stin and walk through the exits:
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
#   * the NPCs of md1stin were announced (`headless: N NPC(s) in Ff7/md1stin: ...`, N >= 1) and so
#     were md1_1's;
#   * the server logged the load of md1_1 ("area loaded") and no bot reported a BATTLE DIVERGENCE.
# Random encounters are off (--no-random-encounters): a bot without --battle-auto would stand frozen in
# the first battle for good, and this smoke is about walking.
#
# Isolation (the owner's state directory is never touched, only its LIBRARY is read): the server and the
# clients run with XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR
# pointing into a throw-away directory, the server with --state-dir inside it, each client with its own
# --state-dir. The library is reached through a symlink `<work>/lib-state/library -> <STATE_DIR>/library`
# passed as --ff7-library, so nothing under STATE_DIR can be created or written. The script lists
# STATE_DIR before and after and fails if it changed.
#
# --library STATE_DIR is the directory that CONTAINS `library/` (default: $FMV_FF7_LIBRARY, else the
# owner's default state directory ~/.local/share/ff-multiverse).
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

usage() {
  cat <<'EOF'
Usage: scripts/travel-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
                               [--duration SECS] [--keep-logs] [-h|--help]

Two headless bots form a party on the real FF7 field md1stin, both walk through its gateway to md1_1
(the server watches the exit line and moves them; each client loads md1_1 from its own copy), see each
other there, and one walks back to md1stin. Passes when both exit 0 with area_changes >= 1 (A ends in
md1stin with area_changes 2, B in md1_1 with area_changes 1 and a peer seen), both were in a party of
two, the NPCs of the fields were announced and the server loaded md1_1.

  --library DIR     state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                     ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS    how long each bot runs (default 45)
EOF
}

# area_changes_of <SMOKE line>: the number in area_changes=N (empty if absent).
area_changes_of() {
  field "$1" area_changes
}

run_travel() {
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

  local -a server_args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters)
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  local area fixture
  area=$(field "$READY_LINE" area)
  fixture=$(field "$READY_LINE" fixture)
  [[ $area == Ff7/md1stin && $fixture == false ]] || fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"

  say "starting bots A and B for ${TRAVEL_DURATION}s against $CONNECT"
  local common=(--server "$CONNECT" --headless --duration "$TRAVEL_DURATION" --bot circle --ff7-library "$WORK/lib-state")
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" \
    --party-invite-first-peer --travel md1_1,md1stin \
    >"$WORK/client-A.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" \
    --party-accept --travel md1_1 \
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
      [[ $(field "$smoke_line" fixture) == false ]] || problem "client $n: fixture is not false: $smoke_line"
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
    # The NPCs of the field it started in were announced (the snapshot of an area carries them).
    log_has "$log" "NPC(s) in Ff7/md1stin" || problem "client $n was never told of any NPC in md1stin"
  done

  # A: back in md1stin after two changes, and it saw B in md1_1 before leaving it.
  if [[ -n ${LINE[A]:-} ]]; then
    [[ $(field "${LINE[A]}" area) == Ff7/md1stin ]] || problem "A should end in Ff7/md1stin: ${LINE[A]}"
    [[ $(area_changes_of "${LINE[A]}") == 2 ]] || problem "A should have changed area twice (there and back): ${LINE[A]}"
    local leaving
    leaving=$(last_line_with "$WORK/client-A.log" "peer(s) were seen in \`Ff7/md1_1\`" || true)
    if [[ -z $leaving ]]; then
      problem "A never logged leaving md1_1"
    elif [[ $(strip_ansi "$leaving") =~ ([0-9]+)\ peer\(s\)\ were\ seen\ in\ \`Ff7/md1_1\` ]] && ((BASH_REMATCH[1] >= 1)); then
      say "A saw ${BASH_REMATCH[1]} peer(s) in md1_1 before it went back"
    else
      problem "A saw nobody in md1_1: $leaving"
    fi
  fi
  # B: stays in md1_1 and saw A there.
  if [[ -n ${LINE[B]:-} ]]; then
    [[ $(field "${LINE[B]}" area) == Ff7/md1_1 ]] || problem "B should end in Ff7/md1_1: ${LINE[B]}"
    [[ $(area_changes_of "${LINE[B]}") == 1 ]] || problem "B should have changed area once: ${LINE[B]}"
    value=$(field "${LINE[B]}" peers_seen)
    [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "B saw nobody in md1_1 (peers_seen=${value:-?}): ${LINE[B]}"
  fi
  log_has "$WORK/client-B.log" "NPC(s) in Ff7/md1_1" || say "note: B was told of no NPC in md1_1 (the field may place none unconditionally)"

  log_has "$WORK/server.log" "area loaded" || problem "the server never loaded another area"
  log_has "$WORK/server.log" "area=Ff7/md1_1" || problem "the server never loaded md1_1"
  log_has "$WORK/server.log" "player leaves through an exit" || problem "the server never moved a player through an exit"

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
  for n in "${CLIENT_NAMES[@]}"; do
    say "client $n: $(last_line_with "$WORK/client-$n.log" "NPC(s) in Ff7/md1stin" || true)"
    say "client $n: $(last_line_with "$WORK/client-$n.log" "travel: entering" || true)"
  done
  say "server: $(last_line_with "$WORK/server.log" "area loaded" || true)"
  RESULT=pass
  say "PASS: two bots in a party walked md1stin -> md1_1 through the gateway, saw each other there, and one walked back; $state_real unchanged ($after)"
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
        TRAVEL_DURATION="$2"
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

  run_travel
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
