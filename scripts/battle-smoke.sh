#!/usr/bin/env bash
# scripts/battle-smoke.sh - shared-battle smoke test for fmv-server + fmv-client (protocol v7).
#
# Usage: scripts/battle-smoke.sh [--fixture|--random|--progress|--commands] [--release|--debug]
#                                [--bin-dir DIR] [--build] [--library STATE_DIR] [--duration SECS]
#                                [--formation ID] [--tick-rate TPS] [--battle-speed N] [--seed N]
#                                [--bot idle|circle] [--keep-logs] [-h|--help]
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
#   * both members acted: the server's "battle ended" line carries commands=[a, b] (accepted
#     commands per actor, of any kind: Attack, Magic, Item, Defend, Limit or Escape; both at least 1)
#     and each bot counts a command of its own applied;
#   * each bot was told its inventory (protocol v6): its SMOKE line ends with items=N, the total
#     of what it carries (a new account starts with the game's starting stock, so N is at least 1).
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
# cannot run battles"), the bot exits 0 and prints battles=0 victories=0 area_changes=0 level=0
# exp=0 items=0 (the FIXTURE server has no characters and no items, so it reports no progress and
# no inventory: 0 is "unknown").
#
# --progress (real data): character progress, inventory and limit gauge, protocol v7. ONE server stays up through
# three runs of the same two bots, which keep their client state directories and so log in again with
# the tokens the first run saved. Run 1: the party fights one shared battle; each bot's progress notes
# ("progress: ... level=L exp=E exp_to_next=N ...", first on joining, then after the win) must show
# experience gained and, if E reached the threshold (E0 + N0), a level gained and leveled_up=true; the
# SMOKE line ends with that level and exp (and the items it carries). Run 2 (a reconnect): each bot is
# told on joining exactly what run 1 ended with (the progress was SAVED and read back), the server's
# second "battle started" line carries those levels (levels=[..]: the second battle's EncounterStart
# carries the level gained), and the second win adds on. Run 3 (a reconnect, no fighting): the SMOKE line
# shows what run 2 ended with. The server must also have logged one "progress saved" line per member
# per battle. Both bots are Cloud (a headless bot registers without a look); on the Steam data he
# starts at level 6 with 610 experience, six short of level 7, so the first win levels him up. The
# server is not restarted here (the client's token files are keyed by server address, and the server
# binds a free port): that the saved progress survives a restart is tested by
# crates/fmv-server/tests/progress.rs.
#
# The same chain for ITEMS: each bot's "inventory: stacks=S total=T items=item:count,.." notes (one per
# Inventory message of the server, the whole inventory) must show, on joining in run 1, the game's
# starting stock (a new account: at least one item), on joining in runs 2 and 3 exactly the items the
# run before ended with (the inventory was SAVED, not given again, and read back), and, after a won
# battle, the stock plus the drops: the two bots' inventories together grew by exactly the items the
# server logged as dealt out to its two members ("battle reward ... dropped=[(item, count), ..]"). The
# bots use no item (they attack, and cast a healing spell when hurt), so an item that went missing is a
# failure; an "item used" line in the server log turns that exact comparison off and says so. A run that
# does not fight leaves the inventory as it was, the SMOKE line's items=N is the last total the server
# reported, and every "progress saved" line carries the inventory's total (items=N). These weak enemies
# often drop nothing under a random seed (a note says when neither battle did); --seed 5 pins one under
# which formation 301 drops a Potion in every battle, so the whole drop chain runs.
#
# The same chain for the LIMIT GAUGE (protocol v7): a new account starts its first battle with an empty
# gauge (the server's "battle started ... limits=[0, 0]"), the damage the party takes fills it, and the
# gauges the first battle ended with (the server's "battle ended ... limits=[a, b]") are exactly what the
# second battle starts with (the server keeps the gauge in its mirror of the character and queues it for
# the saver: "limit kept ... saving=queued"). Stopping the server must have written every queued gauge:
# one "limit saved account=" line per "saving=queued" (those are not "progress saved" lines, which stay one
# per member per won battle). Every "battle started" line also says how hard the fight is to run from
# (run_difficulty=N, or none), read from scene.bin's own flags and counters. The bots give their limit break
# when the gauge is full and never escape, but these weak enemies rarely fill a gauge in one battle, so this
# mode does not fire a limit or an escape: --commands (below) does, over real processes. The gauge a battle
# ends with surviving a restart is tested by crates/fmv-server/tests/progress.rs; Braver and running with the
# owner's own kernel and scenes by crates/fmv-ff7-battle/tests/real_data.rs.
#
# --commands (real data): EVERY battle command over real processes. The headless bots only attack (and
# heal below 30% HP, which the first enemies never bring about), so the server runs with --debug-limit-full
# (every battle member starts with a FULL limit gauge; it needs --debug-encounters, and what it leaves
# alone is in `fmv-server --help`) and the bots follow a --battle-script. ONE server stays up through two
# runs of the same two bots (they log in again with the tokens the first run saved; each run is one
# battle of formation 301, whose run difficulty is 1, so it can be escaped):
#   run 1 "cmds":   A: defend,item:0,magic:0,magic:0:all,limit   B: magic:0,magic:9999,item:0,defend,limit
#                   (Potion is item 0, Cure is spell 0; `magic:0:all` is Cure on the WHOLE PARTY, which takes the
#                   once-per-battle use of the All materia (protocol v8: Cloud's grimoire has one, read from the
#                   owner's kernel) and is checked below as the server's `target=all-allies`; spell 9999 does not
#                   exist: the entry must be skipped, counted and not retried, and the next one tried in the same
#                   turn; the limit goes last, as the enemies of this formation die in one blow, so nothing ends
#                   the battle before the other entries are applied). Expected: both bots win it.
#   run 2 "escape": A and B: escape x6 (a run check succeeds 68% of the time per attempt, so several attempts
#                   are listed; the first success ends the battle). Expected: the battle ends in Escaped.
# The scripts can be overridden to try the mode's failure paths: FMV_SMOKE_SCRIPT_A, FMV_SMOKE_SCRIPT_B
# (run 1) and FMV_SMOKE_ESCAPE_SCRIPT (run 2), in the syntax of `fmv-client --battle-script`.
# The mode FAILS unless:
#   * every bot exits 0 and entered exactly one battle per run, the same battle as the other bot, each battle
#     was REPLAYED by each bot from its own scene.bin/kernel.bin and matched ("verified by replay"), 0
#     divergences, run 1 ended in Victory and run 2 in Escaped;
#   * the commands the server applied for the bots' own characters, read from each bot's closing
#     `headless: battles:` breakdown "(N attack, N magic, N item, N defend, N limit, N escape)" and summed
#     over both bots and both runs, include each of magic, item, defend, limit and escape at least once
#     (a missing or unparseable breakdown is a failure, never a zero);
#   * the server's own key=value lines agree: one "magic used", "defend used", "item used", "limit used" and
#     "escape tried" line per command the bots counted (and an "escape tried ... result=escaped" for run 2),
#     with the item's stock going down (left=N) and every queued write saved when the server stops;
#   * both "battle started" lines say limits=[255, 255] (the --debug-limit-full gauge), the second one says how
#     hard the fight is to run from (run_difficulty=N, a number), and the server warned about the debug flag
#     when it started;
#   * the debug gauge is never stored: no "limit kept" line says limit=255, and the escape battle, in which
#     nobody spent a gauge, kept none (unless a member was knocked out and kept 0);
#   * each bot's script line "entries=N applied=N skipped=N refused=N lost=N unused=N" adds up, and the
#     default script B skipped exactly its one impossible entry.
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
# --progress runs the bots three times, so each battle run is shorter (a battle takes a few seconds);
# an explicit --duration still wins.
PROGRESS_DURATION=16
DURATION_SET=0
# md1stin's first random-encounter formation in the Steam build (two enemies): a starting party wins
# it every time and both members attack in every fight (checked over 200 seeds by
# crates/fmv-server/tests/battle_real.rs, `the_smoke_formations_are_won_every_time_...`).
FORMATION=301
TICK_RATE=240
# Empty: every battle gets a fresh seed. --seed N pins it (the server's --battle-seed).
SEED=
BATTLE_SPEED=0
BOT=idle
PARTY_SIZE=2
# --commands runs the bots twice and a battle with a script takes a few seconds, so each run is short (an explicit
# --duration still wins).
COMMANDS_DURATION=12
# The scripts of --commands (see the header; fmv-client --battle-script syntax). Overridable to try its failure paths.
# Potion is item 0, Cure is spell 0 (checked against the owner's data by the --ignored real-data tests of
# fmv-ff7-battle and fmv-client); spell 9999 does not exist, so script B has one entry that must be skipped.
SCRIPT_A="${FMV_SMOKE_SCRIPT_A:-defend,item:0,magic:0,magic:0:all,limit}"
SCRIPT_B="${FMV_SMOKE_SCRIPT_B:-magic:0,magic:9999,item:0,defend,limit}"
ESCAPE_SCRIPT="${FMV_SMOKE_ESCAPE_SCRIPT:-escape,escape,escape,escape,escape,escape}"

