#!/usr/bin/env bash
# scripts/smoke.sh - local two-bot smoke test for fmv-server + fmv-client.
#
# Usage: scripts/smoke.sh [--release|--debug] [--bin-dir DIR] [--build]
#                         [--bind ADDR] [--duration SECS] [--keep-logs] [-h|--help]
#
# What it does:
#   1. Starts fmv-server with `--bind ADDR` (default 127.0.0.1:0, i.e. an OS-chosen port).
#   2. Waits up to 15 s for the server's READY line and parses the bound address from it:
#        READY protocol=<v> build=<ver> addr=<addr> area=<id> fixture=<bool>
#   3. Starts two headless clients (names A and B) concurrently, each running for
#      --duration seconds (default 5):
#        fmv-client --server <host:port> --name <n> --headless --duration <secs>
#      and expects each to print
#        SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n>
#             party=<n> battles=<n> victories=<n> area_changes=<n> level=<n> exp=<n>
#      with peers_seen >= 1 and to exit 0. (Only the key=value tokens it needs are read, by key: new
#      fields are appended at the end of the line and never move the ones before them. level and exp
#      are the character progress the server reported, 0 and 0 from the FIXTURE server, which has none.)
#   4. Sends the server SIGTERM and expects it to exit (status 0, or 143 when it has no
#      SIGTERM handler and is simply terminated by the signal; the latter is reported).
#
# Exit status: 0 = pass, 1 = failure (the reasons are printed, log tails are shown and
# the log directory is kept), 2 = usage error.
#
# Binaries are taken from --bin-dir DIR (for example an unpacked release tarball, which
# smoke-tests the artifact that would ship), else from
# ${CARGO_TARGET_DIR:-<repo>/target}/{release,debug}. --build runs cargo build for
# fmv-server and fmv-client first (not combinable with --bin-dir).
#
# Isolation: the server and clients run with XDG_DATA_HOME/XDG_STATE_HOME/
# XDG_CONFIG_HOME/XDG_CACHE_HOME pointing into a throw-away directory, so a smoke run can
# never touch your real library or state. NO_COLOR=1 keeps their output free of escapes.
# Only the PIDs this script started are ever signalled.
#
# Timeouts can be overridden for testing: FMV_SMOKE_READY_TIMEOUT (15),
# FMV_SMOKE_SHUTDOWN_TIMEOUT (10), FMV_SMOKE_CLIENT_GRACE (15, extra seconds a client
# may run past --duration before it is killed), all in seconds.

set -euo pipefail

PROFILE=release
BIN_DIR=""
DO_BUILD=0
BIND="127.0.0.1:0"
DURATION=5
KEEP_LOGS=0
READY_TIMEOUT="${FMV_SMOKE_READY_TIMEOUT:-15}"
SHUTDOWN_TIMEOUT="${FMV_SMOKE_SHUTDOWN_TIMEOUT:-10}"
CLIENT_GRACE="${FMV_SMOKE_CLIENT_GRACE:-15}"
CLIENT_NAMES=(A B)

WORK=""
SERVER_PID=""
declare -A CPID=() # client name -> pid (emptied once reaped)
declare -A CRC=()  # client name -> exit status, or "timeout"
RESULT=""
PROBLEMS=()
STOP_RC=0

say() { printf 'smoke.sh: %s\n' "$*"; }
err() { printf 'smoke.sh: error: %s\n' "$*" >&2; }

usage() {
  cat <<'EOF'
Usage: scripts/smoke.sh [--release|--debug] [--bin-dir DIR] [--build]
                        [--bind ADDR] [--duration SECS] [--keep-logs] [-h|--help]

Starts fmv-server, waits for its READY line, runs two headless fmv-client bots (A and B)
concurrently, checks that each prints a SMOKE line with peers_seen >= 1, then stops the
server with SIGTERM. Exits non-zero with a clear message on any failure.

  --release        use <target>/release binaries (default)
  --debug          use <target>/debug binaries
  --bin-dir DIR    use fmv-server and fmv-client from DIR (e.g. an unpacked release)
  --build          cargo build fmv-server and fmv-client first (not with --bin-dir)
  --bind ADDR      server bind address (default 127.0.0.1:0 = OS-chosen port)
  --duration SECS  how long each client runs (default 5)
  --keep-logs      keep the log directory even when the run passes

See the header of this script for the exact line formats and environment knobs.
EOF
}

usage_error() {
  err "$*"
  usage >&2
  exit 2
}

# abs_path <path>: make a path absolute without requiring that it exists.
abs_path() {
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s/%s' "$PWD" "$1" ;;
  esac
}

# strip_ansi <string>: drop CSI escape sequences.
strip_ansi() {
  local s="$1" re
  re=$'\033\\[[0-9;]*[A-Za-z]' # ESC, '[', parameter bytes, final letter
  while [[ $s =~ $re ]]; do
    s="${s/"${BASH_REMATCH[0]}"/}"
  done
  printf '%s' "$s"
}

