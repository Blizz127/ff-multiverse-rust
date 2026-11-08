#!/usr/bin/env bash
# ff-multiverse.sh - entry point of the FF Multiverse launcher package (PRIVATE, owner-only).
#
# Usage: ff-multiverse.sh [fmv-client arguments...]
#        ff-multiverse.sh --stop-server
#        ff-multiverse.sh --wrapper-help
#
# This file is shipped at the package root as `ff-multiverse.sh` next to fmv-server,
# fmv-client and fmv-import (scripts/package.sh stages it). A launcher runs it as the game's
# executable. In order, it
#   1. resolves its own directory (the package root, whatever the working directory is);
#   2. picks the DATA directory (below) and keeps its own logs and state in <DATA>/launcher/;
#   3. first run only: when <DATA>/library holds no FF7 set it finds the owner's Steam copy of
#      FF7 and runs `fmv-import import-pc <folder> --language en` into <DATA>. The Steam folder
#      is only ever READ: nothing is written there, nothing is downloaded, and this package
#      contains no game data;
#   4. makes sure THIS script's own local fmv-server is running (127.0.0.1, port 47772, its own
#      state directory under <DATA>/launcher/server, --debug-encounters), and restarts it when
#      the fmv-server binary changed since it was started. Only the process whose pid this
#      script recorded is ever signalled, after checking that the pid still is that server;
#   5. execs `fmv-client --game ff7 --server 127.0.0.1:<port> ...` with the client state in
#      <DATA>/launcher/client. Every argument you give is passed on to fmv-client, so
#      `ff-multiverse.sh --headless --name Test --duration 5` runs a headless bot (a window
#      without --name opens the character menu).
#
# DATA directory, first match wins:
#   $FMV_DATA_DIR                    what a launcher hands over through an environment variable
#   the first argument, when it is an absolute path (what a launcher that passes the data
#                                    directory as an argument sends; it is consumed here,
#                                    fmv-client never sees it)
#   ${XDG_DATA_HOME:-~/.local/share}/ff-multiverse
#
# Layout under DATA:
#   library/                      the imported FF7 library (fmv-import --state-dir DATA)
#   launcher/launcher.log         what this script did and why it failed (one generation kept)
#   launcher/import.log           the output of the importer's last run
#   launcher/server.log           the server's output since its last start (previous run: .1)
#   launcher/server.pid           "<pid> <sha256 of the fmv-server it was started from>"
#   launcher/server/              the server's accounts, avatars and character progress
#   launcher/client/              the client's saved login tokens
#   launcher/start.lock           serialises concurrent starts
#
# Environment, all optional:
#   FMV_DATA_DIR        the data directory (see above)
#   FMV_FF7_DIR         the FF7 folder to import from instead of looking for Steam: the folder
#                       that holds data/ or the Steam Edition's install root
#   FMV_FF7_LANGUAGE    language to import (default: en)
#   FMV_FF7_REIMPORT=1  run the import again even though a library exists
#   FMV_PORT            the local server's port (default: 47772)
#   FMV_READY_TIMEOUT   seconds to wait for the server's READY line (default: 60)
#   FMV_STOP_TIMEOUT    seconds to wait for the server to exit after SIGTERM (default: 30)
#
# Exit status: whatever fmv-client returns once it runs; before that 2 = usage or environment
# error, 3 = no FF7 copy found or its import failed (the client's own "content" code), 1 =
# anything else (the local server did not start, ...). Messages go to stderr AND to
# launcher/launcher.log, because a launcher shows a failed start only as "exited early".
#
# The server is started detached (own session, parent = init, no lock descriptors) and is left
# running when the client closes: a launcher's STOP kills the whole process tree of the process
# it started, and this keeps that from SIGKILLing the server in the middle of a progress save.
# `ff-multiverse.sh --stop-server` ends it with SIGTERM (the server saves, then exits).
#
# Pure bash on purpose (no grep, no jq): the target box may not have them. Needs bash >= 4.4,
# coreutils, flock (optional) and setsid (optional).

set -euo pipefail

DEFAULT_PORT=47772
STEAM_EDITION_DIR="FINAL FANTASY VII Steam Edition"
STEAM_2013_DIR="FINAL FANTASY VII"
LOG_LIMIT_BYTES=262144