usage() {
  cat <<'EOF'
Usage: scripts/battle-smoke.sh [--fixture|--random|--progress|--commands] [--release|--debug]
                               [--bin-dir DIR] [--build] [--library STATE_DIR] [--duration SECS]
                               [--formation ID] [--tick-rate TPS] [--battle-speed N] [--seed N]
                               [--bot idle|circle] [--keep-logs] [-h|--help]

Real-data mode (default): two headless bots form a party on the real FF7 field md1stin and fight
one shared battle started with --debug-encounters; passes when both report battles=1 victories=1
for the same battle, each replayed it from its own battle data ("verified by replay") and both
members acted (the server's commands=[a, b] are both at least 1) and told their inventory (the SMOKE
line's items=N). --random: the same bots walk
md1stin without --debug-encounters and the server rolls the field's own encounter table (battles
start by themselves); every battle they enter must be verified by replay. --fixture: checks that
a FIXTURE server refuses battles cleanly. --progress: the real-data bots fight, then reconnect with
their saved tokens and fight again, then reconnect once more: each run's progress notes must be
consistent (a win pays experience and a level where the threshold was crossed), the second battle
must carry the level gained and the limit gauges the first battle ended with (protocol v7), and every
reconnect must show what the run before ended with. --commands: every battle command over real
processes: the server runs with --debug-limit-full (every member starts with a full limit gauge) and
the bots follow a --battle-script, once to use defend, item, magic and limit in a battle they win and
once to escape; each command must be applied by the server at least once (the bots' own breakdown
"(N attack, N magic, N item, N defend, N limit, N escape)" and the server's own log lines must agree)
and every battle must be verified by replay with 0 divergences. The scripts can be overridden with
FMV_SMOKE_SCRIPT_A, FMV_SMOKE_SCRIPT_B and FMV_SMOKE_ESCAPE_SCRIPT (see the header of this script).

  --library DIR     state directory that contains library/ (default $FMV_FF7_LIBRARY, else
                     ~/.local/share/ff-multiverse); only the library is read, never written
  --duration SECS    how long each bot runs (default 40; --progress 16, --commands 12)
  --formation ID     the enemy formation of the debug battle (default 301: a two-enemy formation of
                     md1stin's own encounter table in the Steam build)
  --tick-rate TPS    engine ticks per second of battle time (default 240)
  --seed N           pin the seed of every battle (the server's --battle-seed); by default each
                     battle gets a fresh one. Seed 5 makes formation 301 drop a Potion (see the
                     real-data test the_formations_of_the_first_field_drop_items_for_some_seeds in
                     crates/fmv-server/tests/battle_real.rs), which runs the drop chain of --progress
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

# first_line_with <file> <needle>: print the first line containing needle; fail if none (pure bash,
# like last_line_with).
first_line_with() {
  local file="$1" needle="$2" line
  [[ -r $file ]] || return 1
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$needle"* ]]; then
      printf '%s\n' "$line"
      return 0
    fi
  done <"$file"
  return 1
}

# nth_line_with <file> <needle> <n>: print the n-th (1-based) line containing needle; fail if none.
nth_line_with() {
  local file="$1" needle="$2" want="$3" line seen=0
  [[ -r $file ]] || return 1
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$needle"* ]]; then
      seen=$((seen + 1))
      if ((seen == want)); then
        printf '%s\n' "$line"
        return 0
      fi
    fi
  done <"$file"
  return 1
}

# levels_of <server log line>: the levels the server logged for a battle's members (`levels=[6, 6]`),
# sorted, as "6 6" (nothing if the line has none).
levels_of() {
  local line sorted
  local -a levels
  line=$(strip_ansi "$1")
  if [[ $line =~ levels=\[([0-9,\ ]*)\] ]]; then
    read -r -a levels <<<"${BASH_REMATCH[1]//,/ }"
    sorted=$(printf '%s\n' "${levels[@]}" | sort -n | tr '\n' ' ')
    printf '%s' "${sorted% }"
  fi
}

# limits_of <server log line>: the limit gauges the server logged for a battle's members (`limits=[0, 37]`
# on "battle started": what each began with; on "battle ended": what each ended with), sorted, as "0 37"
# (nothing if the line has none). Sorted like levels_of: the two bots may be seated in either order.
limits_of() {
  local line sorted
  local -a limits
  line=$(strip_ansi "$1")
  if [[ $line =~ limits=\[([0-9,\ ]*)\] ]]; then
    read -r -a limits <<<"${BASH_REMATCH[1]//,/ }"
    sorted=$(printf '%s\n' "${limits[@]}" | sort -n | tr '\n' ' ')
    printf '%s' "${sorted% }"
  fi
}

# count_limits_queued <server log>: how many limit gauges the server queued for the saver: its
# "limit kept battle=.. saving=queued" lines.
count_limits_queued() {
  local line n=0
  [[ -r $1 ]] || {
    printf '0'
    return 0
  }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"limit kept battle="* && $line == *"saving=queued"* ]]; then
      n=$((n + 1))
    fi
  done <"$1"
  printf '%s' "$n"
}

# count_queued <server log> <prefix>: how many lines start their message with <prefix> (e.g. "item used battle=")
# and say saving=queued: the writes the server handed to its saver.
count_queued() {
  local line n=0
  [[ -r $1 ]] || {
    printf '0'
    return 0
  }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"$2"* && $line == *"saving=queued"* ]]; then
      n=$((n + 1))
    fi
  done <"$1"
  printf '%s' "$n"
}

