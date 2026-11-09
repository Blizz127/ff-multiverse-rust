#!/usr/bin/env bash
# scripts/scene-smoke.sh - a STORY SCENE over REAL processes and REAL data (the proof: Cloud and Tifa at the bar of 7th Heaven).
#
# Usage: scripts/scene-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
#                               [--answer LINE] [--duration SECS] [--keep-logs] [-h|--help]
#
# WHAT IT PROVES (the acceptance of the scene proof, with the owner's own files read-only and temp state only):
#   * a player's OK next to the scene's trigger starts a scene for that player (the debug preview puts the bot in the field
#     `mds7pb_1` at the scene's own story moment, 72, on a MADE-UP story: the server runs with --debug-scene-preview);
#   * the canon lines play in order: the game's own field script (read by each client from its OWN files) opens the windows,
#     the dialog indexes, who speaks (Tifa 20, Cloud 18) and which window asks are the proof scene's known ones, in order;
#   * a second player, a member of the first one's party, watches the same scene in step: it prints the same lines in the same
#     order and finishes with the owner;
#   * the player is held while it runs and CONTROL RETURNS when it ends (each bot sends one move afterwards and the server applies it);
#   * the server restarts on the same state directory; the same player logs in again: no scene replays, nothing is stuck,
#     and NOTHING of the preview was ever stored (no story record exists for either account, and the state directory is byte
#     for byte what the first session left after the second one).
#
# WHAT IT DOES:
#   Session 1, one server (--debug-scene-preview, a persistent --state-dir inside the throw-away work dir):
#       A  "SceneA"  --party-invite-first-peer --scene-preview ff7/mds7pb_1/tifa-bar --scene-watchers 1 --scene-answer 2
#       B  "SceneB"  --party-accept            --scene-preview ff7/mds7pb_1/tifa-bar --scene-watch
#   A invites B while they still see each other in md1stin, both then preview the scene (B, a member, is placed in the
#   instance of A's preview: its leader's), A waits until B is in its field and presses OK next to Tifa, the scene starts
#   for A (owner) and B (watcher), both play it from their own files, A answers the question with line 2 ('something
#   hard': the long way, eleven windows), and both finish.
#   Session 2: the server is stopped (SIGTERM, exit 0) and started again on the same state directory; A logs in again with
#   its saved token and walks (`--bot circle`): it must see no scene, no story (a new account's) and every move must be applied.
#
# Output the script reads is ids and counts only (`scene line index=.. opener=.. speaker=.. dialog=.. asks=..`): the game's
# own text is game data and is never printed or compared here.
#
# Isolation: the owner's state directory is never touched, only its LIBRARY is read. The server and the clients run with
# XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME and FMV_STATE_DIR pointing into a throw-away directory, the
# server with a --state-dir of its own inside it (persistent across the restart), each client with its own --state-dir. The
# library is reached through a symlink `<work>/lib-state/library -> <STATE_DIR>/library` passed as --ff7-library. The script
# lists STATE_DIR before and after and fails if it changed.
#
# --library STATE_DIR is the directory that CONTAINS `library/` (default $FMV_FF7_LIBRARY, else ~/.local/share/ff-multiverse).
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, logs are kept), 2 = usage.
#
# This script reuses the helpers of scripts/smoke.sh (it sources it; sourcing runs nothing).

set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=smoke.sh
source "$SELF_DIR/smoke.sh"

say() { printf 'scene-smoke.sh: %s\n' "$*"; }
err() { printf 'scene-smoke.sh: error: %s\n' "$*" >&2; }

LIBRARY="${FMV_FF7_LIBRARY:-$HOME/.local/share/ff-multiverse}"
SCENE_ID="ff7/mds7pb_1/tifa-bar"
ANSWER=2
SCENE_DURATION=45
# Reading the whole field archive takes a while on a busy box.
READY_TIMEOUT="${FMV_SMOKE_READY_TIMEOUT:-90}"
CLIENT_NAMES=(A B)
NAME_A="SceneA"
NAME_B="SceneB"

# What the proof scene is, measured on the owner's data (fmv-ff7 tests/scene_real.rs pins the same numbers): answering
# line 2 ("something hard") gives eleven windows, the second of them the question; Tifa (script entity 20) and Cloud (18)
# speak, the windows Cloud's name is on being the 5th, 8th and 10th.
EXPECTED_DIALOGS_2="28 29 31 32 33 34 35 36 37 38 39"
EXPECTED_SPEAKERS_2="20 20 20 20 18 20 20 18 20 18 20"
# Answering line 1 ("I don't feel like it") gives six.
EXPECTED_DIALOGS_1="28 29 30 37 38 39"
EXPECTED_SPEAKERS_1="20 20 20 20 18 20"

