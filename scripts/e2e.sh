#!/usr/bin/env bash
# End-to-end verification harness (Tier E2E).
#
# Black-box exercise of the real `vapor` binary + in-process daemon
# against a disposable sandbox. Everything (runtime dir, watched local
# root, logs, state DB) lives under `<repo>/.vapor/e2e/<run-id>/`, so a
# default run never touches `~/.vapor`, LaunchAgents, the network, or
# anything else on the host, and `./scripts/clean.sh` removes all
# residue.
#
# `--full` additionally runs the service lifecycle round-trip
# against the REAL macOS service
# manager: install → start → status → crash-loop supervision
# (`vapor service check`) through backoff and pause → acknowledge →
# stop → uninstall. That phase is the one part of Tier E2E that
# mutates host state (a LaunchAgent plist + launchd registration for
# `sh.arn.vapor.daemon`), which is why it is opt-in and runs on
# disposable CI runners (`AGENTS.md §9.7`); it refuses outright when a
# `sh.arn.vapor.daemon` LaunchAgent already exists so it can never
# clobber a real Vapor install, and it removes the LaunchAgent on exit.
#
# Full process doc: docs/development/e2e-verification.md
#
# Usage:
#   ./scripts/e2e.sh [--keep] [--skip-build] [--full]  run the scenario suite
#   ./scripts/e2e.sh --sandbox [--skip-build]          provision a manual sandbox
#     --keep        preserve the sandbox directory after a green run
#                   (failed runs always keep it for debugging)
#     --skip-build  reuse an existing target/debug/vapor binary
#     --full        also run the host-mutating service round-trip
#                   (installs a real LaunchAgent; macOS only; intended
#                   for disposable CI runners)
#     --sandbox     build + configure + start a daemon in a fresh
#                   sandbox, print a command cheat-sheet, and leave it
#                   running for manual/exploratory testing
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="suite"
KEEP_SANDBOX=0
SKIP_BUILD=0
FULL=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP_SANDBOX=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --full) FULL=1 ;;
    --sandbox) MODE="sandbox" ;;
    *)
      echo "[e2e] unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

if [[ "$MODE" == "sandbox" && "$FULL" -eq 1 ]]; then
  echo "[e2e] --full and --sandbox are mutually exclusive" >&2
  exit 2
fi

# Keep the run id short so the default sandbox stays under the Unix
# socket-address budget and S2 can assert the *canonical* socket
# placement; the over-budget relocation path gets its own coverage (S9).
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
DEEP_PID=""
PULL_PID=""
FAILED=0

# --full service round-trip phase (host-mutating; see header). The
# phase gets its own runtime home so the R scenarios never share state
# with the in-process S scenarios.
DAEMON_LABEL="sh.arn.vapor.daemon"
PLIST_PATH="$HOME/Library/LaunchAgents/$DAEMON_LABEL.plist"
DOMAIN_TARGET="gui/$(id -u)"
SERVICE_HOME="$E2E_ROOT/service-home"
SERVICE_PHASE_STARTED=0
SERVICE_LAST_OUTPUT=""

log() { echo "[e2e] $*"; }