HERE=""
DATA=""
LAUNCH_DIR=""
LOG_FILE=""
IMPORT_LOG=""
SERVER_LOG=""
PIDFILE=""
SERVER_STATE=""
CLIENT_STATE=""
PORT=""
READY_TIMEOUT=60
STOP_TIMEOUT=30
READY_LINE=""
REC_PID=""
REC_SHA=""
CMD=()
LIBS=()
CANDIDATES=()
SERVER_ARGS=()

# ---------------------------------------------------------------------------
# Messages. Everything user-facing goes to stderr and to the log file once it exists.
# ---------------------------------------------------------------------------

log_line() {
  if [[ -n $LOG_FILE ]]; then
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG_FILE" 2>/dev/null || true
  fi
}

say() {
  printf 'ff-multiverse: %s\n' "$*" >&2
  log_line "$*"
}

# fatal <exit status> <message line>...: print every line, then stop the script.
fatal() {
  local rc="$1" line
  shift
  for line in "$@"; do
    say "$line"
  done
  if [[ -n $LOG_FILE ]]; then
    printf 'ff-multiverse: details: %s\n' "$LOG_FILE" >&2
  fi
  exit "$rc"
}

have() { command -v "$1" >/dev/null 2>&1; }

usage() {
  cat <<'EOF'
Usage: ff-multiverse.sh [fmv-client arguments...]
       ff-multiverse.sh --stop-server
       ff-multiverse.sh --wrapper-help

Starts FF Multiverse (FF7) for the launcher: on the first run it imports your own Steam copy
of FF7 (read-only), then it makes sure this machine's own local server is running and runs
fmv-client against it. Every argument is passed on to fmv-client, for example
`--headless --name Test --duration 5`; without --name a window opens the character menu.

  --stop-server    stop the local server this script started (SIGTERM), then exit
  --wrapper-help   show this text (fmv-client's own --help is passed through)

Environment (all optional):
  FMV_DATA_DIR        data directory (a launcher sets it; the first argument may carry it too);
                      default ${XDG_DATA_HOME:-~/.local/share}/ff-multiverse
  FMV_FF7_DIR         FF7 folder to import from instead of searching Steam libraries
  FMV_FF7_LANGUAGE    language to import (default en)
  FMV_FF7_REIMPORT=1  import again even though a library exists
  FMV_PORT            local server port (default 47772)
  FMV_READY_TIMEOUT   seconds to wait for the server's READY line (default 60)
  FMV_STOP_TIMEOUT    seconds to wait for the server to exit after SIGTERM (default 30)

Logs: <data dir>/launcher/launcher.log, import.log and server.log.
EOF
}

# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

require_bash() {
  if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
    printf 'ff-multiverse: error: bash >= 4.4 is required (found %s)\n' "$BASH_VERSION" >&2
    exit 2
  fi
}

# resolve_here: HERE = the directory that holds this script, with symlinks resolved.
resolve_here() {
  local src
  src=$(readlink -f -- "${BASH_SOURCE[0]}" 2>/dev/null) || src="${BASH_SOURCE[0]}"
  HERE=$(cd -- "$(dirname -- "$src")" && pwd -P) || fatal 1 "cannot resolve the directory of this script"
}

require_binaries() {
  local b
  for b in fmv-server fmv-client fmv-import; do
    if [[ ! -f $HERE/$b || ! -x $HERE/$b ]]; then
      fatal 1 "the package is incomplete: $HERE/$b is missing or not executable" \
        "reinstall FF Multiverse from the launcher"
    fi
  done
}

# sha256_of <file>: the hex digest only.
sha256_of() {
  local out
  out=$(sha256sum <"$1") || return 1
  printf '%s' "${out%% *}"
}

# ---------------------------------------------------------------------------
# The data directory and the logs
# ---------------------------------------------------------------------------

# pick_data_dir <directory given as the first argument, or empty>
pick_data_dir() {
  local from_arg="$1" chosen="" origin="" base=""
  if [[ -n ${FMV_DATA_DIR:-} ]]; then
    chosen="$FMV_DATA_DIR"
    origin="FMV_DATA_DIR"
  elif [[ -n $from_arg ]]; then
    chosen="$from_arg"
    origin="the first argument"
  else
    if [[ -n ${XDG_DATA_HOME:-} ]]; then
      base="$XDG_DATA_HOME"
    elif [[ -n ${HOME:-} ]]; then
      base="$HOME/.local/share"
    else
      fatal 2 "cannot find a data directory: neither FMV_DATA_DIR, XDG_DATA_HOME nor HOME is set"
    fi
    chosen="$base/ff-multiverse"
    origin="the default"
  fi
  if [[ $chosen != /* ]]; then
    fatal 2 "the data directory must be an absolute path (from $origin): $chosen"
  fi
  while [[ $chosen == */ && $chosen != / ]]; do
    chosen="${chosen%/}"
  done
  if [[ $chosen == / ]]; then
    fatal 2 "refusing to use / as the data directory"
  fi
  DATA="$chosen"
  LAUNCH_DIR="$DATA/launcher"
  IMPORT_LOG="$LAUNCH_DIR/import.log"
  SERVER_LOG="$LAUNCH_DIR/server.log"
  PIDFILE="$LAUNCH_DIR/server.pid"
  SERVER_STATE="$LAUNCH_DIR/server"
  CLIENT_STATE="$LAUNCH_DIR/client"
  if ! mkdir -p -- "$SERVER_STATE" "$CLIENT_STATE" 2>/dev/null; then
    fatal 1 "cannot create the data directory $DATA (from $origin)"
  fi
  LOG_FILE="$LAUNCH_DIR/launcher.log"
  rotate_log
  log_line "---- start: pid=$$ package=${HERE##*/} data=$DATA (from $origin)"
}

# rotate_log: keep one previous generation once the log passes LOG_LIMIT_BYTES.
rotate_log() {
  local size
  if [[ -f $LOG_FILE ]]; then
    size=$(wc -c <"$LOG_FILE" 2>/dev/null) || size=0
    size="${size//[[:space:]]/}"
    if [[ $size =~ ^[0-9]+$ ]] && ((size > LOG_LIMIT_BYTES)); then
      mv -f -- "$LOG_FILE" "$LOG_FILE.1" 2>/dev/null || true
    fi
  fi
}

# read_settings: validate the numeric environment knobs.
read_settings() {
  PORT="${FMV_PORT:-$DEFAULT_PORT}"
  if [[ ! $PORT =~ ^[1-9][0-9]{0,4}$ ]] || ((PORT < 1024 || PORT > 65535)); then
    fatal 2 "FMV_PORT must be a port number from 1024 to 65535, got '$PORT'"
  fi
  READY_TIMEOUT="${FMV_READY_TIMEOUT:-60}"
  STOP_TIMEOUT="${FMV_STOP_TIMEOUT:-30}"
  if [[ ! $READY_TIMEOUT =~ ^[1-9][0-9]*$ || ! $STOP_TIMEOUT =~ ^[1-9][0-9]*$ ]]; then
    fatal 2 "FMV_READY_TIMEOUT and FMV_STOP_TIMEOUT must be positive whole numbers of seconds"
  fi
}

# ---------------------------------------------------------------------------
# Finding and importing the owner's FF7
# ---------------------------------------------------------------------------

# library_has_ff7: true when <DATA>/library holds a set for FF7 (set ids start with "ff7-").
library_has_ff7() {
  local f
  for f in "$DATA"/library/sets/ff7-*/manifest.json; do
    if [[ -f $f ]]; then
      return 0
    fi
  done
  return 1
}

# add_library <path>: remember a Steam library folder once (by real path).
add_library() {
  local p="$1" real known
  if [[ $p != /* || ! -d $p ]]; then
    return 0
  fi
  real=$(readlink -f -- "$p" 2>/dev/null) || real="$p"
  for known in "${LIBS[@]}"; do
    if [[ $known == "$real" ]]; then
      return 0
    fi
  done
  LIBS+=("$real")
}

# vdf_library_paths <libraryfolders.vdf>: print the library paths, one per line. Current Steam
# writes `"path"  "/mnt/games"` inside numbered blocks; older Steam wrote `"1"  "/mnt/games"`.
vdf_library_paths() {
  local line value
  local new_re='^[[:space:]]*"path"[[:space:]]+"(/.*)"[[:space:]]*$'
  local old_re='^[[:space:]]*"[0-9]+"[[:space:]]+"(/.*)"[[:space:]]*$'
  [[ -r $1 ]] || return 0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line =~ $new_re || $line =~ $old_re ]]; then
      value="${BASH_REMATCH[1]}"
      value="${value//\\\\/\\}" # VDF writes a backslash as two
      printf '%s\n' "$value"
    fi
  done <"$1"
}

# collect_steam_libraries: fill LIBS from the Steam roots of a native, Flatpak and Snap Steam
# and the library folders each one lists.
collect_steam_libraries() {
  local home="${HOME:-}" root vdf path
  local -a roots=()
  if [[ -n ${XDG_DATA_HOME:-} ]]; then
    roots+=("$XDG_DATA_HOME/Steam")
  fi
  if [[ -n $home ]]; then
    roots+=(
      "$home/.local/share/Steam"
      "$home/.steam/steam"
      "$home/.steam/root"
      "$home/.var/app/com.valvesoftware.Steam/.local/share/Steam"
      "$home/.var/app/com.valvesoftware.Steam/.steam/steam"
      "$home/snap/steam/common/.local/share/Steam"
    )
  fi
  LIBS=()
  for root in "${roots[@]}"; do
    if [[ ! -d $root ]]; then
      continue
    fi
    add_library "$root"
    for vdf in "$root/steamapps/libraryfolders.vdf" "$root/config/libraryfolders.vdf"; do
      while IFS= read -r path; do
        add_library "$path"
      done < <(vdf_library_paths "$vdf")
    done
  done
}

# find_ff7_candidates: fill CANDIDATES with the install folders to try, the Steam Edition
# (appid 3837340) before the 2013 Steam release (appid 39140). FMV_FF7_DIR replaces the search.
find_ff7_candidates() {
  local lib dir
  CANDIDATES=()
  if [[ -n ${FMV_FF7_DIR:-} ]]; then
    if [[ ! -d $FMV_FF7_DIR ]]; then
      fatal 3 "FMV_FF7_DIR is not a directory: $FMV_FF7_DIR"
    fi
    CANDIDATES+=("$FMV_FF7_DIR")
    return 0
  fi
  collect_steam_libraries
  for dir in "$STEAM_EDITION_DIR" "$STEAM_2013_DIR"; do
    for lib in "${LIBS[@]}"; do
      if [[ -d $lib/steamapps/common/$dir ]]; then
        CANDIDATES+=("$lib/steamapps/common/$dir")
      fi
    done
  done
}

# ensure_library: import the owner's FF7 unless the library already holds it.
ensure_library() {
  local lang="${FMV_FF7_LANGUAGE:-en}" cand tried=0 tail_text="" attempt="$LAUNCH_DIR/import.attempt"
  local -a failure=()
  if library_has_ff7 && [[ ${FMV_FF7_REIMPORT:-0} != 1 ]]; then
    log_line "library: an FF7 set is already imported"
    return 0
  fi
  if [[ ! $lang =~ ^[A-Za-z][A-Za-z0-9_-]{0,15}$ ]]; then
    fatal 2 "FMV_FF7_LANGUAGE is not a language code: '$lang'"
  fi
  say "first run: looking for your Final Fantasy VII"
  find_ff7_candidates
  if ((${#CANDIDATES[@]} == 0)); then
    local -a msg=("Final Fantasy VII was not found on this computer.")
    if ((${#LIBS[@]})); then
      msg+=("Steam libraries searched: ${LIBS[*]}")
    else
      msg+=("No Steam installation was found (native, Flatpak or Snap).")
    fi
    msg+=(
      "Install FINAL FANTASY VII from Steam, or set FMV_FF7_DIR to the folder that holds its data/"
      "(for the Steam Edition: the install folder \"$STEAM_EDITION_DIR\")."
      "FF Multiverse only reads your own copy, read-only, and ships no game files."
    )
    fatal 3 "${msg[@]}"
  fi
  : >"$IMPORT_LOG"
  for cand in "${CANDIDATES[@]}"; do
    tried=$((tried + 1))
    say "importing FF7 from $cand into $DATA/library (the original is only read)"
    if "$HERE/fmv-import" --state-dir "$DATA" import-pc "$cand" --language "$lang" >"$attempt" 2>&1 &&
      library_has_ff7; then
      printf '==== import-pc %s (language %s): ok\n' "$cand" "$lang" >>"$IMPORT_LOG"
      cat -- "$attempt" >>"$IMPORT_LOG" 2>/dev/null || true
      rm -f -- "$attempt"
      say "FF7 library imported from $cand"
      return 0
    fi
    printf '==== import-pc %s (language %s): FAILED\n' "$cand" "$lang" >>"$IMPORT_LOG"
    cat -- "$attempt" >>"$IMPORT_LOG" 2>/dev/null || true
    tail_text=$(tail -n 3 -- "$attempt" 2>/dev/null | tr '\n' ' ') || tail_text=""
    rm -f -- "$attempt"
    say "import from $cand failed: ${tail_text:-no output}"
  done
  failure=("FF7 was found ($tried folder(s) tried) but none could be imported."
    "The importer's output is in $IMPORT_LOG.")
  if [[ $tail_text == *language* ]]; then
    failure+=("If your copy is in another language, set FMV_FF7_LANGUAGE (for example fr).")
  fi
  fatal 3 "${failure[@]}"
}

# ---------------------------------------------------------------------------
# The local server
# ---------------------------------------------------------------------------

# is_our_server <pid>: the pid is an fmv-server that runs on THIS data directory's server
# state. The executable path is deliberately not compared: after an update the old server
# runs from the previous version's directory, which may be gone.
is_our_server() {
  local pid="$1" i
  CMD=()
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  [[ -r /proc/$pid/cmdline ]] || return 1
  mapfile -d '' -t CMD <"/proc/$pid/cmdline" 2>/dev/null || return 1
  ((${#CMD[@]} > 2)) || return 1
  [[ ${CMD[0]##*/} == fmv-server ]] || return 1
  for ((i = 1; i < ${#CMD[@]}; i++)); do
    if [[ ${CMD[i]} == "--state-dir=$SERVER_STATE" ]]; then
      return 0
    fi
    if [[ ${CMD[i]} == --state-dir && ${CMD[i + 1]:-} == "$SERVER_STATE" ]]; then
      return 0
    fi
  done
  return 1
}

process_alive() { kill -0 "$1" 2>/dev/null; }

# parent_of <pid>: the parent pid, from /proc.
parent_of() {
  local line value
  [[ -r /proc/$1/status ]] || return 1
  while IFS= read -r line; do
    if [[ $line == PPid:* ]]; then
      value="${line#PPid:}"
      printf '%s' "${value//[[:space:]]/}"
      return 0
    fi
  done <"/proc/$1/status"
  return 1
}

# read_pidfile: REC_PID and REC_SHA from server.pid; returns 1 when it is absent or garbled.
read_pidfile() {
  local first="" second="" rest=""
  REC_PID=""
  REC_SHA=""
  [[ -f $PIDFILE ]] || return 1
  read -r first second rest <"$PIDFILE" || true
  if [[ $first =~ ^[0-9]+$ && $second =~ ^[0-9a-f]{64}$ ]]; then
    REC_PID="$first"
    REC_SHA="$second"
    return 0
  fi
  return 1
}

# port_open: something accepts connections on 127.0.0.1:$PORT.
port_open() {
  (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null
}

# stop_server_pid <pid>: SIGTERM, wait for the server to leave, SIGKILL only as a last resort.
# The caller has already shown that the pid is our server. Returns 0 once it is gone.
stop_server_pid() {
  local pid="$1" deadline
  kill -TERM "$pid" 2>/dev/null || true
  deadline=$((SECONDS + STOP_TIMEOUT))
  while is_our_server "$pid"; do
    if ((SECONDS >= deadline)); then
      break
    fi
    sleep 0.2
  done
  if is_our_server "$pid"; then
    say "the server (pid $pid) did not exit within ${STOP_TIMEOUT}s of SIGTERM; killing it"
    kill -KILL "$pid" 2>/dev/null || true
    sleep 1
  fi
  if is_our_server "$pid"; then
    return 1
  fi
  return 0
}

# ready_line_seen: READY_LINE = the server's READY line, once it is in the log.
ready_line_seen() {
  local line
  [[ -r $SERVER_LOG ]] || return 1
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"READY protocol="* ]]; then
      READY_LINE="$line"
      return 0
    fi
  done <"$SERVER_LOG"
  return 1
}

# server_args: SERVER_ARGS = the exact arguments this script starts fmv-server with.
server_args() {
  SERVER_ARGS=(--bind "127.0.0.1:$PORT" --game ff7 --ff7-library "$DATA"
    --state-dir "$SERVER_STATE" --debug-encounters)
}

# runs_current_args: the server whose argv is in CMD (set by is_our_server) was started with
# SERVER_ARGS, so a changed port or option is noticed even when the binary is the same.
runs_current_args() {
  local i
  ((${#CMD[@]} == ${#SERVER_ARGS[@]} + 1)) || return 1
  for ((i = 0; i < ${#SERVER_ARGS[@]}; i++)); do
    if [[ ${CMD[i + 1]} != "${SERVER_ARGS[i]}" ]]; then
      return 1
    fi
  done
  return 0
}

# start_server <sha256 of fmv-server>: start it detached, wait for READY, record "<pid> <sha>".
start_server() {
  local sha="$1" pid="" tmp="$LAUNCH_DIR/server.pid.new" deadline parent
  if [[ -f $SERVER_LOG ]]; then
    mv -f -- "$SERVER_LOG" "$SERVER_LOG.1" 2>/dev/null || true
  fi
  rm -f -- "$tmp"
  say "starting the local server on 127.0.0.1:$PORT"
  # Detach completely: a subshell starts it in the background and exits at once, so the server
  # is parented by init, not by this script (and so not by the client this script becomes).
  # fd 9 (the start lock) is closed for it, or it would keep the lock for its whole life.
  (
    if have setsid; then
      setsid nohup "$HERE/fmv-server" "${SERVER_ARGS[@]}" >"$SERVER_LOG" 2>&1 </dev/null 9>&- &
    else
      nohup "$HERE/fmv-server" "${SERVER_ARGS[@]}" >"$SERVER_LOG" 2>&1 </dev/null 9>&- &
    fi
    printf '%s\n' "$!" >"$tmp"
  )
  read -r pid <"$tmp" || true
  rm -f -- "$tmp"
  if [[ ! $pid =~ ^[0-9]+$ ]]; then
    fatal 1 "could not start the local server (no pid)"
  fi
  # Record it at once: if this script is stopped while it waits, the next start still knows
  # the server is its own (a server nobody recorded would block the port for good).
  printf '%s %s\n' "$pid" "$sha" >"$PIDFILE.new"
  mv -f -- "$PIDFILE.new" "$PIDFILE"
  READY_LINE=""
  deadline=$((SECONDS + READY_TIMEOUT))
  while ! ready_line_seen; do
    if ! process_alive "$pid"; then
      rm -f -- "$PIDFILE"
      fatal 1 "the local server exited before it was ready; its output ($SERVER_LOG):" \
        "$(tail -n 8 -- "$SERVER_LOG" 2>/dev/null | tr '\n' '|')" \
        "If it mentions battle data, import again with FMV_FF7_REIMPORT=1."
    fi
    if ((SECONDS >= deadline)); then
      if is_our_server "$pid"; then
        kill -TERM "$pid" 2>/dev/null || true
      fi
      rm -f -- "$PIDFILE"
      fatal 1 "the local server was not ready within ${READY_TIMEOUT}s (see $SERVER_LOG)"
    fi
    sleep 0.1
  done
  if ! is_our_server "$pid"; then
    rm -f -- "$PIDFILE"
    fatal 1 "pid $pid is not the server that was just started; not using it"
  fi
  parent=$(parent_of "$pid") || parent=""
  if [[ $parent == "$$" ]]; then
    say "warning: the server is still a child of this script, so a launcher's STOP may kill it with SIGKILL"
  fi
  say "local server ready (pid $pid): ${READY_LINE}"
}

# ensure_server: reuse this script's server when it is the current build, replace it when the
# fmv-server binary changed, refuse to share the port with a foreign process.
ensure_server() {
  local want_sha
  want_sha=$(sha256_of "$HERE/fmv-server") || fatal 1 "cannot hash $HERE/fmv-server"
  server_args
  if read_pidfile && is_our_server "$REC_PID"; then
    if [[ $REC_SHA == "$want_sha" ]] && runs_current_args && port_open; then
      say "using the local server that is already running (pid $REC_PID)"
      return 0
    fi
    if [[ $REC_SHA != "$want_sha" ]]; then
      say "the packaged fmv-server changed since pid $REC_PID was started; restarting it"
    elif ! runs_current_args; then
      say "pid $REC_PID was started with other settings (port or options); restarting it"
    else
      say "pid $REC_PID is this script's server but 127.0.0.1:$PORT does not answer; restarting it"
    fi
    if ! stop_server_pid "$REC_PID"; then
      fatal 1 "could not stop the old local server (pid $REC_PID); stop it by hand and start again"
    fi
  fi
  rm -f -- "$PIDFILE"
  if port_open; then
    fatal 1 "127.0.0.1:$PORT is already in use by a program this launcher did not start." \
      "Joining it could register your character in somebody else's server." \
      "Stop that program, or choose another port with FMV_PORT (for example FMV_PORT=47773)."
  fi
  start_server "$want_sha"
}

# stop_server_command: --stop-server.
stop_server_command() {
  if ! read_pidfile; then
    rm -f -- "$PIDFILE"
    say "no local server of this launcher is recorded; nothing to stop"
    return 0
  fi
  if ! is_our_server "$REC_PID"; then
    rm -f -- "$PIDFILE"
    say "the recorded server (pid $REC_PID) is not running; cleared the record"
    return 0
  fi
  say "stopping the local server (pid $REC_PID)"
  if stop_server_pid "$REC_PID"; then
    rm -f -- "$PIDFILE"
    say "the local server stopped"
    return 0
  fi
  fatal 1 "the local server (pid $REC_PID) is still running"
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

main() {
  local arg_data="" arg lock_fd_open=0
  require_bash
  resolve_here
  umask 077

  # A launcher that passes the data directory as the first argument (an absolute path) is
  # understood; fmv-client has no positional argument, so nothing of the real arguments is lost.
  if [[ ${1:-} == /* ]]; then
    arg_data="$1"
    shift
  fi
  if [[ ${1:-} == --wrapper-help ]]; then
    usage
    return 0
  fi
  # Help and version are fmv-client's own: answer them without importing or starting anything.
  for arg in "$@"; do
    case "$arg" in
      -h | --help | -V | --version)
        require_binaries
        exec "$HERE/fmv-client" "$@"
        ;;
    esac
  done

  require_binaries
  pick_data_dir "$arg_data"
  read_settings
  # Nothing this script starts may fall back to a state directory that is not this data directory.
  export FMV_STATE_DIR="$DATA"

  if [[ ${1:-} == --stop-server ]]; then
    stop_server_command
    return 0
  fi
  log_line "client arguments: $*"

  # One start at a time: two launches in the same second must not both import or both start a server.
  if have flock; then
    exec 9>"$LAUNCH_DIR/start.lock"
    lock_fd_open=1
    if ! flock -w 180 9; then
      fatal 1 "another start of FF Multiverse is still running (lock $LAUNCH_DIR/start.lock)"
    fi
  fi
  ensure_library
  ensure_server
  if ((lock_fd_open)); then
    exec 9>&-
  fi

  log_line "starting fmv-client against 127.0.0.1:$PORT"
  exec "$HERE/fmv-client" --game ff7 --server "127.0.0.1:$PORT" \
    --ff7-library "$DATA" --state-dir "$CLIENT_STATE" "$@"
}

# Keep this as the last statement: bash parses every function above before running main, so
# --wrapper-help exercises a whole-file parse. Sourcing the file (for tests) runs nothing.
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
