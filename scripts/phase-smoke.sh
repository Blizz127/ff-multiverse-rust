#!/usr/bin/env bash
# scripts/phase-smoke.sh - story phasing smoke test for fmv-server + fmv-client (real FF7 data).
#
# Usage: scripts/phase-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
#                               [--duration SECS] [--keep-logs] [-h|--help]
#
# The server hosts the real FF7 field md1stin from the owner's own imported library (read-only), with
# a throw-away state directory. Every account starts in the SAME default phase; only advancing the story
# separates players. The story point used is the one md1stin's own script has: while the story counter is
# 1 (a new account), the player's character standing on walkmesh triangle 12 sets it to 6 (see
# crates/fmv-ff7/src/story.rs). The server validates it from the player's accepted moves.
#
# HOW A HEADLESS BOT TRIGGERS IT (what a caller such as a slice smoke runs):
#   server: fmv-server --bind 127.0.0.1:0 --ff7-library LIBSTATE --state-dir TEMP --no-random-encounters
#   bot:    fmv-client --server HOST:PORT --headless --name NAME --state-dir DIR --ff7-library LIBSTATE \
#                      --duration 40 --story-advance
# (A new account's story is at the counter the point needs. The bot walks from its spawn to triangle 12 of
# md1stin, never crosses an exit, and stands there until the server moves the story. The server runs with
# --no-random-encounters because md1stin has an encounter table and a bot without --battle-auto that is
# caught in a battle stands frozen until its run ends; a caller that wants battles on the same server drops
# that flag and gives the bot --battle-auto.)
# The line the SERVER prints once, when it happens (key=value, one per advance):
#   story advanced account=<n> entity=<n> area=Ff7/md1stin point=0 moment=1->6
# followed, when the write reached the store, by
#   story saved account=<n> bytes=3 progress=010600
# and the CLIENT prints (stderr):  story: advanced point=0 moment=1->6 (progress 010600)
# plus the placement lines (`party: placement: Ff7/md1stin (solo, story advance allowed, phase 4d0200)`)
# and the timeline `headless: peers in view: N`.
#
# Run 1 (server on a fresh state dir):
#   A: --bot idle --story-advance --party-invite-first-peer        (invites B while they still see each other,
#                                                                   then walks to triangle 12 and advances)
#   B: --bot idle --party-accept --party-accept-after-split        (stands still; accepts A's invite only once
#                                                                   A is no longer in view)
# Expected:
#   * A and B first see each other (peers in view: 1), A advances, they stop seeing each other (0), B joins A's
#     party and is placed in A's instance, so they see each other again (1);
#   * the server logged exactly ONE `story advanced ... moment=1->6` (A's), two instances of md1stin were created
#     (the default phase and the advanced one) and each player moved between instances;
#   * A's placements: solo+default, then solo+advanced phase, then the party's with the advanced phase and "story
#     advance allowed"; B's: solo+default, then the party's with A's (advanced) phase and "not allowed" (B keeps its
#     own progression: its stored story is untouched).
# Then the server is stopped (SIGTERM), which saves what is queued, and started again on the same state dir.
# Run 2 (both clients log in again with the tokens they saved; nobody is in a party any more):
#   A and B: --bot idle (no story flags)
# Expected: the phases persisted. A is placed in the advanced phase (and was told its story at join:
#   `story: joined with progress 010600`), B in the default one, and they do NOT see each other (peers_seen=0),
#   although both are in md1stin; the server did not advance anybody again.
# Every bot exits 0 and prints a SMOKE line. Nothing in the owner's state directory is created or changed.
#
# Isolation (the owner's state directory is never touched, only its LIBRARY is read): the server and the
# clients run with XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR pointing
# into a throw-away directory, the server with --state-dir inside it, each client with its own --state-dir.
# The library is reached through a symlink `<work>/lib-state/library -> <STATE_DIR>/library` passed as
# --ff7-library. The script lists STATE_DIR before and after and fails if it changed.
#
# --library STATE_DIR is the directory that CONTAINS `library/` (default: $FMV_FF7_LIBRARY, else the
# owner's default state directory ~/.local/share/ff-multiverse).
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
say() { printf 'phase-smoke.sh: %s\n' "$*"; }
err() { printf 'phase-smoke.sh: error: %s\n' "$*" >&2; }