# last_line_with <file> <needle>: print the last line containing needle; fail if none.
# Pure bash on purpose: the grep on this box is not GNU grep.
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

# field <line> <key>: print the value of the whitespace-delimited key=value token.
field() {
  local line="$1" key="$2"
  if [[ $line =~ (^|[[:space:]])${key}=([^[:space:]]+) ]]; then
    printf '%s' "${BASH_REMATCH[2]}"
  fi
}

# stop_process <pid> <timeout-seconds>: SIGTERM, wait, SIGKILL if it is still alive.
# Sets STOP_RC to the exit status reported by wait. Returns 0 if SIGTERM was enough.
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
  if [[ -z $WORK ]]; then
    return 0
  fi
  for f in "$WORK/server.log" "$WORK"/client-*.log; do
    if [[ -f $f ]]; then
      printf -- '--- %s (last 20 lines) ---\n' "${f##*/}" >&2
      tail -n 20 -- "$f" >&2 || true
    fi
  done
}

# problem <message>: record a failure but keep going (so one run reports everything).
problem() { PROBLEMS+=("$*"); }

# finish_failed: print every recorded problem and exit 1.
finish_failed() {
  local p
  RESULT=fail
  printf 'smoke.sh: FAIL (%d problem(s)):\n' "${#PROBLEMS[@]}" >&2
  for p in "${PROBLEMS[@]}"; do
    printf '  - %s\n' "$p" >&2
  done
  dump_logs
  exit 1
}

# fail <message>: record one problem and stop now.
fail() {
  problem "$*"
  finish_failed
}

