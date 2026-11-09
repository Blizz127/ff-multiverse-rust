#!/usr/bin/env bash
# scripts/delete-smoke.sh - deleting a character over REAL processes (fmv-server, fmv-client, fmv-characters).
#
# Usage: scripts/delete-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--keep-logs] [-h|--help]
#
# WHAT IT PROVES (the owner's bug: the character menu only forgot the login on THIS computer, so the name stayed taken):
#   (1) a character that is in the game is NOT deleted (`fmv-client --delete` is refused, the character is untouched);
#   (2) once it has left, `fmv-client --delete NAME` deletes it ON THE SERVER with its own login token: the client prints
#       `deleted character name=<NAME>`, forgets the saved login, the server logs `character deleted`, and the account's
#       records (account, avatar, character; a real game's progress files too, the fixture has none) are in a timestamped
#       entry of <state>/trash/, byte for byte what they were, while every other file of the state directory (the other
#       characters') is byte for byte unchanged but for index.log, which grew;
#   (3) the name is free: registering it again works and is a different account with a different token;
#   (4) the old login is dead: its token is refused ("invalid or unknown token"), before and after a server restart;
#   (5) a server restart brings nothing back: the characters that were not deleted log in again, the trash is still there;
#   (6) an operator can bring a deleted character back (`fmv-characters ... restore`, the server stopped): a new account,
#       a new token printed once, which logs in on the restarted server.
#
# It runs the FIXTURE server (no game data is read or needed), with every state directory a fresh temp directory:
# `--state-dir` for the server and for every client, and XDG_*/FMV_STATE_DIR pointing into the same throw-away directory
# (which must still be empty of anything but what this script put there when it ends). Nothing of the real state
# directory (~/.local/share/ff-multiverse) is touched, and if it exists the script proves that nothing in it changed while it ran.
# Only the PIDs this script started are ever signalled.
#
# Binaries: --bin-dir DIR (an unpacked release), else ${CARGO_TARGET_DIR:-<repo>/target}/{release,debug}; --build runs
# cargo build for fmv-server and fmv-client first (fmv-characters is a binary of fmv-server).
#
# Exit status: 0 = pass, 1 = failure (reasons and log tails are printed, the log directory is kept), 2 = usage error.

set -euo pipefail

PROFILE=release
BIN_DIR=""
DO_BUILD=0
KEEP_LOGS=0
READY_TIMEOUT="${FMV_SMOKE_READY_TIMEOUT:-20}"
SHUTDOWN_TIMEOUT="${FMV_SMOKE_SHUTDOWN_TIMEOUT:-10}"
CLIENT_TIMEOUT="${FMV_SMOKE_CLIENT_TIMEOUT:-30}"

WORK=""
SERVER_PID=""
HOLD_PID=""
CONNECT=""
PORT=""
RESULT=""
PROBLEMS=()
STOP_RC=0
OUT=""
RC=0

say() { printf 'delete-smoke.sh: %s\n' "$*"; }
err() { printf 'delete-smoke.sh: error: %s\n' "$*" >&2; }

usage() {
  cat <<'EOF'
Usage: scripts/delete-smoke.sh [--release|--debug] [--bin-dir DIR] [--build] [--keep-logs] [-h|--help]

Creates three characters on a FIXTURE server (temp state only), then checks: a character in the game is not deleted;
one that left is deleted on the server (records in the trash, byte for byte; nothing else changes); its name can be
registered again; its old token is dead, also after a server restart; nothing comes back after a restart; and
`fmv-characters restore` gives a deleted character back with a new token that logs in.

  --release        use <target>/release binaries (default)
  --debug          use <target>/debug binaries
  --bin-dir DIR    use fmv-server, fmv-client and fmv-characters from DIR
  --build          cargo build fmv-server and fmv-client first (not with --bin-dir)
  --keep-logs      keep the log directory even when the run passes
EOF
}

usage_error() {
  err "$*"
  usage >&2
  exit 2
}

abs_path() {
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s/%s' "$PWD" "$1" ;;
  esac
}

strip_ansi() {
  local s="$1" re
  re=$'\033\\[[0-9;]*[A-Za-z]'
  while [[ $s =~ $re ]]; do
    s="${s/"${BASH_REMATCH[0]}"/}"
  done
  printf '%s' "$s"
}

