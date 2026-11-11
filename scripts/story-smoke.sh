#!/usr/bin/env bash
# scripts/story-smoke.sh - the STORY AFTER THE ESCAPE over REAL processes and REAL data: the two scenes the first reactor's
# escape ends in (the team at the tunnel mouth, `md8_4`, and on the street, `md8_1`), which start by themselves, and the
# login that brings a story back to where it stands.
#
# Usage: scripts/story-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR] [--echo-s FILE]
#                               [--duration SECS] [--keep-logs] [-h|--help]
#
# WHAT IT PROVES (the owner's own files read-only, temp state only):
#   * an account whose story stands right after the escape (moment 33) comes into the world outside the reactor, in
#     `md8_4`, not where a new account joins: the server sends it on at login (`story entry ... to=md8_4`);
#   * the scene of `md8_4` starts BY ITSELF for that player, plays to its end from the player's own files, changes nothing
#     that is saved, and ends in the field's own jump: the player is carried to `md8_1`;
#   * the scene of `md8_1` starts by itself there, plays to its end, and the story moves from 33 to 36, ONCE, and is SAVED;
#   * with --echo-s, a player alone in a scene is shown Echo-S's edition of it (`script: echo-s`), otherwise the original;
#   * the server restarts on the same state directory (without the debug flag); the same account logs in again and comes
#     in on the street of `md8_1` (`story entry ... to=md8_1`), no scene replays, and it walks (its moves are applied).
#
# HOW an account gets to moment 33 in seconds: session 1's server runs with --debug-start-moment 33 on a throw-away state
# directory (the flag is refused on the default one), so the new account starts where the escape ends instead of playing the
# whole mission first (scripts/travel-smoke.sh --escape is the run that plays it). Everything after that is the real path.
#
# Output the script reads is ids and counts only: the game's own text is never printed or compared here.
#
# Isolation: as scripts/scene-smoke.sh (whose helpers this script uses; sourcing runs nothing): the owner's state directory
# is never touched, only its LIBRARY is read through a symlink in a throw-away directory; the server and the client get
# their own state directories inside it; the script lists STATE_DIR before and after and fails if it changed.
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.

set -euo pipefail

STORY_SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=scene-smoke.sh
source "$STORY_SELF_DIR/scene-smoke.sh"

say() { printf 'story-smoke.sh: %s\n' "$*"; }
err() { printf 'story-smoke.sh: error: %s\n' "$*" >&2; }

ECHO_S=""
STORY_DURATION=25
NAME_A="StoryA"
CLIENT_NAMES=(A)
SCENE_1="ff7/md8_4/post-escape"
SCENE_2="ff7/md8_1/post-escape-station"

story_usage() {
  cat <<'EOF'
Usage: scripts/story-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR] [--echo-s FILE]
                              [--duration SECS] [--keep-logs] [-h|--help]

Plays the two scenes the first reactor's escape ends in with one real headless player over the owner's own FF7 files
(an account started at story moment 33 on a throw-away server), checks that the story moves to 36 once and is saved,
then restarts the server and checks that the account comes back in where its story stands and nothing replays.

  --library DIR    state directory that contains library/ (default $FMV_FF7_LIBRARY, else ~/.local/share/ff-multiverse);
                   only the library is read, never written
  --echo-s FILE    the owner's Echo-S archive (read-only): the scenes are then expected in Echo-S's edition
  --duration SECS  how long the bot of session 1 runs at most (default 25)
EOF
}

# start_story_server <log> <port> [extra server args...]
start_story_server() {
  local log="$1" port="$2"
  shift 2
  local -a args=(--bind "127.0.0.1:$port" --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters "$@")
  say "starting server: $SERVER_BIN ${args[*]}"
  "$SERVER_BIN" "${args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == "Ff7/md1stin" && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"
}