cleanup() {
  local rc=$? n
  # Cleanup must run to completion: no recursion, and ignore further signals (a second
  # Ctrl-C, or a write to a pipe that has been closed) while it works.
  trap - EXIT
  trap '' INT TERM HUP PIPE
  set +e # best effort from here on, even if stderr is a closed pipe
  # Only PIDs recorded by this script are signalled; never a process group.
  if [[ -n $SERVER_PID ]]; then
    stop_process "$SERVER_PID" 3 || true
  fi
  for n in "${CLIENT_NAMES[@]}"; do
    if [[ -n ${CPID[$n]:-} ]]; then
      stop_process "${CPID[$n]}" 3 || true
    fi
  done
  if [[ -n $WORK ]]; then
    if [[ $RESULT == pass && $KEEP_LOGS == 0 ]]; then
      rm -rf -- "$WORK"
    else
      printf 'smoke.sh: logs kept in %s\n' "$WORK" >&2
    fi
  fi
  exit "$rc"
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
      --bind)
        (($# >= 2)) || usage_error "--bind needs an address"
        BIND="$2"
        shift
        ;;
      --duration)
        (($# >= 2)) || usage_error "--duration needs a number of seconds"
        DURATION="$2"
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
  [[ $DURATION =~ ^[1-9][0-9]*$ ]] || usage_error "--duration must be a positive whole number of seconds, got '$DURATION'"
  [[ -n $BIND ]] || usage_error "--bind must not be empty"
  [[ $READY_TIMEOUT =~ ^[1-9][0-9]*$ && $SHUTDOWN_TIMEOUT =~ ^[1-9][0-9]*$ && $CLIENT_GRACE =~ ^[1-9][0-9]*$ ]] ||
    usage_error "FMV_SMOKE_* timeouts must be positive whole numbers of seconds"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi

  local script_dir repo_root target_dir
  script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
  repo_root=$(cd -- "$script_dir/.." && pwd -P)

  # --- binaries ----------------------------------------------------------------
  if [[ -n $BIN_DIR ]]; then
    BIN_DIR=$(abs_path "$BIN_DIR")
  else
    if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
      target_dir=$(abs_path "$CARGO_TARGET_DIR")
      export CARGO_TARGET_DIR="$target_dir" # cargo and this script must agree on it
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
  local server_bin="$BIN_DIR/fmv-server" client_bin="$BIN_DIR/fmv-client" b
  for b in "$server_bin" "$client_bin"; do
    if [[ ! -f $b || ! -x $b ]]; then
      err "executable not found: $b"
      err "build first (cargo build $([[ $PROFILE == release ]] && echo --release) -p fmv-server -p fmv-client), pass --build, or point --bin-dir at a directory that has both binaries"
      exit 1
    fi
  done

  # --- scratch space and isolation ------------------------------------------------
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/fmv-smoke.XXXXXX")
  # Turn the signals that can arrive (Ctrl-C, kill, closed terminal, `smoke.sh | head`) into
  # ordinary exits: cleanup always runs and the status is the conventional 128+signal.
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE
  mkdir -p -- "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/config" "$WORK/xdg/cache"
  export XDG_DATA_HOME="$WORK/xdg/data" XDG_STATE_HOME="$WORK/xdg/state"
  export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_CACHE_HOME="$WORK/xdg/cache"
  export NO_COLOR=1

  # --- server --------------------------------------------------------------------
  say "starting server: $server_bin --bind $BIND"
  "$server_bin" --bind "$BIND" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!

  local deadline ready_line="" rc
  deadline=$((SECONDS + READY_TIMEOUT))
  while :; do
    if ready_line=$(last_line_with "$WORK/server.log" "READY protocol="); then
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
  ready_line=$(strip_ansi "$ready_line")

  local addr area fixture protocol build port connect
  addr=$(field "$ready_line" addr)
  area=$(field "$ready_line" area)
  fixture=$(field "$ready_line" fixture)
  protocol=$(field "$ready_line" protocol)
  build=$(field "$ready_line" build)
  [[ -n $addr ]] || fail "the READY line has no addr= field: $ready_line"
  if [[ ! $addr =~ ^(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9._-]+):([0-9]+)$ ]]; then
    fail "cannot parse addr='$addr' from the READY line: $ready_line"
  fi
  port="${BASH_REMATCH[2]}"
  if [[ $port == 0 ]]; then
    fail "the READY line reports port 0 (it must report the actually bound port): $ready_line"
  fi
  # A wildcard bind address is not connectable; use loopback for the clients.
  case "$addr" in
    0.0.0.0:*) connect="127.0.0.1:${addr#0.0.0.0:}" ;;
    "[::]:"*) connect="[::1]:${addr#\[::\]:}" ;;
    *) connect="$addr" ;;
  esac
  say "server READY: protocol=${protocol:-?} build=${build:-?} addr=$addr area=${area:-?} fixture=${fixture:-?}"
  if [[ $fixture != true ]]; then
    say "note: fixture=${fixture:-?}; the server is not hosting the synthetic FIXTURE area"
  fi

  # --- clients -------------------------------------------------------------------
  local n
  say "starting clients ${CLIENT_NAMES[*]} concurrently for ${DURATION}s against $connect"
  for n in "${CLIENT_NAMES[@]}"; do
    "$client_bin" --server "$connect" --name "$n" --headless --duration "$DURATION" \
      >"$WORK/client-$n.log" 2>&1 &
    CPID[$n]=$!
  done

  local pending=${#CLIENT_NAMES[@]} client_deadline
  client_deadline=$((SECONDS + DURATION + CLIENT_GRACE))
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
    if ((pending == 0)); then
      break
    fi
    if ((SECONDS >= client_deadline)); then
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

  # --- judge the clients ------------------------------------------------------------
  local smoke_line peers updates rejected
  SMOKE_LINES=()
  for n in "${CLIENT_NAMES[@]}"; do
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "client $n was still running ${CLIENT_GRACE}s after its ${DURATION}s duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if smoke_line=$(last_line_with "$WORK/client-$n.log" "SMOKE name="); then
      smoke_line=$(strip_ansi "$smoke_line")
      SMOKE_LINES+=("$smoke_line")
      peers=$(field "$smoke_line" peers_seen)
      updates=$(field "$smoke_line" updates)
      rejected=$(field "$smoke_line" rejected)
      if [[ ! $peers =~ ^[0-9]+$ ]]; then
        problem "client $n: SMOKE line has no numeric peers_seen: $smoke_line"
      elif ((peers < 1)); then
        problem "client $n saw no other player (peers_seen=$peers, updates=${updates:-?}, rejected=${rejected:-?}); expected peers_seen >= 1"
      fi
    else
      problem "client $n printed no SMOKE line"
    fi
  done

  # --- stop the server ----------------------------------------------------------------
  local spid="$SERVER_PID" server_rc=0 graceful=0
  if kill -0 "$spid" 2>/dev/null; then
    say "stopping the server (SIGTERM)"
    if stop_process "$spid" "$SHUTDOWN_TIMEOUT"; then
      graceful=1
    fi
    server_rc=$STOP_RC
    SERVER_PID=""
    if [[ $graceful == 0 ]]; then
      problem "the server did not exit within ${SHUTDOWN_TIMEOUT}s of SIGTERM and had to be killed"
    elif [[ $server_rc == 143 ]]; then
      say "note: the server was terminated by SIGTERM (status 143); it has no graceful SIGTERM handler yet"
    elif [[ $server_rc != 0 ]]; then
      problem "the server exited with status $server_rc after SIGTERM (expected 0)"
    fi
  else
    wait "$spid" || server_rc=$?
    SERVER_PID=""
    problem "the server had already exited (status $server_rc) before the clients finished"
  fi

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  for smoke_line in "${SMOKE_LINES[@]}"; do
    say "client: $smoke_line"
  done
  RESULT=pass
  say "PASS: server READY at $addr; every client saw at least one other player"
}

# Keep this as the last statement: bash parses every function above before running
# main, so `--help` exercises a whole-file parse. Sourcing the file runs nothing.
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