dump_diagnostics() {
  echo "[e2e] ---- diagnostics ----"
  echo "[e2e] sandbox: $E2E_ROOT"
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    "$VAPOR_BIN" status --json 2>&1 | sed 's/^/[e2e] status: /' || true
    # Per-intent "why stuck" rows — names the exact stage and
    # blocker for anything wedged in the pipeline.
    "$VAPOR_BIN" diagnostics --json 2>&1 | sed 's/^/[e2e] diag: /' || true
  fi
  if [[ -f "$STATE_DB" ]]; then
    echo "[e2e] durable queue rows:"
    sqlite3 -readonly "$STATE_DB" "SELECT * FROM queue_intents;" 2>/dev/null \
      | sed 's/^/[e2e]   /' || true
  fi
  if [[ -f "$DAEMON_LOG" ]]; then
    echo "[e2e] last 40 daemon log lines:"
    tail -n 40 "$DAEMON_LOG" | sed 's/^/[e2e]   /'
  fi
  if [[ -s "$DAEMON_OUT" ]]; then
    echo "[e2e] daemon stdout/stderr tail:"
    tail -n 20 "$DAEMON_OUT" | sed 's/^/[e2e]   /'
  fi
  if [[ "$SERVICE_PHASE_STARTED" -eq 1 ]]; then
    echo "[e2e] last service command output: $SERVICE_LAST_OUTPUT"
    launchctl print "$DOMAIN_TARGET/$DAEMON_LABEL" 2>&1 | head -n 25 | sed 's/^/[e2e]   /' || true
    if [[ -f "$SERVICE_HOME/state/lifecycle.json" ]]; then
      echo "[e2e] lifecycle.json:"
      sed 's/^/[e2e]   /' "$SERVICE_HOME/state/lifecycle.json"
    fi
    if [[ -f "$SERVICE_HOME/logs/vapord.logs" ]]; then
      echo "[e2e] last 20 service-daemon log lines:"
      tail -n 20 "$SERVICE_HOME/logs/vapord.logs" | sed 's/^/[e2e]   /'
    fi
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
  stop_daemon "$DEEP_PID" || true
  stop_daemon "$PULL_PID" || true
  if [[ "$SERVICE_PHASE_STARTED" -eq 1 ]]; then
    # Best-effort teardown so the host is left clean even on failure:
    # unregister the service, remove the plist, kill any straggler
    # daemon launched from this repo's target directory. Default runs
    # (no --full) never reach this branch and never touch launchd.
    VAPOR_DIR="$SERVICE_HOME" "$VAPOR_BIN" service uninstall >/dev/null 2>&1 || true
    launchctl bootout "$DOMAIN_TARGET" "$PLIST_PATH" >/dev/null 2>&1 || true
    rm -f "$PLIST_PATH" 2>/dev/null || true
    # Match by executable path, not `pkill -f "<repo path>"`: the repo path
    # is passed to pkill as a regex, so metacharacters in a checkout path
    # (e.g. `budapest[wip]`) would either mis-match or fail to compile and
    # leave a straggler daemon running against a deleted sandbox.
    for straggler_pid in $(pgrep -x vapord 2>/dev/null || true); do
      straggler_exe="$(ps -p "$straggler_pid" -o comm= 2>/dev/null || true)"
      if [[ "$straggler_exe" == "$ROOT_DIR/target/debug/vapord" ]]; then
        kill "$straggler_pid" 2>/dev/null || true
      fi
    done
  fi
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
  local out
  out="$("$VAPOR_BIN" status --json 2>/dev/null)" || return 1
  grep -q "\"run_state\": \"$1\"" <<<"$out"
}

queue_drained() {
  [[ "$(pending_intents)" == "0" ]]
}

enqueues_reached() {
  local baseline="$1" delta="$2" current
  current="$(enqueue_high_water)"
  [[ "$current" -ge 0 && $((current - baseline)) -ge "$delta" ]]
}

file_exists() { [[ -f "$1" ]]; }
file_absent() { [[ ! -e "$1" ]]; }