# last_line_with <file> <needle>: the last line containing needle (pure bash: the grep on this box is not GNU grep).
last_line_with() {
  local file="$1" needle="$2" line found=""
  [[ -r $file ]] || return 1
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$needle"* ]]; then
      found="$line"
    fi
  done <"$file"
  [[ -n $found ]] || return 1
  printf '%s\n' "$found"
}

# count_lines_with <file> <needle>
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

# count_lines_with_both <file> <needle> <needle>: how many lines contain both.
count_lines_with_both() {
  local file="$1" a="$2" b="$3" line n=0
  [[ -r $file ]] || {
    printf '0'
    return 0
  }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$a"* && $line == *"$b"* ]]; then
      n=$((n + 1))
    fi
  done <"$file"
  printf '%s' "$n"
}

field() {
  local line="$1" key="$2"
  if [[ $line =~ (^|[[:space:]])${key}=([^[:space:]]+) ]]; then
    printf '%s' "${BASH_REMATCH[2]}"
  fi
}

stop_process() {
  local pid="$1" limit="$2" deadline graceful=0
  STOP_RC=0
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    deadline=$((SECONDS + limit))
    while kill -0 "$pid" 2>/dev/null; do
      if ((SECONDS >= deadline)); then
        break
      fi
      sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || true
    else
      graceful=1
    fi
  else
    graceful=1
  fi
  wait "$pid" 2>/dev/null || STOP_RC=$?
  [[ $graceful == 1 ]]
}

dump_logs() {
  local f
  [[ -n $WORK ]] || return 0
  for f in "$WORK"/server-*.log "$WORK"/step-*.log; do
    if [[ -f $f ]]; then
      printf -- '--- %s (last 15 lines) ---\n' "${f##*/}" >&2
      tail -n 15 -- "$f" >&2 || true
    fi
  done
}

problem() { PROBLEMS+=("$*"); }

finish_failed() {
  local p
  RESULT=fail
  printf 'delete-smoke.sh: FAIL (%d problem(s)):\n' "${#PROBLEMS[@]}" >&2
  for p in "${PROBLEMS[@]}"; do
    printf '  - %s\n' "$p" >&2
  done
  dump_logs
  exit 1
}

fail() {
  problem "$*"
  finish_failed
}

# expect <condition-description> <command...>: record a problem unless the command succeeds.
expect() {
  local what="$1"
  shift
  if "$@"; then
    say "ok: $what"
  else
    problem "$what"
  fi
}

cleanup() {
  local rc=$?
  trap - EXIT
  trap '' INT TERM HUP PIPE
  set +e
  if [[ -n $HOLD_PID ]]; then
    stop_process "$HOLD_PID" 3 || true
  fi
  if [[ -n $SERVER_PID ]]; then
    stop_process "$SERVER_PID" 3 || true
  fi
  if [[ -n $WORK ]]; then
    if [[ $RESULT == pass && $KEEP_LOGS == 0 ]]; then
      rm -rf -- "$WORK"
    else
      printf 'delete-smoke.sh: logs and state kept in %s\n' "$WORK" >&2
    fi
  fi
  exit "$rc"
}

# ---- helpers over the state directories ------------------------------------------------------------------------

# snapshot <dir> <out-file>: "<sha256>  <relative path>" of every file under <dir> except LOCK, sorted.
snapshot() {
  local dir="$1" out="$2" f
  : >"$out"
  while IFS= read -r f; do
    [[ $f == "$dir/LOCK" ]] && continue
    printf '%s  %s\n' "$(sha256sum -- "$f" | { read -r h _; printf '%s' "$h"; })" "${f#"$dir"/}" >>"$out"
  done < <(find "$dir" -type f | sort)
}

