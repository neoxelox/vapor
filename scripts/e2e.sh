#!/usr/bin/env bash
# End-to-end verification harness (Tier E2E).
#
# Black-box exercise of the real `vapor` binary + in-process daemon
# against a disposable sandbox. Everything (runtime dir, watched local
# root, logs, state DB) lives under `<repo>/.vapor/e2e/<run-id>/`, so a
# run never touches `~/.vapor`, LaunchAgents, the network, or anything
# else on the host, and `./scripts/clean.sh` removes all residue.
#
# Full process doc: docs/development/e2e-verification.md
#
# Usage:
#   ./scripts/e2e.sh [--keep] [--skip-build]     run the scenario suite
#   ./scripts/e2e.sh --sandbox [--skip-build]    provision a manual sandbox
#     --keep        preserve the sandbox directory after a green run
#                   (failed runs always keep it for debugging)
#     --skip-build  reuse an existing target/debug/vapor binary
#     --sandbox     build + configure + start a daemon in a fresh
#                   sandbox, print a command cheat-sheet, and leave it
#                   running for manual/exploratory testing
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="suite"
KEEP_SANDBOX=0
SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP_SANDBOX=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --sandbox) MODE="sandbox" ;;
    *)
      echo "[e2e] unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

# Keep the run id short: the daemon's UDS socket lives at
# <sandbox>/home/vapord.sock and macOS caps socket paths at ~104 bytes
# (SUN_LEN). A long repo path + long run id silently costs the daemon
# its IPC endpoint.
TAG="run"
[[ "$MODE" == "sandbox" ]] && TAG="sbx"
RUN_ID="$TAG-$(date +%H%M%S)-$$"
E2E_ROOT="$ROOT_DIR/.vapor/e2e/$RUN_ID"
LOCAL_ROOT="$E2E_ROOT/local"
CLOUD_ROOT="$E2E_ROOT/cloud/VaporE2E"

# The sandbox home is the daemon's entire runtime universe.
export VAPOR_DIR="$E2E_ROOT/home"
export VAPOR_ENV="dev"
export VAPOR_LOG_LEVEL="debug"
# Never inherit sync-scope overrides from the invoking shell — config
# must flow through `vapor config set` so the e2e run covers the
# vapor.json loader path.
unset VAPOR_LOCAL_SYNC_DIRECTORY VAPOR_CLOUD_SYNC_DIRECTORY

VAPOR_BIN="$ROOT_DIR/target/debug/vapor"
STATE_DB="$VAPOR_DIR/state/vapor.sqlite"
DAEMON_LOG="$VAPOR_DIR/logs/vapord.logs"
DAEMON_OUT="$E2E_ROOT/daemon.out"
DAEMON_PID=""
FAILED=0

log() { echo "[e2e] $*"; }

dump_diagnostics() {
  echo "[e2e] ---- diagnostics ----"
  echo "[e2e] sandbox: $E2E_ROOT"
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    "$VAPOR_BIN" status --json 2>&1 | sed 's/^/[e2e] status: /' || true
  fi
  if [[ -f "$DAEMON_LOG" ]]; then
    echo "[e2e] last 40 daemon log lines:"
    tail -n 40 "$DAEMON_LOG" | sed 's/^/[e2e]   /'
  fi
  if [[ -s "$DAEMON_OUT" ]]; then
    echo "[e2e] daemon stdout/stderr tail:"
    tail -n 20 "$DAEMON_OUT" | sed 's/^/[e2e]   /'
  fi
  echo "[e2e] ---------------------"
}

fail() {
  FAILED=1
  log "FAIL — $*"
  dump_diagnostics
  exit 1
}