usage() {
  cat <<'EOF'
Usage: scripts/scene-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--library STATE_DIR]
                              [--answer 1|2] [--duration SECS] [--keep-logs] [-h|--help]

Plays the proof scene (Cloud and Tifa at the bar of 7th Heaven) with two real headless players over the owner's own
FF7 files: the owner and a party member who watches it, then restarts the server and checks that nothing replays,
nothing is stuck and nothing of the debug preview was stored. See the header of this script.

  --library DIR    state directory that contains library/ (default $FMV_FF7_LIBRARY, else ~/.local/share/ff-multiverse);
                   only the library is read, never written
  --answer N       the line the owner answers the question with: 2 (default, the long way: eleven windows) or 1 (six)
  --duration SECS  how long each bot of session 1 runs at most (default 45)
EOF
}

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
    local -a cargo_args=(build --offline -p fmv-server -p fmv-client)
    if [[ $PROFILE == release ]]; then
      cargo_args+=(--release)
    fi
    say "building: RUSTC_WRAPPER= cargo ${cargo_args[*]}"
    (cd -- "$repo_root" && RUSTC_WRAPPER= cargo "${cargo_args[@]}") || {
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
  local log="$1" deadline rc
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
  [[ $ADDR =~ ^([A-Za-z0-9._-]+):([0-9]+)$ ]] || fail "cannot parse addr='$ADDR' from the READY line: $READY_LINE"
  [[ ${BASH_REMATCH[2]} != 0 ]] || fail "the READY line reports port 0: $READY_LINE"
  CONNECT="$ADDR"
}