# True when a keep-both conflict copy for stem $2 exists under root $1.
conflict_copy_exists() {
  compgen -G "$1/$2~conflict-*" >/dev/null
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

log "sandbox: $E2E_ROOT"
mkdir -p "$VAPOR_DIR" "$E2E_ROOT/cloud"

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  log "building vapor + vapord (cargo build -p vapor-cli -p vapor-daemon)"
  cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-cli -p vapor-daemon
fi
[[ -x "$VAPOR_BIN" ]] || fail "vapor binary missing at $VAPOR_BIN (run without --skip-build)"
[[ -x "$ROOT_DIR/target/debug/vapord" ]] \
  || fail "vapord binary missing next to vapor (doctor's sibling probe needs it)"

# --full preflight: fail fast (before the sandbox scenarios) when the
# host-mutating service phase cannot run safely.
if [[ "$FULL" -eq 1 ]]; then
  if [[ "$(uname -s)" != "Darwin" ]]; then
    log "NOTE — --full service round-trip is macOS-only; it will be skipped"
    FULL=0
  elif [[ -f "$PLIST_PATH" ]]; then
    fail "--full refused: $PLIST_PATH already exists (a real Vapor install?) — the round-trip would uninstall it; remove the LaunchAgent manually first"
  elif ! launchctl print "$DOMAIN_TARGET" >/dev/null 2>&1; then
    fail "--full refused: launchctl domain $DOMAIN_TARGET is unavailable in this session"
  fi
fi

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
# Canonical socket placement is only asserted when the sandbox path fits
# the socket-address budget; deeper checkouts legitimately relocate (S9).
if [[ "${#socket_path}" -le 100 ]]; then
  [[ -S "$socket_path" ]] || fail "S2: IPC socket not present under VAPOR_DIR"
fi
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
# Bound the wait: if the lock regresses the second daemon proceeds into the
# runtime loop and never returns, so a plain command substitution would
# hang the suite forever (the only unbounded wait it had). Background it and
# poll for exit instead.
s5_out="$E2E_ROOT/s5-second-daemon.out"
: >"$s5_out"
"$VAPOR_BIN" run --foreground >"$s5_out" 2>&1 &
s5_pid=$!
second_exit=""
for _ in $(seq 1 60); do
  if ! kill -0 "$s5_pid" 2>/dev/null; then
    second_exit=0
    wait "$s5_pid" || second_exit=$?
    break
  fi
  sleep 0.25
done
if [[ -z "$second_exit" ]]; then
  kill "$s5_pid" 2>/dev/null || true
  wait "$s5_pid" 2>/dev/null || true
  fail "S5: second daemon did not exit within 15s (singleton-lock regression?)"
fi
second_output="$(cat "$s5_out")"
[[ "$second_exit" -ne 0 ]] || fail "S5: second daemon did not exit non-zero"
grep -qi "already running" <<<"$second_output" \
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

# S9 — over-budget VAPOR_DIR: the IPC socket relocates deterministically
# under the OS temp dir, the CLI still reaches the daemon, and doctor
# explains the relocation instead of leaving it silent.
DEEP_HOME="$E2E_ROOT/deep-$(printf 'x%.0s' {1..70})/home"
mkdir -p "$DEEP_HOME"
VAPOR_DIR="$DEEP_HOME" "$VAPOR_BIN" config set localSyncDirectory "$E2E_ROOT/deep-local" >/dev/null
VAPOR_DIR="$DEEP_HOME" "$VAPOR_BIN" config set cloudSyncDirectory "$E2E_ROOT/cloud/DeepE2E" >/dev/null
VAPOR_DIR="$DEEP_HOME" "$VAPOR_BIN" run --foreground >>"$E2E_ROOT/deep-daemon.out" 2>&1 &
DEEP_PID=$!
deep_running() {
  VAPOR_DIR="$DEEP_HOME" "$VAPOR_BIN" status --json 2>/dev/null \
    | grep -q '"run_state": "Running"'
}
wait_until 30 "deep-VAPOR_DIR daemon to be reachable over the relocated socket" deep_running \
  || fail "S9: daemon with over-budget socket path is not reachable via vapor status"
[[ ! -S "$DEEP_HOME/vapord.sock" ]] \
  || fail "S9: socket bound at the canonical over-budget path instead of relocating"
deep_doctor_out="$(VAPOR_DIR="$DEEP_HOME" "$VAPOR_BIN" doctor)" \
  && grep -q "rendezvous" <<<"$deep_doctor_out" \
  || fail "S9: vapor doctor does not explain the socket relocation"
deep_socket="$(grep "relocated under the OS temp directory" "$DEEP_HOME/logs/vapord.logs" \
  | tail -n 1 | sed 's/.*socket_path=\([^ ]*\).*/\1/' || true)"
[[ -n "$deep_socket" ]] || fail "S9: daemon log does not record the relocation"
stop_daemon "$DEEP_PID" || fail "S9: deep-VAPOR_DIR daemon did not stop cleanly"
DEEP_PID=""
# Validate before deleting: the sed capture uses `[^ ]*`, so a
# space-containing TMPDIR could truncate the path and turn this into an
# rm -rf of a real parent directory. Only rm -rf a positively-recognized
# `vapor-*` relocation dir; otherwise remove just the socket file.
socket_dir="$(dirname "$deep_socket")"
if [[ "$deep_socket" == */vapord.sock && "$(basename "$socket_dir")" == vapor-* ]]; then
  rm -rf "$socket_dir"
else
  rm -f "$deep_socket"
  log "S9: relocated dir shape unexpected ($socket_dir); removed only the socket file"
fi
log "PASS S9 — over-budget socket path relocated; CLI + doctor work; temp residue removed"

# S10 — bidirectional filesystem sync: local writes land in the
# cloud root byte-for-byte, and cloud-born content flows back down.
[[ -f "$CLOUD_ROOT/e2e-file-1.txt" ]] \
  || fail "S10: uploaded file missing in cloud root"
cmp -s "$LOCAL_ROOT/e2e-file-1.txt" "$CLOUD_ROOT/e2e-file-1.txt" \
  || fail "S10: cloud copy diverges from the local original"
echo "born in the cloud" >"$CLOUD_ROOT/e2e-from-cloud.txt"
# External cloud writes bypass the provider's changes feed, so a
# reconcile is the designed discovery path for them.
"$VAPOR_BIN" reconcile >/dev/null
wait_until 30 "cloud-born file to download into the local root" \
  file_exists "$LOCAL_ROOT/e2e-from-cloud.txt" \
  || fail "S10: cloud-born file did not download"
cmp -s "$CLOUD_ROOT/e2e-from-cloud.txt" "$LOCAL_ROOT/e2e-from-cloud.txt" \
  || fail "S10: downloaded content diverges from the cloud original"
# POSIX mode carries over in both directions: an executable script must
# stay executable on the other replica (0755 must not decay to 0644).
printf '#!/bin/sh\necho ok\n' >"$LOCAL_ROOT/e2e-script.sh"
chmod 755 "$LOCAL_ROOT/e2e-script.sh"
wait_until 30 "executable script to upload" file_exists "$CLOUD_ROOT/e2e-script.sh" \
  || fail "S10: executable script did not upload"
uploaded_mode() { [[ "$(stat -f '%Lp' "$CLOUD_ROOT/e2e-script.sh" 2>/dev/null || stat -c '%a' "$CLOUD_ROOT/e2e-script.sh")" == "755" ]]; }
wait_until 30 "uploaded script to carry mode 755" uploaded_mode \
  || fail "S10: uploaded script lost its executable mode"
converge 30 || fail "S10: queue did not drain after bidirectional round-trip"
log "PASS S10 — local→cloud upload and cloud→local download round-trip byte-for-byte (modes preserved)"

# S11 — keep-both conflict: the same path diverges on both
# sides while the daemon is down; the restart reconcile must preserve
# BOTH payloads (one canonical, one ~conflict copy) — never overwrite.
echo "conflict v1" >"$LOCAL_ROOT/e2e-conflict.txt"
converge 30 || fail "S11: seed file did not sync"
stop_daemon "$DAEMON_PID" || fail "S11: daemon did not stop for the divergence window"
DAEMON_PID=""
echo "edited locally while down" >"$LOCAL_ROOT/e2e-conflict.txt"
echo "edited in cloud while down" >"$CLOUD_ROOT/e2e-conflict.txt"
start_daemon
"$VAPOR_BIN" reconcile >/dev/null
conflict_somewhere() {
  conflict_copy_exists "$LOCAL_ROOT" "e2e-conflict" \
    || conflict_copy_exists "$CLOUD_ROOT" "e2e-conflict"
}
wait_until 30 "a keep-both conflict copy to appear" conflict_somewhere \
  || fail "S11: no ~conflict copy appeared for the diverged path"
converge 30 || fail "S11: queue did not drain after conflict resolution"
grep -rq "edited locally while down" "$LOCAL_ROOT" "$CLOUD_ROOT" \
  || fail "S11: the local edit was lost"
grep -rq "edited in cloud while down" "$LOCAL_ROOT" "$CLOUD_ROOT" \
  || fail "S11: the cloud edit was lost"
log "PASS S11 — diverged edits kept both payloads via a ~conflict copy; nothing lost"

# S12 — pull-only strict mirror: its own runtime
# home; cloud is authoritative — cloud content materializes locally and
# a local-only file is removed, never uploaded.
PULL_HOME="$E2E_ROOT/pull-home"
PULL_LOCAL="$E2E_ROOT/pull-local"
PULL_CLOUD="$E2E_ROOT/cloud/PullE2E"
mkdir -p "$PULL_HOME" "$PULL_CLOUD"
echo "cloud canonical" >"$PULL_CLOUD/doc.txt"
VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" config set localSyncDirectory "$PULL_LOCAL" >/dev/null
VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" config set cloudSyncDirectory "$PULL_CLOUD" >/dev/null
VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" config set syncMode pull-only >/dev/null
VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" run --foreground >>"$E2E_ROOT/pull-daemon.out" 2>&1 &
PULL_PID=$!
pull_running() {
  VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" status --json 2>/dev/null \
    | grep -q '"run_state": "Running"'
}
wait_until 30 "pull-only daemon to be reachable" pull_running \
  || fail "S12: pull-only daemon did not reach Running"
wait_until 30 "cloud canonical to materialize locally" \
  file_exists "$PULL_LOCAL/doc.txt" \
  || fail "S12: cloud content did not mirror down (startup reconcile)"
echo "local intruder" >"$PULL_LOCAL/extra.txt"
VAPOR_DIR="$PULL_HOME" "$VAPOR_BIN" reconcile >/dev/null
wait_until 30 "local-only file to be mirror-removed" \
  file_absent "$PULL_LOCAL/extra.txt" \
  || fail "S12: local-only file survived in pull-only mode"
[[ ! -e "$PULL_CLOUD/extra.txt" ]] \
  || fail "S12: pull-only mode uploaded a local file"
stop_daemon "$PULL_PID" || fail "S12: pull-only daemon did not stop cleanly"
PULL_PID=""
log "PASS S12 — pull-only mirror: cloud materialized locally; local-only file removed, never uploaded"

# S13 — CLI observability: per-intent diagnostics answer over
# IPC and the support bundle exports with live captures.
diagnostics_out="$("$VAPOR_BIN" diagnostics --json)" \
  && grep -q '"schema_version"' <<<"$diagnostics_out" \
  || fail "S13: vapor diagnostics --json did not answer"
SUPPORT_OUT="$E2E_ROOT/support"
support_out="$("$VAPOR_BIN" support-bundle --output "$SUPPORT_OUT" --json)" \
  && grep -q '"daemonReachable": true' <<<"$support_out" \
  || fail "S13: support bundle did not capture the live daemon"
compgen -G "$SUPPORT_OUT/vapor-support-*/manifest.json" >/dev/null \
  || fail "S13: support bundle manifest missing"
compgen -G "$SUPPORT_OUT/vapor-support-*/status.json" >/dev/null \
  || fail "S13: support bundle live status capture missing"
log "PASS S13 — diagnostics respond; support bundle exported with live captures"

# S14 — symmetric ignore filtering: ignored names (Finder
# metadata, temp files) never sync in either direction — not through
# the changes feed, not through reconcile — and divergence between the
# two sides never manufactures a ~conflict copy.
echo "local finder state" >"$LOCAL_ROOT/.DS_Store"
echo "divergent cloud finder state" >"$CLOUD_ROOT/.DS_Store"
echo "cloud temp residue" >"$CLOUD_ROOT/e2e-residue.tmp"
echo "s14 control" >"$LOCAL_ROOT/e2e-s14-control.txt"
"$VAPOR_BIN" reconcile >/dev/null
wait_until 30 "control file to upload around the ignored names" \
  file_exists "$CLOUD_ROOT/e2e-s14-control.txt" \
  || fail "S14: control file did not sync"
converge 30 || fail "S14: queue did not drain after the ignore-filter reconcile"
grep -q "local finder state" "$LOCAL_ROOT/.DS_Store" \
  || fail "S14: local .DS_Store was overwritten from the cloud side"
grep -q "divergent cloud finder state" "$CLOUD_ROOT/.DS_Store" \
  || fail "S14: cloud .DS_Store was overwritten from the local side"
[[ ! -e "$LOCAL_ROOT/e2e-residue.tmp" ]] \
  || fail "S14: an ignored cloud-side name downloaded into the local root"
if conflict_copy_exists "$LOCAL_ROOT" ".DS_Store" \
  || conflict_copy_exists "$CLOUD_ROOT" ".DS_Store"; then
  fail "S14: ignored divergence manufactured a ~conflict copy"
fi
log "PASS S14 — ignore rules hold in both directions; no conflict copies for ignored names"

# S15 — conflict surfacing: `vapor conflicts list` finds the keep-both
# copy S11 left behind (the files are the durable registry — no
# timeline cap applies), `resolve --keep copy` promotes the preserved
# version, and the resolution syncs like any other edit.
conflicts_out="$("$VAPOR_BIN" conflicts list --json)" \
  && grep -q 'e2e-conflict~conflict-' <<<"$conflicts_out" \
  || fail "S15: conflicts list did not find the S11 conflict copy"
grep -q '"deviceId"' <<<"$conflicts_out" \
  || fail "S15: conflict record is missing the origin device id"
# S11 can preserve a divergent copy per side; promote the first and
# discard any others so the scope ends conflict-free.
# `|| true` so an empty match does not abort the script under
# `set -euo pipefail` before the guarded `[[ -n ]]` check can fail loudly
# with diagnostics (and preserve the sandbox).
S15_COPY="$(compgen -G "$LOCAL_ROOT/e2e-conflict~conflict-*" | head -n 1 || true)"
[[ -n "$S15_COPY" ]] || fail "S15: local conflict copy missing"
S15_KEPT_PAYLOAD="$(cat "$S15_COPY")"
"$VAPOR_BIN" conflicts resolve "$S15_COPY" --keep copy >/dev/null \
  || fail "S15: conflicts resolve exited non-zero"
[[ "$(cat "$LOCAL_ROOT/e2e-conflict.txt")" == "$S15_KEPT_PAYLOAD" ]] \
  || fail "S15: the kept copy's payload did not become the canonical content"
[[ ! -e "$S15_COPY" ]] || fail "S15: resolved conflict copy still exists locally"
for leftover in "$LOCAL_ROOT"/e2e-conflict~conflict-*; do
  [[ -e "$leftover" ]] || continue
  "$VAPOR_BIN" conflicts resolve "$leftover" --keep canonical >/dev/null \
    || fail "S15: resolving a leftover copy with --keep canonical failed"
done
converge 30 || fail "S15: queue did not drain after conflict resolution"
no_cloud_conflict_copy() { ! conflict_copy_exists "$CLOUD_ROOT" "e2e-conflict"; }
wait_until 30 "resolved conflict copy to disappear from the cloud root" \
  no_cloud_conflict_copy \
  || fail "S15: resolution did not propagate the copy's deletion to the cloud"
conflicts_after_out="$("$VAPOR_BIN" conflicts list --json)" \
  && grep -q '"conflicts": \[\]' <<<"$conflicts_after_out" \
  || fail "S15: conflicts list is not empty after resolution"
log "PASS S15 — conflicts listed from durable file state; resolve promoted the copy and synced"

# S16 — local deletion propagates through the real watcher. Real
# fs-watch backends split one unlink into several fragments; the
# classification must come from ground truth, not fragment order
# (deletes used to become uploads that no-op'd as "vanished", leaving
# the remote copy immortal).
[[ -f "$CLOUD_ROOT/e2e-file-2.txt" ]] || fail "S16: expected S3 file in the cloud root"
rm "$LOCAL_ROOT/e2e-file-2.txt"
wait_until 30 "local deletion to remove the cloud copy" \
  file_absent "$CLOUD_ROOT/e2e-file-2.txt" \
  || fail "S16: local deletion never propagated to the cloud"
converge 30 || fail "S16: queue did not drain after the deletion"
log "PASS S16 — a plain local delete removes the cloud copy"

# S17 — special files are inert: a FIFO in the watched root never
# becomes a remote object and never wedges the queue (hashing a FIFO
# would block forever; before the guard the intent sat permanently in
# WaitingForHash).
mkfifo "$LOCAL_ROOT/e2e-pipe.fifo"
echo "s17 control" >"$LOCAL_ROOT/e2e-s17-control.txt"
wait_until 30 "control file to sync around the FIFO" \
  file_exists "$CLOUD_ROOT/e2e-s17-control.txt" \
  || fail "S17: control file did not sync"
converge 30 || fail "S17: queue did not drain with a FIFO in the watched root"
[[ ! -e "$CLOUD_ROOT/e2e-pipe.fifo" ]] \
  || fail "S17: a special file produced a remote object"
log "PASS S17 — special files are ignored; queue drains with a FIFO present"

# S18 — pipeline friendliness: a downstream reader that closes the pipe
# early (head, grep -q) must not make the CLI panic. The CLI restores
# default SIGPIPE handling, so it dies silently like standard Unix
# tools instead of printing a stdout panic.
sigpipe_err="$( { "$VAPOR_BIN" logs 2>&1 | head -n 1 >/dev/null; } 2>&1 || true )"
[[ "$sigpipe_err" != *panicked* ]] \
  || fail "S18: vapor logs | head -1 panicked on SIGPIPE: $sigpipe_err"
log "PASS S18 — early-closed pipe does not panic the CLI"

# S19 — feed-driven cloud deletion of an UPLOADED file. Regression
# guard: the upload's no-clobber commit once used hard-link + unlink,
# which detached FSEvents file tracking from the destination — a later
# cloud-side rm of the uploaded file produced zero watcher events and
# the deletion never propagated (field-testing find). This scenario
# must converge through the live changes feed alone: no reconcile.
echo "uploaded then deleted in the cloud" >"$LOCAL_ROOT/e2e-feed-delete.txt"
wait_until 30 "file to upload" file_exists "$CLOUD_ROOT/e2e-feed-delete.txt" \
  || fail "S19: seed file did not upload"
converge 30 || fail "S19: queue did not drain after the seed upload"
rm "$CLOUD_ROOT/e2e-feed-delete.txt"
# A loaded CI host throttles the daemon to a 60s poll cadence. Nudge an
# immediate poll on every probe (the same nudge a user gets from
# `vapor flush-now`): the deletion still travels through the live feed,
# the nudges only defeat the throttled cadence and FSEvents latency.
feed_delete_propagated() {
  "$VAPOR_BIN" flush-now >/dev/null 2>&1
  [[ ! -f "$LOCAL_ROOT/e2e-feed-delete.txt" ]]
}
wait_until 45 "cloud deletion to propagate through the changes feed" feed_delete_propagated \
  || fail "S19: cloud deletion of an uploaded file never propagated locally (feed lost the event)"
log "PASS S19 — cloud deletion of an uploaded file propagates via the live feed (no reconcile)"

# --- service lifecycle round-trip (--full only) ---
#
# Everything below drives `vapor service` against the REAL macOS
# service manager: install → start → status → crash-loop supervision
# (`vapor service check`) through backoff and pause → acknowledge →
# stop → uninstall. It is the only Tier E2E phase that mutates host
# state (a LaunchAgent plist + launchd registration), which is why it
# is opt-in and intended for disposable CI runners. Runtime state
# (config, logs, durable DBs, lifecycle state) stays in the sandbox via
# VAPOR_DIR; cleanup removes the LaunchAgent and any straggler daemon.

if [[ "$FULL" -ne 1 ]]; then
  log "OK — all e2e scenarios passed (service round-trip skipped; opt in with --full)"
  exit 0
fi

# The in-process S-phase daemon is done; stop it so the launchd-managed
# daemon is the only vapord running from this repo.
stop_daemon "$DAEMON_PID" || fail "could not stop the S-phase daemon before the service phase"
DAEMON_PID=""

SERVICE_PHASE_STARTED=1
export VAPOR_DIR="$SERVICE_HOME"
mkdir -p "$VAPOR_DIR"
"$VAPOR_BIN" config set localSyncDirectory "$E2E_ROOT/service-local" >/dev/null
"$VAPOR_BIN" config set cloudSyncDirectory "$E2E_ROOT/cloud/VaporServiceRT" >/dev/null

# svc <subcommand...> — runs the CLI, captures output for assertions
# and diagnostics. Service subcommands exit 0 even for deferred/paused
# outcomes; a non-zero exit is always a failure worth diagnostics.
svc() {
  if ! SERVICE_LAST_OUTPUT="$("$VAPOR_BIN" service "$@" 2>&1)"; then
    fail "vapor service $* exited non-zero: $SERVICE_LAST_OUTPUT"
  fi
}

expect_last() {
  local needle="$1" description="$2"
  if ! grep -qF "$needle" <<<"$SERVICE_LAST_OUTPUT"; then
    fail "$description — expected '$needle' in: $SERVICE_LAST_OUTPUT"
  fi
}

service_status_is() {
  local out
  out="$("$VAPOR_BIN" service status --json 2>/dev/null)" || return 1
  grep -q "\"status\": \"$1\"" <<<"$out"
}

daemon_pid_from_launchd() {
  launchctl print "$DOMAIN_TARGET/$DAEMON_LABEL" 2>/dev/null \
    | awk '/pid = /{print $3; exit}'
}

kill_service_daemon() {
  local pid
  pid="$(daemon_pid_from_launchd)"
  [[ -n "$pid" ]] || fail "cannot simulate a crash — daemon pid not found via launchctl"
  kill -KILL "$pid" 2>/dev/null || fail "could not SIGKILL daemon pid $pid"
  wait_until 15 "launchd to observe the daemon exit" service_status_is "stopped" \
    || fail "launchd did not observe the daemon exit"
}

# R1 — fresh runner reports not_installed.
svc status --json
expect_last '"status": "not_installed"' "R1: fresh status"
log "PASS R1 — status reports not_installed before install"

# R2 — install: plist on disk, daemon running, IPC reachable.
svc install --json
expect_last '"result": "started"' "R2: install result"
[[ -f "$PLIST_PATH" ]] || fail "R2: LaunchAgent plist not written at $PLIST_PATH"
wait_until 30 "service status to report running" service_status_is "running" \
  || fail "R2: daemon did not reach running after install"
# The plist embeds VAPOR_DIR, so the daemon must come up inside the
# sandbox — reaching it over the sandbox IPC socket proves the env
# wiring end to end.
wait_until 30 "daemon IPC endpoint inside the sandbox" run_state_is "Running" \
  || fail "R2: daemon not reachable over the sandbox IPC socket"
svc check --json
expect_last '"health": "running"' "R2: healthy check"
log "PASS R2 — install: plist written, daemon Running, sandbox IPC reachable"

# R3 — crash 1: check restarts immediately (NoDelay).
kill_service_daemon
svc check --json
expect_last '"health": "restarted_after_crash"' "R3: first crash restarts immediately"
wait_until 30 "daemon running again after crash-1 restart" service_status_is "running" \
  || fail "R3: daemon not running after crash-1 restart"
log "PASS R3 — unexpected exit detected; immediate restart (crash 1)"

# R4 — crash 2: backoff defers, same exit not double-counted, then restart.
kill_service_daemon
svc check --json
expect_last '"health": "restart_deferred"' "R4: second crash defers restart"
svc status --json
expect_last '"consecutive_crashes": 2' "R4: durable crash count"
svc check --json
expect_last '"health": "restart_deferred"' "R4: repeat check while deferred"
svc status --json
expect_last '"consecutive_crashes": 2' "R4: repeat check must not double-count"
sleep 2.5 # default policy: crash 2 backs off 2 s (bounded external wait)
svc check --json
expect_last '"health": "restarted_after_crash"' "R4: restart after backoff elapsed"
wait_until 30 "daemon running after crash-2 restart" service_status_is "running" \
  || fail "R4: daemon not running after crash-2 restart"
log "PASS R4 — backoff deferral honored; no double-count; restarted after 2s"

# R5 — crashes 3+4 walk the backoff schedule (4 s, 8 s).
kill_service_daemon
svc check --json
expect_last '"health": "restart_deferred"' "R5: third crash defers"
sleep 4.5
svc check --json
expect_last '"health": "restarted_after_crash"' "R5: restart after 4s backoff"
wait_until 30 "daemon running after crash-3 restart" service_status_is "running" \
  || fail "R5: daemon not running after crash-3 restart"
kill_service_daemon
svc check --json
expect_last '"health": "restart_deferred"' "R5: fourth crash defers"
sleep 8.5
svc check --json
expect_last '"health": "restarted_after_crash"' "R5: restart after 8s backoff"
wait_until 30 "daemon running after crash-4 restart" service_status_is "running" \
  || fail "R5: daemon not running after crash-4 restart"
log "PASS R5 — exponential backoff schedule (4s, 8s) walked end to end"

# R6 — crash 5: durable crash-loop pause; start refuses.
kill_service_daemon
svc check --json
expect_last '"health": "crash_loop_paused"' "R6: fifth crash pauses"
svc status --json
expect_last '"status": "crash_loop_paused"' "R6: status overlays the pause"
expect_last '"paused": true' "R6: crash_loop.paused"
svc start --json
expect_last '"result": "crash_loop_paused"' "R6: start refused while paused"
# The pause must be durable state, not process memory: every CLI
# invocation above was a separate process.
grep -q '"paused_indefinitely": true' "$VAPOR_DIR/state/lifecycle.json" \
  || fail "R6: pause not persisted in lifecycle.json"
log "PASS R6 — crash-loop pause engaged, durable, and refusing restarts"

# R7 — acknowledge, then start works again.
svc acknowledge --json
expect_last '"result": "acknowledged"' "R7: acknowledge"
svc start --json
expect_last '"result": "started"' "R7: start after acknowledge"
wait_until 30 "daemon running after acknowledge + start" service_status_is "running" \
  || fail "R7: daemon not running after acknowledge + start"
log "PASS R7 — acknowledge cleared the pause; daemon started"

# R8 — an expected stop is not a crash.
svc stop --json
expect_last '"result": "stopped"' "R8: stop"
wait_until 15 "service status to report stopped" service_status_is "stopped" \
  || fail "R8: daemon did not stop"
svc check --json
expect_last '"health": "stopped_expected"' "R8: expected stop is not a crash"
log "PASS R8 — clean stop; check does not treat it as a crash"

# R9 — uninstall removes the plist; status returns to not_installed.
svc uninstall --json
[[ ! -f "$PLIST_PATH" ]] || fail "R9: plist still present after uninstall"
svc status --json
expect_last '"status": "not_installed"' "R9: status after uninstall"
svc check --json
expect_last '"health": "not_installed"' "R9: check after uninstall"
log "PASS R9 — uninstall removed the LaunchAgent; status/check report not_installed"

log "OK — all e2e scenarios passed, including the --full service round-trip"