stop_daemon() {
  local pid="$1"
  if [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  kill -TERM "$pid" 2>/dev/null || true
  for _ in $(seq 1 40); do
    if ! kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  log "daemon did not exit within 10s of SIGTERM; sending SIGKILL"
  kill -KILL "$pid" 2>/dev/null || true
  return 1
}

cleanup() {
  stop_daemon "$DAEMON_PID" || true
  if [[ "$FAILED" -eq 1 || "$KEEP_SANDBOX" -eq 1 ]]; then
    log "sandbox preserved at $E2E_ROOT (remove with ./scripts/clean.sh)"
  else
    rm -rf "$E2E_ROOT"
  fi
}
trap cleanup EXIT

# wait_until <seconds> <description> <command...>
# Polls the command every 250ms with a hard deadline. This is bounded
# observation of an external process, not a timing assertion — the
# in-process no-sleep rule (testing-strategy) does not apply here.
wait_until() {
  local deadline_seconds="$1" description="$2"
  shift 2
  local attempts=$((deadline_seconds * 4))
  for _ in $(seq 1 "$attempts"); do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  log "timed out after ${deadline_seconds}s waiting for: $description"
  return 1
}

# --- sqlite helpers (read-only observers of the durable state DB) ---

db_scalar() {
  # Emits -1 on transient contention so poll loops just retry.
  sqlite3 -readonly "$STATE_DB" "$1" 2>/dev/null || echo "-1"
}

pending_intents() { db_scalar "SELECT COUNT(*) FROM queue_intents;"; }
failed_intents() { db_scalar "SELECT COUNT(*) FROM failed_intents;"; }
enqueue_high_water() {
  db_scalar "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='queue_intents'), 0);"
}

run_state_is() {
  "$VAPOR_BIN" status --json 2>/dev/null | grep -q "\"run_state\": \"$1\""
}

queue_drained() {
  [[ "$(pending_intents)" == "0" ]]
}

enqueues_reached() {
  local baseline="$1" delta="$2" current
  current="$(enqueue_high_water)"
  [[ "$current" -ge 0 && $((current - baseline)) -ge "$delta" ]]
}

start_daemon() {
  "$VAPOR_BIN" run --foreground >>"$DAEMON_OUT" 2>&1 &
  DAEMON_PID=$!
  wait_until 30 "daemon IPC endpoint to report run_state=Running" run_state_is "Running" \
    || fail "daemon did not reach Running"
}

converge() {
  # A drain hint is allowed to fail benignly if it races a state flip.
  "$VAPOR_BIN" flush-now >/dev/null 2>&1 || true
  wait_until "$1" "durable queue to drain to zero pending intents" queue_drained
}

# --- run ---

socket_path="$VAPOR_DIR/vapord.sock"
if [[ "${#socket_path}" -gt 100 ]]; then
  echo "[e2e] socket path would exceed the macOS SUN_LEN limit (~104 bytes):" >&2
  echo "[e2e]   $socket_path (${#socket_path} bytes)" >&2
  echo "[e2e] the daemon would start without its IPC endpoint. Move the" >&2
  echo "[e2e] repository to a shorter path and re-run." >&2
  exit 2
fi

log "sandbox: $E2E_ROOT"
mkdir -p "$VAPOR_DIR" "$E2E_ROOT/cloud"

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  log "building vapor + vapord (cargo build -p vapor-cli -p vapor-daemon)"
  cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-cli -p vapor-daemon
fi
[[ -x "$VAPOR_BIN" ]] || fail "vapor binary missing at $VAPOR_BIN (run without --skip-build)"
[[ -x "$ROOT_DIR/target/debug/vapord" ]] \
  || fail "vapord binary missing next to vapor (doctor's sibling probe needs it)"

# S1 — configuration reaches disk through the CLI, not env vars.
"$VAPOR_BIN" config set localSyncDirectory "$LOCAL_ROOT" >/dev/null
"$VAPOR_BIN" config set cloudSyncDirectory "$CLOUD_ROOT" >/dev/null
[[ "$("$VAPOR_BIN" config get localSyncDirectory)" == "$LOCAL_ROOT" ]] \
  || fail "S1: config get did not round-trip localSyncDirectory"
[[ -f "$VAPOR_DIR/vapor.json" ]] || fail "S1: vapor.json was not written under VAPOR_DIR"
log "PASS S1 — config set/get round-trips through vapor.json"

if [[ "$MODE" == "sandbox" ]]; then
  start_daemon
  # The daemon stays up after this script exits; disarm the cleanup trap.
  trap - EXIT
  cat <<EOF
[e2e] sandbox ready — daemon running (pid $DAEMON_PID)

  export VAPOR_DIR="$VAPOR_DIR"
  vapor=$VAPOR_BIN

  watched local root:  $LOCAL_ROOT
  daemon log:          $DAEMON_LOG
  state DB (read-only): sqlite3 -readonly "$STATE_DB" 'SELECT * FROM queue_intents;'

  \$vapor status --json          live daemon state
  \$vapor logs --tail 50         recent daemon log lines
  \$vapor pause / resume         flip work admission
  \$vapor flush-now              hint a queue drain
  \$vapor doctor                 sanity checks
  echo hi > "$LOCAL_ROOT/f.txt"  feed the watcher a change

  stop daemon:    kill -TERM $DAEMON_PID
  remove sandbox: rm -rf "$E2E_ROOT"   (or ./scripts/clean.sh for all of .vapor)
EOF
  exit 0
fi

# S2 — daemon starts from that config and creates the missing local root.
start_daemon
[[ -d "$LOCAL_ROOT" ]] || fail "S2: daemon did not create the missing local sync root"
[[ -S "$VAPOR_DIR/vapord.sock" ]] || fail "S2: IPC socket not present under VAPOR_DIR"
log "PASS S2 — daemon Running; local sync root auto-created; IPC socket up"

# S3 — local writes propagate: ingest -> debounce -> durable queue -> executor -> drained.
baseline="$(enqueue_high_water)"
[[ "$baseline" -ge 0 ]] || fail "S3: could not read durable state DB at $STATE_DB"
for i in 1 2 3; do
  echo "vapor e2e payload $i" >"$LOCAL_ROOT/e2e-file-$i.txt"
done
wait_until 30 "3 durable intents to be captured for the written files" \
  enqueues_reached "$baseline" 3 \
  || fail "S3: writes were not captured as durable intents"
converge 30 || fail "S3: durable queue did not drain"
[[ "$(failed_intents)" == "0" ]] || fail "S3: intents landed in failed_intents"
log "PASS S3 — 3 local writes captured as durable intents and drained"

# S4 — pause/resume flips run state over IPC and work drains after resume.
"$VAPOR_BIN" pause >/dev/null
wait_until 10 "run_state=Paused" run_state_is "Paused" || fail "S4: pause did not stick"
echo "written while paused" >"$LOCAL_ROOT/e2e-paused.txt"
"$VAPOR_BIN" resume >/dev/null
wait_until 10 "run_state=Running" run_state_is "Running" || fail "S4: resume did not stick"
converge 30 || fail "S4: backlog did not drain after resume"
log "PASS S4 — pause/resume round-trip; paused backlog drained on resume"

# S5 — singleton lock: a second daemon on the same VAPOR_DIR must refuse.
set +e
second_output="$("$VAPOR_BIN" run --foreground 2>&1)"
second_exit=$?
set -e
[[ "$second_exit" -ne 0 ]] || fail "S5: second daemon did not exit non-zero"
echo "$second_output" | grep -qi "already running" \
  || fail "S5: second daemon refusal message missing (got: $second_output)"
log "PASS S5 — second daemon on same VAPOR_DIR refused by singleton lock"

# S6 — doctor sanity checks pass inside the sandbox.
"$VAPOR_BIN" doctor >/dev/null || fail "S6: vapor doctor exited non-zero"
log "PASS S6 — vapor doctor healthy"

# S7 — restart recovery: clean SIGTERM shutdown, then a fresh daemon
# reuses the durable state and keeps syncing.
stop_daemon "$DAEMON_PID" || fail "S7: daemon did not shut down cleanly on SIGTERM"
DAEMON_PID=""
start_daemon
echo "post-restart write" >"$LOCAL_ROOT/e2e-after-restart.txt"
converge 30 || fail "S7: post-restart write did not converge"
[[ "$(failed_intents)" == "0" ]] || fail "S7: failed_intents non-empty after restart"
log "PASS S7 — clean shutdown, restart on same state DB, post-restart write converged"

# S8 — log hygiene: a healthy run must not emit ERROR-level lines.
if grep -q "\[ERROR\]" "$DAEMON_LOG"; then
  grep "\[ERROR\]" "$DAEMON_LOG" | tail -n 10 | sed 's/^/[e2e]   /'
  fail "S8: daemon log contains ERROR lines"
fi
warning_count="$(grep -c "\[WARNING\]" "$DAEMON_LOG" || true)"
log "PASS S8 — no ERROR lines in daemon log (${warning_count} warnings)"

log "OK — all e2e scenarios passed"