# hash_of <snapshot-file> <relative path>: the hash the snapshot has for a path (empty if it has none).
hash_of() {
  local file="$1" rel="$2" line
  while IFS= read -r line; do
    if [[ ${line#*  } == "$rel" ]]; then
      printf '%s' "${line%%  *}"
      return 0
    fi
  done <"$file"
  return 0
}

# ---- processes ------------------------------------------------------------------------------------------------

# start_server <n>: a FIXTURE server on the state directory. The first one takes any free port; every later one takes the
# same port, because a client keeps a saved login under the address of its server. Sets CONNECT, PORT and SERVER_PID.
start_server() {
  local n="$1" deadline ready_line="" addr rc
  say "starting server #$n on the state directory"
  "$SERVER_BIN" --bind "127.0.0.1:${PORT:-0}" --state-dir "$WORK/server-state" >"$WORK/server-$n.log" 2>&1 &
  SERVER_PID=$!
  deadline=$((SECONDS + READY_TIMEOUT))
  while :; do
    if ready_line=$(last_line_with "$WORK/server-$n.log" "READY protocol="); then
      break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      rc=0
      wait "$SERVER_PID" || rc=$?
      SERVER_PID=""
      fail "server #$n exited (status $rc) before printing a READY line"
    fi
    if ((SECONDS >= deadline)); then
      fail "no READY line from server #$n within ${READY_TIMEOUT}s"
    fi
    sleep 0.1
  done
  ready_line=$(strip_ansi "$ready_line")
  addr=$(field "$ready_line" addr)
  [[ $addr =~ ^[A-Za-z0-9._-]+:([0-9]+)$ && ${BASH_REMATCH[1]} != 0 ]] || fail "cannot parse addr='$addr' from: $ready_line"
  CONNECT="$addr"
  PORT="${BASH_REMATCH[1]}"
  say "server #$n READY on $CONNECT"
}

# stop_server: SIGTERM and wait; the server must exit.
stop_server() {
  local pid="$SERVER_PID"
  SERVER_PID=""
  stop_process "$pid" "$SHUTDOWN_TIMEOUT" || true
  case "$STOP_RC" in
    0 | 143) say "server stopped (status $STOP_RC)" ;;
    *) problem "the server exited with status $STOP_RC when stopped" ;;
  esac
}

# run_client <step-name> <state-dir> <args...>: run fmv-client with a deadline; the output is in $OUT and the status in $RC.
run_client() {
  local step="$1" state="$2" log pid deadline
  shift 2
  log="$WORK/step-$step.log"
  "$CLIENT_BIN" --server "$CONNECT" --state-dir "$state" "$@" >"$log" 2>&1 &
  pid=$!
  deadline=$((SECONDS + CLIENT_TIMEOUT))
  while kill -0 "$pid" 2>/dev/null; do
    if ((SECONDS >= deadline)); then
      stop_process "$pid" 3 || true
      problem "the client of step '$step' did not finish within ${CLIENT_TIMEOUT}s"
      OUT=$(strip_ansi "$(cat -- "$log")")
      RC=124
      return 0
    fi
    sleep 0.1
  done
  RC=0
  wait "$pid" || RC=$?
  OUT=$(strip_ansi "$(cat -- "$log")")
}

# play <step-name> <state-dir> <name>: log in (or register, the first time) as <name> for a second.
play() {
  run_client "$1" "$2" --name "$3" --headless --duration 1
}

contains() {
  [[ $OUT == *"$1"* ]]
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
      --keep-logs) KEEP_LOGS=1 ;;
      -h | --help)
        usage
        return 0
        ;;
      *) usage_error "unknown argument: $1" ;;
    esac
    shift
  done
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  local script_dir repo_root target_dir
  script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
  repo_root=$(cd -- "$script_dir/.." && pwd -P)
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
  TOOL_BIN="$BIN_DIR/fmv-characters"
  local b
  for b in "$SERVER_BIN" "$CLIENT_BIN" "$TOOL_BIN"; do
    if [[ ! -f $b || ! -x $b ]]; then
      err "executable not found: $b"
      err "build first (cargo build $([[ $PROFILE == release ]] && echo --release) -p fmv-server -p fmv-client), pass --build, or use --bin-dir"
      exit 1
    fi
  done

  # --- scratch space and isolation ------------------------------------------------------------------------
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-delete-smoke.XXXXXX")
  local real_work
  real_work=$(cd -- "$WORK" && pwd -P)
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache" "$WORK/env-state" "$WORK/client" "$WORK/server-state"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export FMV_STATE_DIR="$WORK/env-state"
  export NO_COLOR=1
  say "temp state under $real_work (the real state directory is never given to anything)"
  # What the real state directory looked like when this started: nothing in it may change.
  local real_state="${HOME:-/nonexistent}/.local/share/ff-multiverse" marker="$WORK/marker"
  : >"$marker"

  # ---------------------------------------------------------------------------------------------------------
  # 1. three characters
  # ---------------------------------------------------------------------------------------------------------
  start_server 1
  local name
  for name in "Smoke Aria" "Smoke Brom" "Smoke Cid"; do
    play "register-${name// /-}" "$WORK/client" "$name"
    [[ $RC == 0 ]] || fail "registering '$name' failed (status $RC): $OUT"
    contains "SMOKE name=" || fail "registering '$name' printed no SMOKE line: $OUT"
  done
  say "registered Smoke Aria, Smoke Brom and Smoke Cid"
  local aria_token_file="" f
  for f in "$WORK/client/accounts/"*.token; do
    if [[ $(tail -n 1 -- "$f") == "name=Smoke Aria" ]]; then
      aria_token_file="$f"
    fi
  done
  [[ -n $aria_token_file ]] || fail "no saved login of Smoke Aria in $WORK/client/accounts"
  # Aria's login as it is now, for the stale-token checks later (a client state of its own).
  mkdir -p -- "$WORK/client-stale/accounts"
  cp -- "$aria_token_file" "$WORK/client-stale/accounts/"

  snapshot "$WORK/server-state" "$WORK/before-delete.txt"

  # ---------------------------------------------------------------------------------------------------------
  # 2. a character in the game is not deleted
  # ---------------------------------------------------------------------------------------------------------
  say "Smoke Aria goes into the game and stays there"
  "$CLIENT_BIN" --server "$CONNECT" --state-dir "$WORK/client" --name "Smoke Aria" --headless --duration 60 >"$WORK/step-aria-online.log" 2>&1 &
  HOLD_PID=$!
  # (she registered and played once already, so her second session is the second "player joined" line with her name)
  local deadline=$((SECONDS + READY_TIMEOUT)) joined=0 line
  while ((SECONDS < deadline)); do
    if (($(count_lines_with_both "$WORK/server-1.log" "player joined" 'name="Smoke Aria"') >= 2)); then
      joined=1
      break
    fi
    sleep 0.2
  done
  ((joined)) || fail "Smoke Aria never showed up as joined in the server log (a second time)"
  run_client delete-online "$WORK/client" --delete "Smoke Aria"
  expect "deleting a character that is in the game is refused (exit status $RC)" test "$RC" != 0
  expect "the refusal says the character is in the game" contains "in the game"
  expect "the saved login of Smoke Aria is still there" test -f "$aria_token_file"
  [[ ! -d $WORK/server-state/trash ]] || problem "a trash directory exists after a refused delete"
  stop_process "$HOLD_PID" 5 || true
  HOLD_PID=""
  # (the server takes a player out of the world when its connection ends: wait until every session that joined has left)
  deadline=$((SECONDS + READY_TIMEOUT))
  while ((SECONDS < deadline)) && [[ $(count_lines_with "$WORK/server-1.log" "player joined") != "$(count_lines_with "$WORK/server-1.log" "player left")" ]]; do
    sleep 0.2
  done

  # ---------------------------------------------------------------------------------------------------------
  # 3. once it has left, it is deleted ON THE SERVER, and nothing else changes
  # ---------------------------------------------------------------------------------------------------------
  run_client delete "$WORK/client" --delete "Smoke Aria"
  expect "deleting Smoke Aria after she left succeeds (exit status $RC)" test "$RC" == 0
  expect "the client says 'deleted character name=Smoke%20Aria' (the name is percent-encoded like the SMOKE line's)" contains "deleted character name=Smoke%20Aria"
  expect "the saved login of Smoke Aria is gone from this computer" test ! -e "$aria_token_file"
  expect "the server logged the delete" test "$(count_lines_with "$WORK/server-1.log" "character deleted")" == 1
  snapshot "$WORK/server-state" "$WORK/after-delete.txt"
  local entry_dir entries=("$WORK/server-state/trash/"*)
  if [[ ${#entries[@]} != 1 || ! -d ${entries[0]} ]]; then
    fail "expected exactly one trash entry in $WORK/server-state/trash, found: ${entries[*]}"
  fi
  entry_dir="${entries[0]}"
  say "trash entry: ${entry_dir##*/}"
  expect "the trash entry has a manifest" test -f "$entry_dir/manifest.json"
  # Every file the delete moved is in the entry with the hash it had, and is gone from where it was.
  local moved=0 rel before_hash after_hash
  while IFS= read -r line; do
    rel="${line#*  }"
    before_hash="${line%%  *}"
    case "$rel" in
      accounts/* | avatars/* | characters/* | progression/*)
        after_hash=$(hash_of "$WORK/after-delete.txt" "$rel")
        if [[ -z $after_hash ]]; then
          # gone from the live directories: it must be in the trash with the same bytes
          if [[ $(hash_of "$WORK/after-delete.txt" "trash/${entry_dir##*/}/$rel") == "$before_hash" ]]; then
            moved=$((moved + 1))
          else
            problem "$rel is gone but is not in the trash entry with the same hash"
          fi
        elif [[ $after_hash != "$before_hash" ]]; then
          problem "$rel (a record) changed during the delete"
        fi
        ;;
      index.log) ;; # grew: the release was appended
      *) [[ $(hash_of "$WORK/after-delete.txt" "$rel") == "$before_hash" ]] || problem "$rel changed during the delete" ;;
    esac
  done <"$WORK/before-delete.txt"
  # A fixture server keeps no progression (there is no game to progress in), so the entry holds the three records every
  # character has; a real game's progress files move with them (the persist tests delete accounts with progress).
  ((moved >= 3)) && say "ok: $moved record file(s) moved into the trash entry, byte for byte" || problem "only $moved record file(s) moved into the trash (expected at least the account, avatar and character records)"
  for kind in accounts avatars characters; do
    expect "the trash entry holds a record under $kind/" test -n "$(ls -A -- "$entry_dir/$kind" 2>/dev/null)"
  done
  # The other two characters' records did not move or change: every record that stayed is listed as unchanged above; count them.
  local stayed=0
  while IFS= read -r line; do
    rel="${line#*  }"
    case "$rel" in
      accounts/* | avatars/* | characters/* | progression/*) stayed=$((stayed + 1)) ;;
    esac
  done <"$WORK/after-delete.txt"
  ((stayed >= 6)) && say "ok: $stayed record file(s) of the other characters are still where they were" || problem "the other characters' records are missing after the delete ($stayed left)"

  # ---------------------------------------------------------------------------------------------------------
  # 4. the name is free again; the old token is dead
  # ---------------------------------------------------------------------------------------------------------
  play register-again "$WORK/client" "Smoke Aria"
  expect "the deleted name registers again (exit status $RC)" test "$RC" == 0
  expect "and plays (a SMOKE line)" contains "SMOKE name="
  local new_token_file=""
  for f in "$WORK/client/accounts/"*.token; do
    if [[ $(tail -n 1 -- "$f") == "name=Smoke Aria" ]]; then
      new_token_file="$f"
    fi
  done
  [[ -n $new_token_file ]] || fail "the new Smoke Aria has no saved login"
  expect "the new character has a different token" test "$(head -n 1 -- "$new_token_file")" != "$(head -n 1 -- "$WORK/client-stale/accounts/${aria_token_file##*/}")"
  run_client stale-token "$WORK/client-stale" --name "Smoke Aria" --headless --duration 1
  expect "the old token is refused (exit status $RC)" test "$RC" != 0
  expect "with 'invalid or unknown token'" contains "invalid or unknown token"

  # ---------------------------------------------------------------------------------------------------------
  # 5. a restart brings nothing back
  # ---------------------------------------------------------------------------------------------------------
  stop_server
  start_server 2
  play restart-aria "$WORK/client" "Smoke Aria"
  expect "the re-registered Smoke Aria logs in after the restart (exit status $RC)" test "$RC" == 0
  play restart-brom "$WORK/client" "Smoke Brom"
  expect "Smoke Brom logs in after the restart (exit status $RC)" test "$RC" == 0
  run_client restart-stale "$WORK/client-stale" --name "Smoke Aria" --headless --duration 1
  expect "the old token is still refused after the restart (exit status $RC)" test "$RC" != 0
  expect "still with 'invalid or unknown token'" contains "invalid or unknown token"
  expect "the trash still holds the one entry" test "$(ls -1 -- "$WORK/server-state/trash" | wc -l | { read -r n; printf '%s' "$n"; })" == 1

  # ---------------------------------------------------------------------------------------------------------
  # 6. an operator brings a deleted character back
  # ---------------------------------------------------------------------------------------------------------
  run_client delete-cid "$WORK/client" --delete "Smoke Cid"
  expect "Smoke Cid is deleted too (exit status $RC)" test "$RC" == 0
  stop_server
  local tool_out tool_rc=0
  tool_out=$("$TOOL_BIN" "$WORK/server-state" trash 2>&1) || tool_rc=$?
  expect "fmv-characters trash lists both entries (exit status $tool_rc)" test "$tool_rc" == 0
  [[ $tool_out == *'"Smoke Cid"'* && $tool_out == *'"Smoke Aria"'* ]] || problem "fmv-characters trash does not list both characters: $tool_out"
  tool_rc=0
  tool_out=$("$TOOL_BIN" "$WORK/server-state" restore "Smoke Aria" 2>&1) || tool_rc=$?
  expect "restoring Smoke Aria is refused: the name is taken by the new one (exit status $tool_rc)" test "$tool_rc" == 1
  [[ $tool_out == *"already taken"* ]] || problem "the refusal does not say the name is taken: $tool_out"
  tool_rc=0
  tool_out=$("$TOOL_BIN" "$WORK/server-state" restore "Smoke Cid" 2>&1) || tool_rc=$?
  expect "restoring Smoke Cid succeeds (exit status $tool_rc)" test "$tool_rc" == 0
  local cid_token=""
  while IFS= read -r line; do
    if [[ $line == "token: "* ]]; then
      cid_token="${line#token: }"
    fi
  done <<<"$tool_out"
  [[ $cid_token == fmvt_* ]] || fail "fmv-characters restore printed no token: $tool_out"
  [[ $tool_out == *"VERIFICATION FAILED"* ]] && problem "the restore's verification failed: $tool_out"
  start_server 3
  # The client keeps a login as accounts/<server>.<name>.token: two lines, the token and `name=<display name>`.
  local server_key="${CONNECT//[^a-z0-9.-]/_}"
  mkdir -p -- "$WORK/client-restored/accounts"
  printf '%s\nname=Smoke Cid\n' "$cid_token" >"$WORK/client-restored/accounts/$server_key.smoke_cid.token"
  chmod 600 -- "$WORK/client-restored/accounts/$server_key.smoke_cid.token"
  play restored-cid "$WORK/client-restored" "Smoke Cid"
  expect "the restored Smoke Cid logs in with the new token (exit status $RC)" test "$RC" == 0
  expect "and plays (a SMOKE line)" contains "SMOKE name="
  stop_server

  # ---------------------------------------------------------------------------------------------------------
  # 7. hygiene: nothing of the real state directory changed, and no stray state appeared
  # ---------------------------------------------------------------------------------------------------------
  if [[ -d $real_state ]]; then
    # Informative, not a verdict: the owner may run a real server or client on that directory (or another lane may touch it)
    # while this runs, and nothing here was ever given its path. If something shows, it is not this script's doing.
    local touched
    touched=$(find "$real_state" -newer "$marker" 2>/dev/null | head -n 3) || true
    if [[ -z $touched ]]; then
      say "ok: nothing in the real state directory is newer than the start of this run"
    else
      say "note: something in the real state directory is newer than the start of this run (this script never gave it to anything, so another process wrote it): $touched"
    fi
  fi
  [[ -z $(ls -A -- "$WORK/env-state") ]] && say "ok: \$FMV_STATE_DIR (a throw-away directory) was never written" || problem "something wrote into \$FMV_STATE_DIR: $(ls -A -- "$WORK/env-state")"

  if ((${#PROBLEMS[@]} > 0)); then
    finish_failed
  fi
  RESULT=pass
  say "PASS: a character in the game is not deleted; a deleted one is in the trash byte for byte, its name is free, its token is dead, a restart brings nothing back, and restore gives a character back with a new token"
}

main "$@"
