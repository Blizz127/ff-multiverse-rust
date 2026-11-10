#!/usr/bin/env bash
# scripts/resume-smoke.sh - MMO-STYLE SAVING over REAL processes and REAL data: a player that leaves the game comes back
# on the spot it left, in the field it left, after the server itself has restarted.
#
# Usage: scripts/resume-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR] [--keep-logs] [-h|--help]
#
# WHAT IT PROVES (the owner's own files read-only, temp state only):
#   * session 1: a new account joins in md1stin like everyone, walks through md1stin's exit into md1_1 and its client
#     ends there; the server saves where it stood (`progression/<id>/Ff7.location.json`: md1_1, its position, its story);
#   * the server restarts on the same state directory;
#   * session 2: the same account logs in and is put back on that spot (`resumed account=... area=md1_1 at=(x,y,z)`,
#     the saved position), its client ends in md1_1, not in md1stin where a login used to put everyone;
#   * nothing but the saved spot changed in the state directory across session 2 (a login is no story step).
# (`scripts/travel-smoke.sh --escape` is the run that proves a story whose entry is elsewhere still goes there: its
# re-login at moment 33 lands in md8_4, not in nrthmk where its client ended.)
#
# Isolation: as scripts/scene-smoke.sh (whose helpers this script uses): the owner's state directory is never touched,
# only its LIBRARY is read through a symlink in a throw-away directory; the script lists it before and after.
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.

set -euo pipefail

RESUME_SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=scene-smoke.sh
source "$RESUME_SELF_DIR/scene-smoke.sh"

say() { printf 'resume-smoke.sh: %s\n' "$*"; }
err() { printf 'resume-smoke.sh: error: %s\n' "$*" >&2; }

NAME_A="ResumeA"
CLIENT_NAMES=(A)

resume_usage() {
  cat <<'EOF'
Usage: scripts/resume-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR] [--keep-logs] [-h|--help]

One real headless player walks from md1stin into md1_1 and stops; the server restarts; the player logs in again and
must come back in md1_1 on the spot it saved (MMO-style saving), with nothing else of the state directory changed.
EOF
}

# start_resume_server <log> <port>
start_resume_server() {
  local log="$1" port="$2"
  local -a args=(--bind "127.0.0.1:$port" --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters)
  say "starting server: $SERVER_BIN ${args[*]}"
  "$SERVER_BIN" "${args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == "Ff7/md1stin" && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"
}

# saved_spot <file>: "area x y z" of a location record (whole units), from the JSON the store wrote.
saved_spot() {
  local file="$1"
  [[ -f $file ]] || return 1
  python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["area"], *[round(v) for v in r["pos"]])' "$file"
}

run_resume() {
  local state_real="$LIBRARY"
  [[ -d $state_real/library/sets ]] || {
    err "no library under $state_real (needs owned FF7 content imported with fmv-import); pass --library STATE_DIR"
    exit 1
  }
  local before after
  before=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  mkdir -p -- "$WORK/lib-state" "$WORK/state-env" "$WORK/server-state" "$WORK/client-A"
  ln -s -- "$(cd -- "$state_real" && pwd -P)/library" "$WORK/lib-state/library"
  export FMV_STATE_DIR="$WORK/state-env"
  export FMV_AUDIO=null
  [[ $(realpath -- "$WORK") == /tmp/* || $(realpath -- "$WORK") == "${TMPDIR:-/tmp}"/* ]] || fail "the work directory is not a temporary one: $WORK"

  # ---- session 1: a new account walks into md1_1 and leaves there ----
  start_resume_server "$WORK/s1-server.log" 0
  local port="${ADDR##*:}" s1="$WORK/s1-server.log"
  start_player A r1 40 "$NAME_A" --travel md1_1 --mods off
  wait_clients $((40 + CLIENT_GRACE))
  judge_exit r1 A
  local smoke_line
  if smoke_line=$(last_line_with "$WORK/r1-client-A.log" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
    expect_equal "where the player ends session 1" "$(field "$smoke_line" area)" "Ff7/md1_1"
  else
    problem "client A printed no SMOKE line in session 1"
  fi
  stop_server "session 1"
  local spot_file spot1
  spot_file=$(find "$WORK/server-state/progression" -name 'Ff7.location.json' | head -1)
  spot1=$(saved_spot "$spot_file" || true)
  [[ $spot1 == "md1_1 "* ]] || problem "the server saved no spot in md1_1 when the player left (got '${spot1:-nothing}' in '${spot_file:-no file}')"
  local digest_after_1
  digest_after_1=$(tree_digest "$WORK/server-state")

  # ---- session 2: the server restarts; the account comes back on its spot ----
  start_resume_server "$WORK/s2-server.log" "$port"
  [[ ${ADDR##*:} == "$port" ]] || fail "the restarted server is not on port $port (the client's saved token is keyed by the address): $READY_LINE"
  local s2="$WORK/s2-server.log"
  start_player A r2 10 "$NAME_A" --bot idle --mods off
  wait_clients $((10 + CLIENT_GRACE))
  judge_exit r2 A
  local resumed
  resumed=$(strip_ansi "$(last_line_with "$s2" "resumed account=" || true)")
  [[ $resumed == *" area=md1_1 at=("* ]] || problem "the returning account was not put back in md1_1: '${resumed:-$(last_line_with "$s2" "saved spot" || echo nothing)}'"
  # The saved spot and the one it was put back on are the same (whole units: the line prints whole units).
  local x y z
  read -r _ x y z <<<"$spot1"
  [[ $resumed == *"at=($x,$y,$z)"* ]] || problem "the account was put back at another spot than the saved ($x,$y,$z): '$resumed'"
  if smoke_line=$(last_line_with "$WORK/r2-client-A.log" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
    expect_equal "where the returning player stands" "$(field "$smoke_line" area)" "Ff7/md1_1"
    expect_equal "moves refused after the restart (nothing is stuck)" "$(field "$smoke_line" rejected)" "0"
  else
    problem "client A printed no SMOKE line after the restart"
  fi
  if log_has "$s2" "story advanced account="; then
    problem "the story moved at the re-login: $(last_line_with "$s2" "story advanced account=" || true)"
  fi
  stop_server "session 2"
  expect_equal "a login rewrote something other than the saved spot" "$(tree_digest "$WORK/server-state")" "$digest_after_1"
  local spot2
  spot2=$(saved_spot "$spot_file" || true)
  [[ $spot2 == "md1_1 "* ]] || problem "after session 2 the saved spot is no longer in md1_1: '${spot2:-nothing}'"

  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  expect_equal "the contents of $state_real" "$after" "$before"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  RESULT=pass
  say "RESUME-SMOKE left=md1_1 at=($x,$y,$z) restarted=yes back=md1_1 resumed=\"$resumed\""
  say "PASS"
}

resume_main() {
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
      --keep-logs) KEEP_LOGS=1 ;;
      -h | --help)
        resume_usage
        return 0
        ;;
      *) usage_error "unknown argument: $1" ;;
    esac
    shift
  done
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-resume-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  run_resume
}

resume_main "$@"