# Debug builds are the default of this script (the sourced ones default to release).
PROFILE=debug
LIBRARY="${FMV_FF7_LIBRARY:-$HOME/.local/share/ff-multiverse}"
PHASE_DURATION=40
RECONNECT_DURATION=8
AREA=Ff7/md1stin

usage() {
  cat <<'EOF'
Usage: scripts/phase-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
                              [--duration SECS] [--keep-logs] [-h|--help]

Two headless bots on the real FF7 field md1stin: A advances its story (walks onto walkmesh triangle 12, which the
server verifies) and B does not, so they stop seeing each other in that field; B joins A's party and is placed in
A's instance; then the server restarts and both log in again, and the phases persisted (A in the advanced phase, B
in the default one, apart). Passes when every bot exits 0 and the checks in the header of this script all hold.

  --debug / --release   which build to use (default debug)
  --build               run cargo build -p fmv-server -p fmv-client first (not with --bin-dir)
  --library DIR         state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                         ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS       how long each bot of run 1 runs (default 40)
EOF
}

usage_error() {
  err "$*"
  usage >&2
  exit 2
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
  local seq="$1" want v
  shift
  local -a rest=($seq)
  local i=0
  for want in "$@"; do
    while ((i < ${#rest[@]})) && [[ ${rest[$i]} != "$want" ]]; do i=$((i + 1)); done
    ((i < ${#rest[@]})) || return 1
    i=$((i + 1))
  done
  return 0
}

# placements_of <client log>: the text after `placement: ` of every placement line, one per line, in order.
placements_of() {
  local log="$1" line
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"placement: "* ]]; then
      line=$(strip_ansi "$line")
      printf '%s\n' "${line#*placement: }"
    fi
  done <"$log"
}

# phase_of <placement text>: the phase named in `(..., phase X)`, or empty.
phase_of() {
  local text="$1"
  if [[ $text =~ phase\ ([0-9a-f]+|default)\) ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
}

# start_server <log> [bind]: start the server on the shared state dir (on `bind`, default any free port) and set
# CONNECT. A client keeps the token of an account per server ADDRESS, so run 2 must come back to run 1's port.
start_server() {
  local log="$1" bind="${2:-127.0.0.1:0}"
  local -a server_args=(--bind "$bind" --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters)
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == "$AREA" && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting $AREA from real data: $READY_LINE"
}

# client_args <name> <duration> <extra>...: run a bot and remember its pid.
start_client() {
  local n="$1" duration="$2" log="$3"
  shift 3
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration "$duration" --name "$n" --state-dir "$WORK/client-$n" \
    --ff7-library "$WORK/lib-state" "$@" >"$log" 2>&1 &
  CPID[$n]=$!
}

# read_smoke <name> <log>: record the SMOKE line of a client (and that it exited 0).
read_smoke() {
  local n="$1" log="$2" line
  if [[ ${CRC[$n]:-} == timeout ]]; then
    problem "client $n was still running ${CLIENT_GRACE}s after its duration and was killed"
  elif [[ ${CRC[$n]:-} != 0 ]]; then
    problem "client $n exited with status ${CRC[$n]:-unknown}"
  fi
  if line=$(last_line_with "$log" "SMOKE name="); then
    line=$(strip_ansi "$line")
    SMOKE[$n]="$line"
    [[ $(field "$line" fixture) == false ]] || problem "client $n: fixture is not false: $line"
    [[ $(field "$line" area) == "$AREA" ]] || problem "client $n should end in $AREA: $line"
  else
    problem "client $n printed no SMOKE line"
  fi
}

# the hex of the progression blob stored for the account of a server-state progression dir, or empty.
stored_blob() {
  local file="$1" text
  [[ -r $file ]] || return 0
  text=$(<"$file")
  if [[ $text =~ \"blob\"[[:space:]]*:[[:space:]]*\"([0-9a-f]*)\" ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
}

declare -A SMOKE=()

run_phase() {
  local state_real before after n line value
  state_real="$LIBRARY"
  [[ -d $state_real/library/sets ]] || {
    err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
    exit 1
  }
  before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A" "$WORK/client-B"
  ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
  export FMV_STATE_DIR="$WORK/state-env"

  # ---------------------------------------------------------------- run 1: advance, split, party
  start_server "$WORK/server-1.log"
  local first_addr="$CONNECT"
  say "run 1: bots A and B for ${PHASE_DURATION}s against $CONNECT"
  start_client A "$PHASE_DURATION" "$WORK/client-A-1.log" --bot idle --story-advance --party-invite-first-peer
  start_client B "$PHASE_DURATION" "$WORK/client-B-1.log" --bot idle --party-accept --party-accept-after-split
  wait_clients $((PHASE_DURATION + CLIENT_GRACE))
  for n in A B; do read_smoke "$n" "$WORK/client-$n-1.log"; done
  local smoke_a="${SMOKE[A]:-}" smoke_b="${SMOKE[B]:-}"

  # The server's story lines: exactly one advance, in the right place, and it was queued for saving.
  local advanced
  advanced=$(count_lines_with "$WORK/server-1.log" "story advanced ")
  [[ $advanced == 1 ]] || problem "the server logged $advanced 'story advanced' lines in run 1 (expected exactly 1)"
  line=$(last_line_with "$WORK/server-1.log" "story advanced " || true)
  line=$(strip_ansi "$line")
  [[ $(field "$line" area) == "$AREA" && $(field "$line" point) == 0 && $(field "$line" moment) == "1->6" ]] ||
    problem "the story line is not md1stin point 0 moment=1->6: $line"
  local advancing_account
  advancing_account=$(field "$line" account)
  say "server: $line"
  log_has "$WORK/server-1.log" "instance created" || problem "the server never logged an instance"
  local created moved
  created=$(count_lines_with "$WORK/server-1.log" "instance created ")
  ((created >= 2)) || problem "the server created $created instance(s) of $AREA (expected the default and the advanced phase: 2)"
  moved=$(count_lines_with "$WORK/server-1.log" "player moved to another instance")
  ((moved >= 2)) || problem "$moved player move(s) between instances (expected A's advance and B's joining A's party: 2)"

  # A's own view: it advanced.
  log_has "$WORK/client-A-1.log" "story: advanced point=0 moment=1->6" || problem "client A never logged 'story: advanced point=0 moment=1->6'"
  log_has "$WORK/client-B-1.log" "story: advanced" && problem "client B logged a story advance (it must not advance)"

  # The timeline of who sees whom: together, apart, together again (the party puts B in A's instance).
  local seq_a seq_b
  seq_a=$(in_view_of "$WORK/client-A-1.log")
  seq_b=$(in_view_of "$WORK/client-B-1.log")
  say "A: peers in view over time: $seq_a"
  say "B: peers in view over time: $seq_b"
  has_in_order "$seq_a" 1 0 1 || problem "A's view should go 1, 0, 1 (together, apart after its advance, together in the party): '$seq_a'"
  has_in_order "$seq_b" 1 0 1 || problem "B's view should go 1, 0, 1: '$seq_b'"

  # The placements: phases follow the story, then the leader's.
  local -a pa=() pb=()
  mapfile -t pa < <(placements_of "$WORK/client-A-1.log")
  mapfile -t pb < <(placements_of "$WORK/client-B-1.log")
  say "A placements: ${#pa[@]}; B placements: ${#pb[@]}"
  local pa_first="${pa[0]:-}" pb_first="${pb[0]:-}"
  [[ $(phase_of "$pa_first") == default && $pa_first == *solo* ]] || problem "A's first placement should be solo in the default phase: '$pa_first'"
  [[ $(phase_of "$pb_first") == default && $pb_first == *solo* ]] || problem "B's first placement should be solo in the default phase: '$pb_first'"
  # A's advanced phase: the first placement of A that is solo and not the default one (right after its advance).
  local phase_a="" p
  for p in "${pa[@]+"${pa[@]}"}"; do
    if [[ $p == *solo* && $(phase_of "$p") != default && -z $phase_a ]]; then
      phase_a=$(phase_of "$p")
    fi
  done
  [[ -n $phase_a ]] || problem "A should have been placed solo in an advanced phase right after advancing: ${pa[*]:-none}"
  # Then the party: A (the leader) is allowed to advance in it, B (placed by A) is not, and both are in A's phase.
  # (The LAST placements are not looked at: when A's run ends and A leaves, B is placed on its own again.)
  local a_in_party=0 b_in_party=0
  for p in "${pa[@]+"${pa[@]}"}"; do
    [[ $p == *party* && $p == *"story advance allowed"* && $(phase_of "$p") == "$phase_a" ]] && a_in_party=1
  done
  for p in "${pb[@]+"${pb[@]}"}"; do
    [[ $p == *party* && $p == *"story advance not allowed"* && $(phase_of "$p") == "$phase_a" ]] && b_in_party=1
  done
  ((a_in_party)) || problem "A (the leader) should have been placed in the party in its phase $phase_a, allowed to advance: ${pa[*]:-none}"
  ((b_in_party)) || problem "B should have been placed in A's party in A's phase $phase_a, not allowed to advance there: ${pb[*]:-none}"
  for n in A B; do
    local sl="${SMOKE[$n]:-}"
    [[ -z $sl ]] && continue
    value=$(field "$sl" party)
    [[ $value =~ ^[0-9]+$ ]] && ((value >= 2)) || problem "client $n was never in a party of two (party=${value:-?}): $sl"
    value=$(field "$sl" peers_seen)
    [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "client $n never saw the other (peers_seen=${value:-?}): $sl"
  done

  stop_server
  # The advance was saved (stop_server waits for the saver): A's record holds the story, B has none.
  log_has "$WORK/server-1.log" "story saved account=$advancing_account bytes=3 progress=010600" ||
    problem "the server never logged 'story saved account=$advancing_account bytes=3 progress=010600'"
  local stored="" f dir blob stored_accounts=0
  for dir in "$WORK"/server-state/progression/*/; do
    [[ -d $dir ]] || continue
    f="${dir}Ff7.json"
    blob=$(stored_blob "$f")
    if [[ -n $blob ]]; then
      stored_accounts=$((stored_accounts + 1))
      stored="$blob"
      [[ ${dir%/} == */"$advancing_account" ]] || problem "a story was stored for another account than the one that advanced: $f"
    fi
  done
  [[ $stored_accounts == 1 && $stored == 010600 ]] || problem "expected exactly one stored story (A's, 010600), found $stored_accounts (last: '$stored')"

  # ---------------------------------------------------------------- run 2: reconnect, the phases persist
  start_server "$WORK/server-2.log" "$first_addr"
  say "run 2: A and B log in again for ${RECONNECT_DURATION}s against $CONNECT"
  start_client A "$RECONNECT_DURATION" "$WORK/client-A-2.log" --bot idle
  start_client B "$RECONNECT_DURATION" "$WORK/client-B-2.log" --bot idle
  wait_clients $((RECONNECT_DURATION + CLIENT_GRACE))
  for n in A B; do read_smoke "$n" "$WORK/client-$n-2.log"; done
  local -a qa=() qb=()
  mapfile -t qa < <(placements_of "$WORK/client-A-2.log")
  mapfile -t qb < <(placements_of "$WORK/client-B-2.log")
  local qa_first="${qa[0]:-}" qb_first="${qb[0]:-}"
  [[ $(phase_of "$qa_first") == "$phase_a" && $qa_first == *solo* ]] || problem "after reconnecting A should be solo in the advanced phase $phase_a: '$qa_first'"
  [[ $(phase_of "$qb_first") == default && $qb_first == *solo* ]] || problem "after reconnecting B should be solo in the default phase: '$qb_first'"
  log_has "$WORK/client-A-2.log" "story: joined with progress 010600" || problem "client A was not told its story (progress 010600) when it logged in again"
  log_has "$WORK/client-B-2.log" "story: " && problem "client B was told a story: it never advanced: $(last_line_with "$WORK/client-B-2.log" "story: " || true)"
  for n in A B; do
    local sl="${SMOKE[$n]:-}"
    [[ -z $sl ]] && continue
    value=$(field "$sl" peers_seen)
    [[ $value == 0 ]] || problem "client $n saw somebody after reconnecting although they are in different phases (peers_seen=${value:-?}): $sl"
  done
  [[ $(count_lines_with "$WORK/server-2.log" "story advanced ") == 0 ]] || problem "the server advanced somebody's story in run 2"
  stop_server

  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  say "client A run 1: $smoke_a"
  say "client B run 1: $smoke_b"
  for n in A B; do
    say "client $n run 2: ${SMOKE[$n]:-}"
  done
  say "A placements: ${pa[*]}"
  say "B placements: ${pb[*]}"
  say "after reconnecting: A: $qa_first | B: $qb_first"
  RESULT=pass
  say "PASS: A advanced md1stin's story (moment=1->6, saved as 010600) and B did not, so they stopped seeing each other; B joined A's party and was placed in A's instance; after a server restart and both logging in again the phases persisted (A $phase_a, B default) and they did not see each other; $state_real unchanged ($after)"
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
        PHASE_DURATION="$2"
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
  [[ $PHASE_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-phase-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  run_phase
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