# note_field <line> <key>: the value after "<key>=" up to the next space (the client's scene notes are `key=value` words).
note_field() {
  local line="$1" key="$2" rest
  [[ $line == *"$key="* ]] || return 0
  rest=${line#*"$key="}
  printf '%s' "${rest%% *}"
}

run_story() {
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
  # A headless bot is silent unless told otherwise; say so explicitly, so no run of this script can ever play a sound.
  export FMV_AUDIO=null
  [[ $(realpath -- "$WORK") == /tmp/* || $(realpath -- "$WORK") == "${TMPDIR:-/tmp}"/* ]] || fail "the work directory is not a temporary one: $WORK"
  local real_state
  real_state=$(realpath -m -- "$HOME/.local/share/ff-multiverse")
  case "$(realpath -- "$WORK")" in "$real_state"*) fail "the work directory is inside the real state directory: $WORK" ;; esac

  local -a mods=(--mods off)
  local want_script="original"
  if [[ -n $ECHO_S ]]; then
    [[ -r $ECHO_S ]] || fail "--echo-s: cannot read $ECHO_S"
    mods=(--echo-s "$ECHO_S")
    want_script="echo-s"
  fi

  # ---- session 1: an account at moment 33 comes in outside the reactor and the two scenes play ----
  start_story_server "$WORK/s1-server.log" 0 --debug-start-moment 33
  local port="${ADDR##*:}" sl="$WORK/s1-server.log"
  log_has "$sl" "--debug-start-moment 33 is on" || problem "the server did not warn that the debug start moment is on"
  log_has "$sl" "the extended save is off" || problem "the server did not say that the extended save is off"
  start_player A r1 "$STORY_DURATION" "$NAME_A" --bot idle "${mods[@]}"
  wait_clients $((STORY_DURATION + CLIENT_GRACE))
  local la="$WORK/r1-client-A.log"
  judge_exit r1 A

  # what the client played
  local started1 started2 ended1 ended2
  started1=$(strip_ansi "$(last_line_with "$la" "scene: started run=1 " || true)")
  started2=$(strip_ansi "$(last_line_with "$la" "scene: started run=2 " || true)")
  ended1=$(strip_ansi "$(last_line_with "$la" "scene: ended run=1 " || true)")
  ended2=$(strip_ansi "$(last_line_with "$la" "scene: ended run=2 " || true)")
  [[ $started1 == *" $SCENE_1 as owner with 1 participant(s), script: $want_script"* ]] || problem "the first scene did not start as expected on the client: '${started1:-nothing}'"
  [[ $started2 == *" $SCENE_2 as owner with 1 participant(s), script: $want_script"* ]] || problem "the second scene did not start as expected on the client: '${started2:-nothing}'"
  expect_equal "the first scene's outcome on the client" "$(note_field "$ended1" outcome)" "Finished"
  expect_equal "the second scene's outcome on the client" "$(note_field "$ended2" outcome)" "Finished"
  local w1 w2
  w1=$(note_field "$ended1" windows)
  w2=$(note_field "$ended2" windows)
  [[ $w1 =~ ^[0-9]+$ && $w1 -ge 3 ]] || problem "the first scene showed '$w1' windows (the team talks: at least three)"
  [[ $w2 =~ ^[0-9]+$ && $w2 -ge 4 ]] || problem "the second scene showed '$w2' windows (at least four)"
  expect_equal "scenes started on the client" "$(count_lines_with "$la" "scene: started run=")" "2"
  if log_has "$la" "scene: cannot play"; then
    problem "the client could not play a scene: $(last_line_with "$la" "scene: cannot play" || true)"
  fi
  local smoke_line
  if smoke_line=$(last_line_with "$la" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
    expect_equal "where the player ends session 1" "$(field "$smoke_line" area)" "Ff7/md8_1"
  else
    problem "client A printed no SMOKE line in session 1"
  fi

  # what the server saw
  # Registration persists the initial story before joining the world, so assert the
  # bytes delivered to the client rather than the old world-side seeding log.
  log_has "$la" "joined with progress 012100" || problem "the new account did not receive the debug story at moment 33"
  log_has "$sl" "to=md8_4 (where its story stands)" || problem "the account was not sent to md8_4 at login: $(last_line_with "$sl" "story entry" || true)"
  expect_equal "the server's scene starts" "$(count_lines_with "$sl" "scene started run=")" "2"
  expect_equal "the server's scene ends" "$(count_lines_with "$sl" "scene ended run=")" "2"
  local end1 end2
  end1=$(strip_ansi "$(last_line_with "$sl" "scene ended run=1 " || true)")
  end2=$(strip_ansi "$(last_line_with "$sl" "scene ended run=2 " || true)")
  [[ $end1 == *"scene=$SCENE_1 outcome=Finished"*"story=unchanged"* ]] || problem "the first scene should end Finished and change no story: '$end1'"
  [[ $end2 == *"scene=$SCENE_2 outcome=Finished"*"story=applied"* ]] || problem "the second scene should end Finished and apply its story: '$end2'"
  expect_equal "story steps the server applied" "$(count_lines_with "$sl" "story advanced account=")" "1"
  local advanced
  advanced=$(strip_ansi "$(last_line_with "$sl" "story advanced account=" || true)")
  [[ $advanced == *"moment=33->36"* ]] || problem "the story should move from 33 to 36: '$advanced'"
  if log_has "$sl" "NOT SAVED"; then
    problem "the server could not save: $(last_line_with "$sl" "NOT SAVED" || true)"
  fi
  if log_has "$sl" "scene destination unavailable" || log_has "$sl" "story entry unavailable"; then
    problem "a place of the story could not be hosted: $(last_line_with "$sl" "unavailable" || true)"
  fi

  stop_server "session 1"
  local digest_after_1
  digest_after_1=$(tree_digest "$WORK/server-state")
  [[ -n $digest_after_1 ]] || problem "the server stored nothing at all (not even the account)"

  # ---- session 2: the server restarts WITHOUT the debug flag; the account comes back in where its story stands ----
  start_story_server "$WORK/s2-server.log" "$port"
  [[ ${ADDR##*:} == "$port" ]] || fail "the restarted server is not on port $port (the client's saved token is keyed by the address): $READY_LINE"
  local s2="$WORK/s2-server.log"
  if log_has "$s2" "debug-start-moment"; then
    problem "the restarted server still has the debug start moment on"
  fi
  start_player A r2 10 "$NAME_A" --bot circle "${mods[@]}"
  wait_clients $((10 + CLIENT_GRACE))
  local l2="$WORK/r2-client-A.log"
  judge_exit r2 A
  log_has "$s2" "to=md8_1 (where its story stands)" || problem "the returning account was not sent to md8_1: $(last_line_with "$s2" "story entry" || true)"
  if log_has "$s2" "starts with the debug story"; then
    problem "the returning account was given the debug story again"
  fi
  if log_has "$s2" "scene started run=" || log_has "$l2" "scene: started"; then
    problem "a scene replayed after the restart: $(last_line_with "$s2" "scene started run=" || true)"
  fi
  if log_has "$s2" "story advanced account="; then
    problem "the story moved again after the restart: $(last_line_with "$s2" "story advanced account=" || true)"
  fi
  if smoke_line=$(last_line_with "$l2" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
    expect_equal "where the returning player stands" "$(field "$smoke_line" area)" "Ff7/md8_1"
    expect_equal "moves refused after the restart (nothing is stuck)" "$(field "$smoke_line" rejected)" "0"
    [[ $(field "$smoke_line" updates) =~ ^[0-9]+$ && $(field "$smoke_line" updates) -ge 1 ]] || problem "the player saw no updates after the restart (stuck?): $smoke_line"
  else
    problem "client A printed no SMOKE line after the restart"
  fi
  stop_server "session 2"
  expect_equal "a login after the restart rewrote something in the state directory" "$(tree_digest "$WORK/server-state")" "$digest_after_1"

  # ---- the real state directory is untouched ----
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  expect_equal "the contents of $state_real" "$after" "$before"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  RESULT=pass
  say "STORY-SMOKE scenes=[$SCENE_1 $SCENE_2] script=$want_script windows=[$w1 $w2] story=33->36 saved=once relogin=md8_1 replayed=0"
  say "PASS"
}

story_main() {
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
      --echo-s)
        (($# >= 2)) || usage_error "--echo-s needs a file"
        ECHO_S="$2"
        shift
        ;;
      --duration)
        (($# >= 2)) || usage_error "--duration needs a number of seconds"
        STORY_DURATION="$2"
        shift
        ;;
      --keep-logs) KEEP_LOGS=1 ;;
      -h | --help)
        story_usage
        return 0
        ;;
      *) usage_error "unknown argument: $1" ;;
    esac
    shift
  done
  [[ $STORY_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-story-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  run_story
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  story_main "$@"
fi