# breakdown_of <client log>: the commands of the client's own character that the server applied, by kind, from the
# parenthesis of its last `headless: battles:` line, "(N attack, N magic, N item, N defend, N limit, N escape)", as
# "<attack> <magic> <item> <defend> <limit> <escape>". Nothing if the line is missing or the parenthesis is not
# exactly that: a caller must treat that as a failure, never as zeros.
breakdown_of() {
  local line re
  line=$(last_line_with "$1" "headless: battles:") || return 0
  line=$(strip_ansi "$line")
  re='\(([0-9]+) attack, ([0-9]+) magic, ([0-9]+) item, ([0-9]+) defend, ([0-9]+) limit, ([0-9]+) escape\)'
  if [[ $line =~ $re ]]; then
    printf '%s %s %s %s %s %s' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}" "${BASH_REMATCH[4]}" "${BASH_REMATCH[5]}" "${BASH_REMATCH[6]}"
  fi
}

# script_has <script> <entry>: is <entry> one of the comma-separated entries of <script>?
script_has() {
  [[ ",$1," == *",$2,"* ]]
}

# progress_note <first|last> <client log>: that `progress:` note of a client (the client's `progress`
# module writes them: `progress: Cloud level=7 exp=626 exp_to_next=323 gil=20 ap=4 leveled_up=true (was
# level 6)`), ANSI stripped; nothing if it has none.
progress_note() {
  local which="$1" log="$2" line
  if [[ $which == first ]]; then
    line=$(first_line_with "$log" "progress: ") || return 0
  else
    line=$(last_line_with "$log" "progress: ") || return 0
  fi
  strip_ansi "$line"
}

# inventory_note <first|last> <client log>: that `inventory: stacks=S total=T items=item:count,..` note of
# a client (the client's battle module writes one per Inventory message of the server, which always
# carries the WHOLE inventory; `items=-` is an empty one), ANSI stripped; nothing if it has none.
inventory_note() {
  local which="$1" log="$2" line
  if [[ $which == first ]]; then
    line=$(first_line_with "$log" "inventory: stacks=") || return 0
  else
    line=$(last_line_with "$log" "inventory: stacks=") || return 0
  fi
  strip_ansi "$line"
}

# dropped_total <server log line>: the number of items a `battle reward ... dropped=[(item, count), ..]`
# line says were dealt to that member (0 for a line without drops).
dropped_total() {
  local line rest sum=0
  line=$(strip_ansi "$1")
  if [[ $line =~ dropped=\[([^]]*)\] ]]; then
    rest="${BASH_REMATCH[1]}"
    while [[ $rest =~ \(([0-9]+),\ ([0-9]+)\) ]]; do
      sum=$((sum + BASH_REMATCH[2]))
      rest="${rest#*"${BASH_REMATCH[0]}"}"
    done
  fi
  printf '%s' "$sum"
}