# wait_clients <seconds>: wait for every client in CLIENT_NAMES (killing stragglers) and fill CRC.
wait_clients() {
  local limit="$1" pending=0 deadline n rc
  for n in "${CLIENT_NAMES[@]}"; do
    [[ -n ${CPID[$n]:-} ]] && pending=$((pending + 1))
  done
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
log_has() { last_line_with "$1" "$2" >/dev/null 2>&1; }

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

# scene_field <client log> <key>: the value of <key> on every `scene line` line of the log, in order, space separated.
scene_field() {
  local log="$1" key="$2" line out=() v
  [[ -r $log ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line == *"scene line index="* ]] || continue
    line=$(strip_ansi "$line")
    v=$(field "$line" "$key")
    out+=("${v:-?}")
  done <"$log"
  printf '%s' "${out[*]:-}"
}

# expect_equal <what> <got> <want>: a problem unless they are equal.
expect_equal() {
  [[ $2 == "$3" ]] || problem "$1: got '$2', expected '$3'"
}

# stop_server <label>: SIGTERM the server and record a problem if it does not exit 0.
stop_server() {
  local label="$1" spid="$SERVER_PID" graceful=0
  if [[ -z $spid ]]; then
    return 0
  fi
  say "stopping the server (SIGTERM) after $label"
  if stop_process "$spid" "$SHUTDOWN_TIMEOUT"; then
    graceful=1
  fi
  SERVER_PID=""
  if [[ $graceful != 1 ]]; then
    problem "$label: the server did not stop on SIGTERM within ${SHUTDOWN_TIMEOUT}s"
  elif [[ $STOP_RC != 0 ]]; then
    problem "$label: the server exited with status $STOP_RC on SIGTERM (expected 0)"
  fi
}

# start_server <log>: the real server over the owner's library, the debug scene preview on.
start_server() {
  local log="$1"
  local -a args=(--bind 127.0.0.1:0 --ff7-library "$WORK/lib-state" --state-dir "$WORK/server-state" --no-random-encounters --debug-scene-preview)
  say "starting server: $SERVER_BIN ${args[*]}"
  "$SERVER_BIN" "${args[@]}" >"$log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == "Ff7/md1stin" && $(field "$READY_LINE" fixture) == false ]] ||
    fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"
}

# start_player <label A|B> <run> <duration> <name> <extra client args...>: a headless player with its own state dir.
start_player() {
  local label="$1" run="$2" duration="$3" name="$4"
  shift 4
  "$CLIENT_BIN" --server "$CONNECT" --headless --duration "$duration" --bot idle --ff7-library "$WORK/lib-state" \
    --name "$name" --state-dir "$WORK/client-$label" "$@" >"$WORK/$run-client-$label.log" 2>&1 &
  CPID[$label]=$!
}

# tree_digest <dir>: every file under <dir> (except lock files, which change with a login) and its sha256, sorted: the
# state of a directory as bytes.
tree_digest() {
  local dir="$1"
  (cd -- "$dir" && find . -type f ! -iname '*lock*' -print0 | sort -z | xargs -0 -r sha256sum)
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

# check_scene_client <label> <role> <log> <want dialogs> <want speakers>: what one bot printed about the scene.
check_scene_client() {
  local n="$1" role="$2" log="$3" want_dialogs="$4" want_speakers="$5" started ended restored count
  count=$(wc -w <<<"$want_dialogs")
  started=$(last_line_with "$log" "scene started " || true)
  started=$(strip_ansi "$started")
  if [[ -z $started ]]; then
    problem "client $n never printed 'scene started' (no scene reached it)"
    return 0
  fi
  expect_equal "client $n role" "$(field "$started" role)" "$role"
  expect_equal "client $n scene" "$(field "$started" scene)" "$SCENE_ID"
  expect_equal "client $n participants (the owner and its watcher)" "$(field "$started" participants)" "2"
  expect_equal "client $n dialogs, in order" "$(scene_field "$log" dialog)" "$want_dialogs"
  expect_equal "client $n speakers, in order" "$(scene_field "$log" speaker)" "$want_speakers"
  expect_equal "client $n windows" "$(count_lines_with "$log" "scene line index=")" "$count"
  # exactly one window asks: the second
  local want_asks="false true" i
  for ((i = 3; i <= count; i++)); do
    want_asks+=" false"
  done
  expect_equal "client $n which windows ask" "$(scene_field "$log" asks)" "$want_asks"
  ended=$(last_line_with "$log" "scene ended " || true)
  ended=$(strip_ansi "$ended")
  if [[ -z $ended ]]; then
    problem "client $n never printed 'scene ended'"
  else
    expect_equal "client $n outcome" "$(field "$ended" outcome)" "Finished"
    expect_equal "client $n lines" "$(field "$ended" lines)" "$count"
  fi
  restored=$(last_line_with "$log" "scene control restored" || true)
  restored=$(strip_ansi "$restored")
  if [[ -z $restored ]]; then
    problem "client $n never printed 'scene control restored' (control did not come back, or was never checked)"
  else
    expect_equal "client $n control after the scene (a move was applied)" "$(field "$restored" moved)" "true"
  fi
  if log_has "$log" "scene refused" || log_has "$log" "scene unavailable"; then
    problem "client $n was refused a scene or could not build it: $(last_line_with "$log" "scene " || true)"
  fi
}

run_scene() {
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
  [[ $(realpath -- "$WORK") == /tmp/* || $(realpath -- "$WORK") == "${TMPDIR:-/tmp}"/* ]] || fail "the work directory is not a temporary one: $WORK"
  local real_state
  real_state=$(realpath -m -- "$HOME/.local/share/ff-multiverse")
  case "$(realpath -- "$WORK")" in "$real_state"*) fail "the work directory is inside the real state directory: $WORK" ;; esac

  local want_dialogs want_speakers
  if [[ $ANSWER == 2 ]]; then
    want_dialogs="$EXPECTED_DIALOGS_2"
    want_speakers="$EXPECTED_SPEAKERS_2"
  else
    want_dialogs="$EXPECTED_DIALOGS_1"
    want_speakers="$EXPECTED_SPEAKERS_1"
  fi

  # ---- session 1: the owner and its watcher play the scene ----
  start_server "$WORK/s1-server.log"
  log_has "$WORK/s1-server.log" "--debug-scene-preview is on" || problem "the server did not warn that the debug scene preview is on"
  start_player A r1 "$SCENE_DURATION" "$NAME_A" --party-invite-first-peer --scene-preview "$SCENE_ID" --scene-watchers 1 --scene-answer "$ANSWER" --scene-wait-secs 60
  start_player B r1 "$SCENE_DURATION" "$NAME_B" --party-accept --scene-preview "$SCENE_ID" --scene-watch --scene-wait-secs 60
  wait_clients $((SCENE_DURATION + CLIENT_GRACE))
  local la="$WORK/r1-client-A.log" lb="$WORK/r1-client-B.log" sl="$WORK/s1-server.log" n
  for n in A B; do
    judge_exit r1 "$n"
  done
  check_scene_client A Owner "$la" "$want_dialogs" "$want_speakers"
  check_scene_client B Watcher "$lb" "$want_dialogs" "$want_speakers"
  # the same lines, in the same order, for both
  expect_equal "the watcher saw the owner's dialogs" "$(scene_field "$lb" dialog)" "$(scene_field "$la" dialog)"
  expect_equal "the watcher saw the owner's speakers" "$(scene_field "$lb" speaker)" "$(scene_field "$la" speaker)"
  # the owner's answer reached the scene (and the watcher saw the same branch)
  expect_equal "the owner's answer" "$(field "$(strip_ansi "$(last_line_with "$la" "scene line index=1 " || true)")" answer)" "$ANSWER"

  # what the server saw
  local started ended
  started=$(last_line_with "$sl" "scene started run=" || true)
  started=$(strip_ansi "$started")
  expect_equal "the server's scene starts" "$(count_lines_with "$sl" "scene started run=")" "1"
  expect_equal "the server's scene ends" "$(count_lines_with "$sl" "scene ended run=")" "1"
  expect_equal "the scene" "$(field "$started" scene)" "$SCENE_ID"
  expect_equal "it was a preview (made-up story)" "$(field "$started" preview)" "true"
  [[ $started == *"watchers=[0-9]"* || $started =~ watchers=\[[0-9]+\] ]] || problem "the server's scene had no watcher: $started"
  ended=$(last_line_with "$sl" "scene ended run=" || true)
  ended=$(strip_ansi "$ended")
  expect_equal "the server's outcome" "$(field "$ended" outcome)" "Finished"
  expect_equal "the owner's answer reached the server (one answer)" "$(field "$ended" answers)" "1"
  [[ $ended == *"story=unchanged"* ]] || problem "the proof scene writes nothing the story carries, but the server says: $ended"
  if log_has "$sl" "story advanced"; then
    problem "the server logged a story step during the preview: $(last_line_with "$sl" "story advanced" || true)"
  fi
  if log_has "$sl" "NOT SAVED"; then
    problem "the server could not save: $(last_line_with "$sl" "NOT SAVED" || true)"
  fi
  log_has "$sl" "scene preview entity=" || problem "the server never logged a scene preview"

  stop_server "session 1"
  # NOTHING of a preview is stored: no story record exists for either account
  local records
  records=$(find "$WORK/server-state" -path '*progression*' -type f 2>/dev/null | wc -l)
  expect_equal "story records stored for the two accounts" "$records" "0"
  local digest_after_1
  digest_after_1=$(tree_digest "$WORK/server-state")
  [[ -n $digest_after_1 ]] || problem "the server stored nothing at all (not even the accounts)"

  # ---- session 2: the server restarts; A logs in again ----
  start_server "$WORK/s2-server.log"
  CLIENT_NAMES=(A)
  start_player A r2 10 "$NAME_A" --bot circle
  wait_clients $((10 + CLIENT_GRACE))
  local l2="$WORK/r2-client-A.log" smoke_line
  judge_exit r2 A
  if log_has "$l2" "scene started" || log_has "$l2" "scene line"; then
    problem "a scene replayed after the restart: $(last_line_with "$l2" "scene " || true)"
  fi
  if log_has "$l2" "story: joined with progress"; then
    problem "the account was told a story at login, but a preview stores none: $(last_line_with "$l2" "story: joined with progress" || true)"
  fi
  if smoke_line=$(last_line_with "$l2" "SMOKE name="); then
    smoke_line=$(strip_ansi "$smoke_line")
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
  say "SCENE-SMOKE scene=$SCENE_ID answer=$ANSWER lines=$(wc -w <<<"$want_dialogs") dialogs=[$(scene_field "$la" dialog)] speakers=[$(scene_field "$la" speaker)] owner=A watcher=B outcome=Finished control_restored=true restart_replays=0 stored_story_records=0"
  say "PASS"
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
      --answer)
        (($# >= 2)) || usage_error "--answer needs 1 or 2"
        ANSWER="$2"
        shift
        ;;
      --duration)
        (($# >= 2)) || usage_error "--duration needs a number of seconds"
        SCENE_DURATION="$2"
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
  [[ $ANSWER == 1 || $ANSWER == 2 ]] || usage_error "--answer must be 1 or 2"
  [[ $SCENE_DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  locate_binaries

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-scene-smoke.XXXXXX")
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  run_scene
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