# check_saved_items <server log>: every `progress saved` line carries the inventory's total (items=N): the
# saver reports the inventory it stored (or would store) with the progress.
check_saved_items() {
  local log="$1" line total bad=0 seen=0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"progress saved account="* ]]; then
      seen=$((seen + 1))
      total=$(field "$(strip_ansi "$line")" items)
      [[ $total =~ ^[0-9]+$ ]] && ((total >= 1)) || bad=$((bad + 1))
    fi
  done <"$log"
  ((bad == 0)) || problem "$bad of $seen 'progress saved' line(s) of the server carry no items=N total of at least 1"
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
  if [[ -n $SEED ]]; then
    server_args+=(--battle-seed "$SEED")
  fi
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
      # Character progress (protocol v5): the line carries the level and experience the server last
      # reported, which the won battle has just paid (see --progress for the full chain).
      value=$(field "$smoke_line" level)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "client $n: the SMOKE line has no level of at least 1 (level=$value): $smoke_line"
      value=$(field "$smoke_line" exp)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "client $n: the SMOKE line has no experience (exp=$value): $smoke_line"
      # Inventory (protocol v6): a new account carries the game's starting stock, and the server told it.
      value=$(field "$smoke_line" items)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 1)) || problem "client $n: the SMOKE line has no items (items=$value): the server told the bot no inventory: $smoke_line"
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
    # Both members acted: the server's own count of accepted commands per actor (of any kind: attacks,
    # and spells when a bot was hurt), e.g. [2, 3].
    local counts actor_counts=() c
    counts=$(commands_of "$ended")
    read -r -a actor_counts <<<"$counts"
    ((${#actor_counts[@]} == 2)) || problem "the server's battle-end line has no two-member commands=[..]: $ended"
    for c in "${actor_counts[@]}"; do
      ((c >= 1)) || problem "a member of the battle never gave a command (server: commands per actor [$counts])"
    done
  fi
  (($(count_lines_with "$WORK/server.log" "battle started") <= 1)) || problem "the server started more than one battle"

  stop_server
  # Stopping the server saved everything queued: both members' payments are in the store.
  (($(count_lines_with "$WORK/server.log" "progress saved account=") >= 2)) || problem "the server did not save the progress of both members (progress saved lines: $(count_lines_with "$WORK/server.log" "progress saved account="))"
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
  say "PASS: two bots fought one shared battle on the real field, both gave commands, each replayed it from its own battle data, won it and carries an inventory; $state_real unchanged ($after)"
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
  if [[ -n $SEED ]]; then
    server_args+=(--battle-seed "$SEED")
  fi
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

# What each bot saw at the end of the previous run of --progress (the next run must be told exactly
# this on joining), the level each was told on joining in each run, and a line per bot and run for the
# PASS message.
declare -A PREV_LEVEL=() PREV_EXP=() RUN_JOIN_LEVEL=()
# The same for the items: what each bot carried at the end of the previous run (`item:count,..`), the
# total each run's battle added to the two bots' inventories together, and the stock they started with.
declare -A PREV_ITEMS=() RUN_GAIN=()
TOTAL_DROPPED=0
PROGRESS_SUMMARY=()

# start_bots <run> <duration> <battle|idle>: run bots A and B against the server, which stays up, and
# wait for both. Every run uses the same client state directories, so after the first one the bots
# log in with the tokens it saved (a saved account is used when --name names it; registering again
# needs --register).
start_bots() {
  local run="$1" duration="$2" kind="$3" n
  CPID=()
  CRC=()
  local common=(--server "$CONNECT" --headless --duration "$duration" --bot "$BOT" --ff7-library "$WORK/lib-state")
  if [[ $kind == battle ]]; then
    "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" \
      --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party "$PARTY_SIZE" \
      >"$WORK/client-A.$run.log" 2>&1 &
    CPID[A]=$!
    "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" --party-accept --battle-auto \
      >"$WORK/client-B.$run.log" 2>&1 &
    CPID[B]=$!
  else
    for n in "${CLIENT_NAMES[@]}"; do
      "$CLIENT_BIN" "${common[@]}" --name "$n" --state-dir "$WORK/client-$n" >"$WORK/client-$n.$run.log" 2>&1 &
      CPID[$n]=$!
    done
  fi
  wait_clients $((duration + CLIENT_GRACE))
}

# check_items <run> <battle|idle> <client> <client log> <SMOKE line>: the inventory chain of one run of one
# bot (see the --progress paragraph of the header). It is told on joining: the starting stock in run 1, and
# in a reconnect exactly the items the run before ended with (saved, not given again, read back). A run
# that does not fight leaves it alone, and the SMOKE line's items=N is the last total the server reported.
# What a won battle adds is summed over both bots in RUN_GAIN and compared with the server's log by
# check_drops.
check_items() {
  local run="$1" kind="$2" n="$3" log="$4" smoke_line="$5"
  local join end t0 t1 list0 list1
  join=$(inventory_note first "$log")
  end=$(inventory_note last "$log")
  if [[ -z $join ]]; then
    problem "$run: client $n was not told its inventory on joining (no 'inventory: stacks=' note)"
    return 0
  fi
  t0=$(field "$join" total)
  list0=$(field "$join" items)
  if [[ ! $t0 =~ ^[0-9]+$ || -z $list0 ]]; then
    problem "$run: client $n: the inventory note on joining has no usable total/items: $join"
    return 0
  fi
  if [[ -z ${PREV_ITEMS[$n]:-} ]]; then
    # The first login of a new account: the game's own starting stock, never empty.
    ((t0 >= 1)) || problem "$run: client $n: a new account was told an empty inventory instead of the starting stock: $join"
  elif [[ $list0 != "${PREV_ITEMS[$n]}" ]]; then
    problem "$run: client $n joined carrying [$list0], but the run before ended with [${PREV_ITEMS[$n]}]: the inventory was not kept (or the starting stock was given again)"
  fi
  t1=$t0
  list1=$list0
  if [[ -n $end ]]; then
    t1=$(field "$end" total)
    list1=$(field "$end" items)
  fi
  if [[ ! $t1 =~ ^[0-9]+$ || -z $list1 ]]; then
    problem "$run: client $n: the last inventory note has no usable total/items: $end"
    return 0
  fi
  [[ $(field "$smoke_line" items) == "$t1" ]] || problem "$run: client $n: the SMOKE line says items=$(field "$smoke_line" items) but its last inventory note says total=$t1: $smoke_line"
  if [[ $kind == battle ]]; then
    RUN_GAIN[$run]=$((${RUN_GAIN[$run]:-0} + t1 - t0))
  elif [[ $list1 != "$list0" ]]; then
    problem "$run: client $n did not fight, yet its inventory changed from [$list0] to [$list1]"
  fi
  PREV_ITEMS[$n]=$list1
  PROGRESS_SUMMARY+=("$run client $n: items $t0 -> $t1 ([$list0] -> [$list1])")
}

# check_drops <run> <number of the first "battle reward" line of the run>: what the server dealt out in the
# battle of <run> (the "battle reward" lines of its two members, which it logs one after the other) is
# exactly what the two bots' inventories grew by together. Skipped, with a note, when an item was used in
# a battle (the bots use none: a use would make the sum differ for a good reason).
check_drops() {
  local run="$1" first="$2" dropped=0 line k
  if log_has "$WORK/server.log" "item used battle="; then
    say "note: an item was used in a battle, so the drops of $run are not compared with the inventories"
    return 0
  fi
  for k in "$first" $((first + 1)); do
    line=$(nth_line_with "$WORK/server.log" "battle reward battle=" "$k" || true)
    if [[ -z $line ]]; then
      problem "$run: the server logged no 'battle reward' line number $k"
      return 0
    fi
    dropped=$((dropped + $(dropped_total "$line")))
  done
  TOTAL_DROPPED=$((TOTAL_DROPPED + dropped))
  [[ ${RUN_GAIN[$run]:-0} == "$dropped" ]] || problem "$run: the bots' inventories grew by ${RUN_GAIN[$run]:-0} item(s) together, but the server logged $dropped item(s) dropped to the party"
}

# check_phase <run> <battle|idle>: the checks of one run of both bots (see the --progress paragraph of
# the header): clean exit, a SMOKE line that ends with the last level and exp the server reported, a
# progress note on joining that equals what the run before ended with, and for a battle run a win that
# paid experience and a level exactly where the threshold (exp + exp_to_next on joining) was crossed.
check_phase() {
  local run="$1" kind="$2" n log smoke_line join_note end_note value
  local l0 e0 x0 l1 e1 up
  for n in "${CLIENT_NAMES[@]}"; do
    log="$WORK/client-$n.$run.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "$run: client $n was still running ${CLIENT_GRACE}s after its duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "$run: client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if ! smoke_line=$(last_line_with "$log" "SMOKE name="); then
      problem "$run: client $n printed no SMOKE line"
      continue
    fi
    smoke_line=$(strip_ansi "$smoke_line")
    SMOKE_LINES+=("$run client $n: $smoke_line")
    [[ $(field "$smoke_line" fixture) == false ]] || problem "$run: client $n: fixture is not false: $smoke_line"
    if log_has "$log" "DIVERGENCE"; then
      problem "$run: client $n reported a BATTLE DIVERGENCE (its replay disagreed with the server)"
    fi
    check_items "$run" "$kind" "$n" "$log" "$smoke_line"

    # What the server said on joining.
    join_note=$(progress_note first "$log")
    end_note=$(progress_note last "$log")
    if [[ $join_note != *"(on joining)"* ]]; then
      problem "$run: client $n was not told its progress on joining (first note: '$join_note')"
      continue
    fi
    l0=$(field "$join_note" level)
    e0=$(field "$join_note" exp)
    x0=$(field "$join_note" exp_to_next)
    if [[ ! $l0 =~ ^[0-9]+$ || ! $e0 =~ ^[0-9]+$ || ! $x0 =~ ^[0-9]+$ ]] || ((l0 < 1)); then
      problem "$run: client $n: the note on joining has no usable level/exp/exp_to_next: $join_note"
      continue
    fi
    # A reconnect is told what the run before ended with: the progress was saved and read back.
    if [[ -n ${PREV_LEVEL[$n]:-} ]] && [[ $l0 != "${PREV_LEVEL[$n]}" || $e0 != "${PREV_EXP[$n]}" ]]; then
      problem "$run: client $n joined with level $l0 and $e0 exp, but the run before ended with level ${PREV_LEVEL[$n]} and ${PREV_EXP[$n]} exp: the progress was not kept"
    fi
    RUN_JOIN_LEVEL["$run.$n"]=$l0
    l1=$l0
    e1=$e0
    if [[ $kind == battle ]]; then
      for value in "party=2" "battles=1" "victories=1"; do
        [[ $smoke_line == *" $value"* ]] || problem "$run: client $n: the SMOKE line lacks $value: $smoke_line"
      done
      # The mirror replayed the battle from the client's own data, with the levels the server sent.
      check_mirror "$n" "$log" 1
      l1=$(field "$end_note" level)
      e1=$(field "$end_note" exp)
      up=$(field "$end_note" leveled_up)
      if [[ ! $l1 =~ ^[0-9]+$ || ! $e1 =~ ^[0-9]+$ ]]; then
        problem "$run: client $n: no progress note after the battle (last note: '$end_note')"
        continue
      fi
      ((e1 > e0)) || problem "$run: client $n: the won battle paid no experience ($e0 -> $e1)"
      if ((x0 > 0 && e1 >= e0 + x0)); then
        # The threshold of the next level was crossed: a level was gained, and the server said so.
        if ! { ((l1 > l0)) && [[ $up == true ]]; }; then
          problem "$run: client $n: experience went $e0 -> $e1, past the threshold at $((e0 + x0)), but the level went $l0 -> $l1 (leveled_up=$up)"
        fi
      elif ((l1 != l0)) || [[ $up != false ]]; then
        problem "$run: client $n: experience went $e0 -> $e1, short of the threshold at $((e0 + x0)), but the level went $l0 -> $l1 (leveled_up=$up)"
      fi
    else
      [[ $smoke_line == *" battles=0 "* ]] || problem "$run: client $n: a run that does not fight entered a battle: $smoke_line"
      if [[ $end_note != "$join_note" ]]; then
        problem "$run: client $n was told more than its progress on joining although it did not fight: $end_note"
      fi
    fi
    # The SMOKE line ends with the last thing the server said.
    if [[ $(field "$smoke_line" level) != "$l1" || $(field "$smoke_line" exp) != "$e1" ]]; then
      problem "$run: client $n: the SMOKE line says level=$(field "$smoke_line" level) exp=$(field "$smoke_line" exp) but its last progress note says level $l1, exp $e1: $smoke_line"
    fi
    PREV_LEVEL[$n]=$l1
    PREV_EXP[$n]=$e1
    PROGRESS_SUMMARY+=("$run client $n: level $l0 -> $l1, exp $e0 -> $e1")
  done
}

# joined_levels <run>: the levels both bots were told on joining in <run>, sorted, as "6 6".
joined_levels() {
  local sorted
  sorted=$(printf '%s\n' "${RUN_JOIN_LEVEL[$1.A]:-}" "${RUN_JOIN_LEVEL[$1.B]:-}" | sort -n | tr '\n' ' ')
  printf '%s' "${sorted% }"
}

run_progress() {
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
  if [[ -n $SEED ]]; then
    server_args+=(--battle-seed "$SEED")
  fi
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin && $(field "$READY_LINE" fixture) == false ]] || fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"

  local duration=$BATTLE_DURATION
  ((DURATION_SET)) || duration=$PROGRESS_DURATION
  SMOKE_LINES=()
  say "run 1 of 3: bots A and B form a party and fight one battle (${duration}s)"
  start_bots run1 "$duration" battle
  check_phase run1 battle
  say "run 2 of 3: both log in again with their saved tokens and fight a second battle (${duration}s)"
  start_bots run2 "$duration" battle
  check_phase run2 battle
  say "run 3 of 3: both log in once more and only look at what was kept (5s)"
  start_bots run3 5 idle
  check_phase run3 idle

  # The server's side: two battles of the whole party, the second fought at the levels the first one's
  # win left (the level gained is in the second EncounterStart), one saved payment per member and battle.
  local started_1 started_2 ended_1 got want value
  (($(count_lines_with "$WORK/server.log" "battle started") == 2)) || problem "the server started $(count_lines_with "$WORK/server.log" "battle started") battle(s), expected 2"
  started_1=$(nth_line_with "$WORK/server.log" "battle started" 1 || true)
  started_2=$(nth_line_with "$WORK/server.log" "battle started" 2 || true)
  if [[ -z $started_1 || -z $started_2 ]]; then
    problem "the server did not log both battle starts"
  else
    [[ $started_1 == *"members=2"* && $started_2 == *"members=2"* ]] || problem "a battle did not have both members: [$started_1] [$started_2]"
    got=$(levels_of "$started_1")
    want=$(joined_levels run1)
    [[ $got == "$want" ]] || problem "the first battle was fought at levels [$got], but the bots were told levels [$want] on joining"
    got=$(levels_of "$started_2")
    want=$(joined_levels run2)
    [[ $got == "$want" ]] || problem "the second battle carries levels [$got], but the first win left the bots at levels [$want]: the level gained was not used"
    # The limit gauges (protocol v7): a new account starts with an empty one, and the gauges the first battle
    # ended with are the ones the second starts with (kept in the server's mirror, queued for the saver).
    got=$(limits_of "$started_1")
    [[ $got == "0 0" ]] || problem "the first battle was started with limit gauges [$got], expected [0 0] for two new accounts: $started_1"
    ended_1=$(nth_line_with "$WORK/server.log" "battle ended" 1 || true)
    want=$(limits_of "$ended_1")
    got=$(limits_of "$started_2")
    [[ -n $want && $got == "$want" ]] || problem "the second battle starts with limit gauges [$got], but the first one ended with [$want]: the gauge was not kept (first end: $ended_1)"
    if [[ $want =~ ^0( 0)*$ ]]; then
      say "note: nobody was hurt in the first battle (every limit gauge ended at 0), so the carry-over of a non-empty gauge was not exercised over real processes; these weak enemies hurt somebody in only some runs, so run it again"
    else
      say "the first battle left the limit gauges [$want] and the second one started with exactly those"
    fi
    # Every fight says how hard it is to run from (scene.bin's counter; none: it cannot be run from).
    [[ $started_1 == *" run_difficulty="* && $started_2 == *" run_difficulty="* ]] || problem "a battle start does not say how hard the fight is to run from (run_difficulty=): [$started_1] [$started_2]"
  fi
  local ended
  ended=$(last_line_with "$WORK/server.log" "battle ended" || true)
  [[ $ended == *Victory* ]] || problem "the server's last battle did not end in Victory: $ended"
  # The items: what each win dropped to the party is what the bots' inventories grew by (two "battle
  # reward" lines per battle: the first battle's are lines 1 and 2, the second's 3 and 4).
  check_drops run1 1
  check_drops run2 3
  if ((TOTAL_DROPPED == 0)); then
    say "note: neither battle dropped an item, so the drop chain (deal, tell, save, read back) was not exercised over real processes; --seed 5 makes formation 301 drop a Potion"
  else
    say "the battles dropped $TOTAL_DROPPED item(s) in all, and both bots' inventories grew by exactly that"
  fi

  stop_server
  # Stopping the server saved everything queued: one payment per member per battle, none lost.
  value=$(count_lines_with "$WORK/server.log" "progress saved account=")
  [[ $value == 4 ]] || problem "the server logged $value 'progress saved' line(s), expected 4 (two members, two battles)"
  check_saved_items "$WORK/server.log"
  # Every limit gauge the server queued was written too (one "limit saved" line per "limit kept ... saving=queued";
  # the "battle reward" lines say saving=queued as well, so only the "limit kept" ones count).
  got=$(count_limits_queued "$WORK/server.log")
  value=$(count_lines_with "$WORK/server.log" "limit saved account=")
  [[ $value == "$got" ]] || problem "the server queued $got limit gauge(s) for the saver but logged $value 'limit saved' line(s)"
  if log_has "$WORK/server.log" "NOT SAVED" || log_has "$WORK/server.log" "NOT QUEUED"; then
    problem "the server lost a payment: $(last_line_with "$WORK/server.log" "NOT SAVED" || last_line_with "$WORK/server.log" "NOT QUEUED")"
  fi
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local l
  for l in "${SMOKE_LINES[@]}"; do
    say "$l"
  done
  for l in "${PROGRESS_SUMMARY[@]}"; do
    say "progress, $l"
  done
  say "server: $started_1"
  say "server: $started_2"
  RESULT=pass
  say "PASS: two bots fought two battles on the real field across three logins; each win paid experience (and a level where the threshold was crossed), the progress and the inventory (the starting stock once, plus the drops) were saved and read back on every reconnect, and the second battle was fought at the level and with the limit gauges the first one left; $state_real unchanged ($after)"
}

# --commands: what the server applied for the bots' own characters, by kind, summed over every bot and run (read from
# each bot's `headless: battles:` breakdown; see breakdown_of).
COMMAND_KINDS=(attack magic item defend limit escape)
declare -A CMD_TOTAL=([attack]=0 [magic]=0 [item]=0 [defend]=0 [limit]=0 [escape]=0)

# start_script_bots <run> <duration> <script A> <script B>: bots A and B against the running server, each with its own
# --battle-script (they log in again with the tokens the run before saved), and wait for both.
start_script_bots() {
  local run="$1" duration="$2" script_a="$3" script_b="$4"
  CPID=()
  CRC=()
  local common=(--server "$CONNECT" --headless --duration "$duration" --bot "$BOT" --ff7-library "$WORK/lib-state")
  "$CLIENT_BIN" "${common[@]}" --name A --state-dir "$WORK/client-A" \
    --party-invite-first-peer --battle-auto --debug-encounters --battle-wait-party "$PARTY_SIZE" --battle-script "$script_a" \
    >"$WORK/client-A.$run.log" 2>&1 &
  CPID[A]=$!
  "$CLIENT_BIN" "${common[@]}" --name B --state-dir "$WORK/client-B" \
    --party-accept --battle-auto --battle-script "$script_b" \
    >"$WORK/client-B.$run.log" 2>&1 &
  CPID[B]=$!
  wait_clients $((duration + CLIENT_GRACE))
}

# check_commands_phase <cmds|escape> <Victory|Escaped>: the checks of one run of both bots of --commands (see the header):
# clean exit, one battle that both bots entered, replayed from their own data and matched, that ended the way the run
# is meant to, a breakdown of the commands the server applied (added to CMD_TOTAL) and a script line that adds up.
check_commands_phase() {
  local run="$1" want="$2" n log smoke_line counts script_line value k id wins=1
  local e a s r l u
  local -a c ids=()
  [[ $want == Victory ]] || wins=0
  for n in "${CLIENT_NAMES[@]}"; do
    log="$WORK/client-$n.$run.log"
    if [[ ${CRC[$n]:-} == timeout ]]; then
      problem "$run: client $n was still running ${CLIENT_GRACE}s after its duration and was killed"
    elif [[ ${CRC[$n]:-} != 0 ]]; then
      problem "$run: client $n exited with status ${CRC[$n]:-unknown}"
    fi
    if ! smoke_line=$(last_line_with "$log" "SMOKE name="); then
      problem "$run: client $n printed no SMOKE line"
      continue
    fi
    smoke_line=$(strip_ansi "$smoke_line")
    SMOKE_LINES+=("$run client $n: $smoke_line")
    for value in "party=2" "battles=1" "victories=$wins"; do
      [[ $smoke_line == *" $value "* ]] || problem "$run: client $n: the SMOKE line lacks $value: $smoke_line"
    done
    [[ $(field "$smoke_line" fixture) == false ]] || problem "$run: client $n: fixture is not false: $smoke_line"
    if log_has "$log" "DIVERGENCE"; then
      problem "$run: client $n reported a BATTLE DIVERGENCE (its replay disagreed with the server)"
    fi
    # The bot REPLAYED the battle from its own scene.bin/kernel.bin and matched (one battle entered, one verified, none
    # diverged), and it ended the way this run is meant to: won by the party, or escaped from.
    check_mirror "$n ($run)" "$log" 1
    log_has "$log" "ended in $want" || problem "$run: client $n never saw the battle end in $want"
    [[ $(summary_number "$log" 'escaped,') == "$((1 - wins))" ]] || problem "$run: client $n: its summary does not say $((1 - wins)) battle(s) escaped: $(last_line_with "$log" "headless: battles:" || true)"
    ids+=("$(battle_id_of "$log" || true)")

    # The commands the server applied for this bot's own character, by kind: a missing breakdown is a failure.
    counts=$(breakdown_of "$log")
    if [[ -z $counts ]]; then
      problem "$run: client $n printed no parseable '(N attack, N magic, N item, N defend, N limit, N escape)' breakdown on its 'headless: battles:' line"
    else
      read -r -a c <<<"$counts"
      for k in "${!COMMAND_KINDS[@]}"; do
        CMD_TOTAL[${COMMAND_KINDS[$k]}]=$((CMD_TOTAL[${COMMAND_KINDS[$k]}] + c[k]))
      done
    fi

    # How the script went: every entry is exactly one of applied, skipped, refused, lost or unused.
    script_line=$(last_line_with "$log" "headless: battle script:" || true)
    script_line=$(strip_ansi "$script_line")
    if [[ -z $script_line ]]; then
      problem "$run: client $n printed no 'headless: battle script:' line"
      continue
    fi
    e=$(field "$script_line" entries)
    a=$(field "$script_line" applied)
    s=$(field "$script_line" skipped)
    r=$(field "$script_line" refused)
    l=$(field "$script_line" lost)
    u=$(field "$script_line" unused)
    if [[ ! $e =~ ^[0-9]+$ || ! $a =~ ^[0-9]+$ || ! $s =~ ^[0-9]+$ || ! $r =~ ^[0-9]+$ || ! $l =~ ^[0-9]+$ || ! $u =~ ^[0-9]+$ ]]; then
      problem "$run: client $n: its script line has no numeric entries/applied/skipped/refused/lost/unused: $script_line"
      continue
    fi
    if ((a + s + r + l + u != e)); then
      problem "$run: client $n: its script entries do not add up ($e entries; $a applied + $s skipped + $r refused + $l lost + $u unused): $script_line"
    fi
    # The default scripts of run 1 are exactly known: A's entries can all be given, and B's one impossible entry
    # (spell 9999) is skipped, counted and not retried, with the entry after it given in the same turn.
    if [[ $run == cmds && $n == A && -z ${FMV_SMOKE_SCRIPT_A:-} ]] && ((s != 0)); then
      problem "$run: client $n: the default script skipped $s entr(y/ies), expected none: $script_line"
    fi
    if [[ $run == cmds && $n == B && -z ${FMV_SMOKE_SCRIPT_B:-} ]] && ((s != 1 || a < 3)); then
      problem "$run: client $n: the default script should skip exactly its one impossible entry (magic:9999) and apply the others (at least 3): $script_line"
    fi
  done
  if [[ -z ${ids[0]:-} || ${ids[0]} != "${ids[1]:-}" ]]; then
    problem "$run: the bots did not enter the same battle (A: '${ids[0]:-}', B: '${ids[1]:-}')"
  fi
  id=${ids[0]:-}
  PROGRESS_SUMMARY+=("$run: both bots fought ${id:-?}")
}

run_commands() {
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
    --debug-encounters --debug-limit-full --no-random-encounters --battle-speed "$BATTLE_SPEED" --battle-tick-rate "$TICK_RATE")
  server_args+=(--debug-formation "$FORMATION")
  if [[ -n $SEED ]]; then
    server_args+=(--battle-seed "$SEED")
  fi
  say "starting server: $SERVER_BIN ${server_args[*]}"
  "$SERVER_BIN" "${server_args[@]}" >"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  wait_ready "$WORK/server.log"
  say "server READY: $READY_LINE"
  [[ $(field "$READY_LINE" area) == Ff7/md1stin && $(field "$READY_LINE" fixture) == false ]] || fail "the server is not hosting the real FF7 field md1stin: $READY_LINE"

  local duration=$BATTLE_DURATION
  ((DURATION_SET)) || duration=$COMMANDS_DURATION
  SMOKE_LINES=()
  say "run 1 of 2 (cmds): A --battle-script $SCRIPT_A, B --battle-script $SCRIPT_B (${duration}s)"
  start_script_bots cmds "$duration" "$SCRIPT_A" "$SCRIPT_B"
  check_commands_phase cmds Victory
  say "run 2 of 2 (escape): A and B --battle-script $ESCAPE_SCRIPT (${duration}s)"
  start_script_bots escape "$duration" "$ESCAPE_SCRIPT" "$ESCAPE_SCRIPT"
  check_commands_phase escape Escaped

  # What the bots counted, summed over both bots and both runs: every command the server applied for them.
  local pair kind needle value line got started_1 started_2 ended_1 ended_2 escape_battle
  for kind in magic item defend limit escape; do
    ((CMD_TOTAL[$kind] >= 1)) || problem "the server applied no $kind command for either bot in either run (the bots' breakdowns add up to ${CMD_TOTAL[$kind]})"
  done

  # The server's side: two battles of the whole party, both started with the full gauge --debug-limit-full gives.
  log_has "$WORK/server.log" "DEBUG: --debug-limit-full is on" || problem "the server did not warn about --debug-limit-full when it started"
  (($(count_lines_with "$WORK/server.log" "battle started") == 2)) || problem "the server started $(count_lines_with "$WORK/server.log" "battle started") battle(s), expected 2 (one per run)"
  started_1=$(nth_line_with "$WORK/server.log" "battle started" 1 || true)
  started_2=$(nth_line_with "$WORK/server.log" "battle started" 2 || true)
  ended_1=$(nth_line_with "$WORK/server.log" "battle ended" 1 || true)
  ended_2=$(nth_line_with "$WORK/server.log" "battle ended" 2 || true)
  escape_battle=
  if [[ -z $started_1 || -z $started_2 ]]; then
    problem "the server did not log both battle starts"
  else
    for line in "$started_1" "$started_2"; do
      [[ $line == *"members=2"* ]] || problem "a battle did not have both members: $line"
      got=$(limits_of "$line")
      [[ $got == "255 255" ]] || problem "a battle was started with limit gauges [$got], expected [255 255] (the --debug-limit-full gauge): $line"
    done
    escape_battle=$(field "$(strip_ansi "$started_2")" battle)
    value=$(field "$(strip_ansi "$started_2")" run_difficulty)
    [[ $value =~ ^[0-9]+$ ]] || problem "the escape battle's start says run_difficulty='$value' (a number is expected: this fight can be run from): $started_2"
  fi
  [[ $ended_1 == *"outcome=Victory"* ]] || problem "the server's first battle did not end in Victory: $ended_1"
  [[ $ended_2 == *"outcome=Escaped"* ]] || problem "the server's second battle did not end in Escaped: $ended_2"

  # The server's own key=value record of every command it applied must agree with what the bots counted (one line
  # per command, both bots together; a plain attack is not logged).
  for pair in "magic|magic used battle=" "item|item used battle=" "defend|defend used battle=" "limit|limit used battle=" "escape|escape tried battle="; do
    kind=${pair%%|*}
    needle=${pair#*|}
    value=$(count_lines_with "$WORK/server.log" "$needle")
    ((value >= 1)) || problem "the server logged no '$needle' line"
    ((value == CMD_TOTAL[$kind])) || problem "the server logged $value '$needle' line(s), but the bots counted ${CMD_TOTAL[$kind]} $kind command(s) applied"
  done
  log_has "$WORK/server.log" "magic used battle=" && { log_has "$WORK/server.log" " spell=0 " || problem "the server's 'magic used' lines name no spell=0 (Cure)"; }
  # Whole sides (protocol v8): the default script A casts Cure on the whole party once (its All materia use), and the
  # server's record of it says so; a one-target cast says the actor's number as it always did.
  if [[ -z ${FMV_SMOKE_SCRIPT_A:-} ]]; then
    log_has "$WORK/server.log" " spell=0 " && log_has "$WORK/server.log" "target=all-allies" || problem "the server's 'magic used' lines show no Cure on the whole party (target=all-allies), which the default script A gives"
    value=$(count_lines_with "$WORK/server.log" "target=all-allies")
    [[ $value == 1 ]] || problem "the server logged $value whole-party cast(s) (target=all-allies), expected exactly 1 (only bot A's script has one)"
  fi
  log_has "$WORK/server.log" "item used battle=" && { log_has "$WORK/server.log" " item=0 left=" || problem "the server's 'item used' lines name no item=0 (Potion)"; }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == *"item used battle="* ]]; then
      value=$(field "$(strip_ansi "$line")" left)
      [[ $value =~ ^[0-9]+$ ]] || problem "an 'item used' line has no numeric left=: $line"
    elif [[ $line == *"limit used battle="* ]]; then
      value=$(field "$(strip_ansi "$line")" attack)
      [[ $value =~ ^[0-9]+$ ]] && ((value >= 128)) || problem "a 'limit used' line has no limit id (128 and up) in attack=: $line"
    fi
  done <"$WORK/server.log"
  # Exactly one escape got away (it ended the battle for everyone), and nothing else did.
  value=$(count_lines_with "$WORK/server.log" "result=escaped")
  [[ $value == 1 ]] || problem "the server logged $value successful escape(s), expected exactly 1"
  # An escape pays nothing: no reward line for the escape battle, and the won one paid both members.
  if [[ -n $escape_battle ]]; then
    value=$(count_lines_with "$WORK/server.log" "battle reward battle=$escape_battle ")
    [[ $value == 0 ]] || problem "the server paid $value reward(s) for the battle that was escaped from (battle=$escape_battle)"
    # Under --debug-limit-full NO gauge is ever stored, spent or not: no 'limit kept' line at all, and one
    # 'limit not kept battle=N reason=debug-limit-full' line for every battle that ended.
    value=$(count_lines_with "$WORK/server.log" "limit kept battle=")
    [[ $value == 0 ]] || problem "the server kept $value limit gauge(s) under --debug-limit-full: $(last_line_with "$WORK/server.log" "limit kept battle=")"
    value=$(count_lines_with "$WORK/server.log" "battle ended battle=")
    got=$(count_lines_with "$WORK/server.log" "limit not kept battle=")
    [[ $got == "$value" ]] || problem "the server skipped the gauge save for $got of $value ended battle(s) ('limit not kept ... reason=debug-limit-full'), expected all"
  fi

  stop_server
  # Stopping the server saved everything queued: the two payments of the won battle, every gauge and item use that was queued.
  value=$(count_lines_with "$WORK/server.log" "progress saved account=")
  [[ $value == 2 ]] || problem "the server logged $value 'progress saved' line(s), expected 2 (two members, one won battle)"
  got=$(count_limits_queued "$WORK/server.log")
  value=$(count_lines_with "$WORK/server.log" "limit saved account=")
  [[ $value == "$got" ]] || problem "the server queued $got limit gauge(s) for the saver but logged $value 'limit saved' line(s)"
  got=$(count_queued "$WORK/server.log" "item used battle=")
  value=$(count_lines_with "$WORK/server.log" "item use saved account=")
  [[ $value == "$got" ]] || problem "the server queued $got item use(s) for the saver but logged $value 'item use saved' line(s)"
  if log_has "$WORK/server.log" "NOT SAVED" || log_has "$WORK/server.log" "NOT QUEUED"; then
    problem "the server lost a write: $(last_line_with "$WORK/server.log" "NOT SAVED" || last_line_with "$WORK/server.log" "NOT QUEUED")"
  fi
  after=$(ls -A -- "$state_real" | sort | tr '\n' ' ')
  [[ $before == "$after" ]] || problem "the library state directory changed: before [$before], after [$after]"

  if ((${#PROBLEMS[@]})); then
    finish_failed
  fi
  local n run
  for line in "${SMOKE_LINES[@]}"; do
    say "$line"
  done
  for run in cmds escape; do
    for n in "${CLIENT_NAMES[@]}"; do
      say "$run client $n: $(last_line_with "$WORK/client-$n.$run.log" "headless: battles:" || true)"
      say "$run client $n: $(last_line_with "$WORK/client-$n.$run.log" "headless: battle script:" || true)"
    done
  done
  for line in "${PROGRESS_SUMMARY[@]}"; do
    say "$line"
  done
  say "server: $started_1"
  say "server: $ended_1"
  say "server: $started_2"
  say "server: $ended_2"
  for needle in "magic used battle=" "defend used battle=" "item used battle=" "limit used battle=" "escape tried battle="; do
    say "server: $(count_lines_with "$WORK/server.log" "$needle") line(s) '$needle', e.g. $(last_line_with "$WORK/server.log" "$needle" || true)"
  done
  say "commands applied by the server for the bots, both runs: ${CMD_TOTAL[attack]} attack, ${CMD_TOTAL[magic]} magic, ${CMD_TOTAL[item]} item, ${CMD_TOTAL[defend]} defend, ${CMD_TOTAL[limit]} limit, ${CMD_TOTAL[escape]} escape"
  RESULT=pass
  say "PASS: every battle command was applied by the real server for the real-data bots (${CMD_TOTAL[magic]} magic, ${CMD_TOTAL[item]} item, ${CMD_TOTAL[defend]} defend, ${CMD_TOTAL[limit]} limit, ${CMD_TOTAL[escape]} escape; the server's own log lines match), the first battle was won and the second escaped from, and every battle was replayed by each bot from its own battle data with 0 divergences; $state_real unchanged ($after)"
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
    # The FIXTURE server has no characters and no items and reports no progress and no inventory:
    # level=0 exp=0 items=0 ("unknown"), last.
    [[ $smoke_line == *" battles=0 victories=0 area_changes=0 level=0 exp=0 items=0" ]] || problem "the SMOKE line does not end in battles=0 victories=0 area_changes=0 level=0 exp=0 items=0: $smoke_line"
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
      --progress) MODE=progress ;;
      --commands) MODE=commands ;;
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
        DURATION_SET=1
        shift
        ;;
      --formation)
        (($# >= 2)) || usage_error "--formation needs a formation id"
        FORMATION="$2"
        shift
        ;;
      --seed)
        (($# >= 2)) || usage_error "--seed needs a whole number"
        SEED="$2"
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
  [[ -z $SEED || $SEED =~ ^[0-9]+$ ]] || usage_error "--seed must be a whole number"
  [[ $BOT == idle || $BOT == circle ]] || usage_error "--bot must be idle or circle"
  if [[ $DO_BUILD == 1 && -n $BIN_DIR ]]; then
    usage_error "--build cannot be combined with --bin-dir"
  fi
  # The scripts of --commands go to `fmv-client --battle-script` as they are: words, ids, ':' and ','.
  local script
  for script in "$SCRIPT_A" "$SCRIPT_B" "$ESCAPE_SCRIPT"; do
    [[ $script =~ ^[A-Za-z0-9:,]+$ ]] || usage_error "a smoke script is made of command words, ids, ':' and ',' (see fmv-client --help), got '$script'"
  done

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
    progress) run_progress ;;
    commands) run_commands ;;
    *) run_real ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
