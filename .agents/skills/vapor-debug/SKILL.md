---
name: vapor-debug
description: Diagnoses Vapor app and vapord daemon failures by correlating live CLI diagnostics, daemon logs, macOS crash reports, and the durable state DB against the source, then proposes a fix plan and waits for approval before changing code. Use when the daemon crashed, will not start, or keeps restarting; when sync is stuck or not converging; when `vapor status` or `vapor doctor` reports an unexpected state, reason, or throttle; when a `./scripts/e2e.sh` run left a failed sandbox to investigate; or when daemon log output needs a systematic review. macOS only.
license: GPL-3.0-only
---

## What I do

I help you debug issues with the **Vapor** macOS application and its **vapord** sync daemon. I will:

1. Collect and analyze daemon/app logs, live CLI diagnostics, durable state, and system crash reports
2. Cross-reference errors against the actual source code
3. Produce a clear diagnostic report with root cause analysis
4. Propose a concrete fix plan and wait for your go-ahead before touching any code

## When to use me

Use this skill when:

- Vapor or vapord has crashed and you want to understand why
- You're seeing unexpected behavior, errors, or sync failures
- You want a systematic review of recent log output
- You need help correlating a crash report back to your source code

## Where Vapor's state lives

Everything runtime lives under one directory root, `VAPOR_DIR`. Resolve it first — reading the wrong instance's logs wastes the whole session:

1. `VAPOR_DIR` environment variable, if set (explicit override)
2. `./.vapor` (repo-local) when running via repository scripts / `VAPOR_ENV=dev` — this is what dev, test, CI, and e2e workflows use
3. `~/.vapor` — the real user install (the project owner's machine)

E2E sandboxes live under `<repo>/.vapor/e2e/<run-id>/<scenario id>/`: `home/` is that scenario's `VAPOR_DIR`, `local/` and `cloud/Vapor/` are its two trees, `<label>-daemon.out` is the daemon's stdout/stderr, and a scenario with several daemons has `<label>-home/` siblings. A failed run keeps the sandbox, prints its path, and has already printed `vapor status --json`, `vapor diagnostics --json`, the queue rows, and the log tail for every daemon; start from that output, then open the directory. The run's `e2e-result.json` next to the scenario directories carries every verdict and note.

Inside `VAPOR_DIR`:

| Artifact | Path |
|----------|------|
| Config | `vapor.json` (a running daemon applies the live keys within seconds; roots, provider, profiles and sync mode need a restart and show up in `vapor status` as `config_restart_required`) |
| Daemon log | `logs/vapord.logs` (rotates at 8 MiB, three generations); `logs/vapord.stdout.log` / `.stderr.log` are the service manager's redirects; `logs/vapor-supervisor.log` is the headless supervisor's (`vapor service check --loop` under launchd, when installed with `--supervise`) |
| Durable queue/state DB | `state/vapor.sqlite` for the implicit `default` profile; `state/profiles/<id>/vapor.sqlite` per configured profile (tables: `queue_intents`, `failed_intents`, `pending_decisions`, `name_aliases`, `state_entries`, `sync_index`, `tombstones`, `schema_meta`) |
| Quarantined DB | `vapor.sqlite.corrupt-<ms>` next to the DB: the daemon moved a corrupt file aside and started fresh; a startup reconcile rebuilt the queue |
| Lifecycle state | `state/lifecycle.json` (crash-loop bookkeeping shared by the CLI and the app) |
| Trash | `trash/<profile>/<entry>/` (what Vapor removed on this device: the payload under its original name plus `meta.json`; `vapor trash list` reads it) |
| IPC socket (framed JSON over UDS — Vapor does not use XPC) | `vapord.sock`, relocated under the OS temp dir when the path exceeds ~104 bytes (`vapor doctor` reports where) |
| Singleton lock | `vapord.lock` |

## How I work

### Step 1 — Live diagnostics first (when a daemon is running)

The `vapor` CLI is the fastest signal — use it before reading raw files:

- `vapor status --json` — run state, throttle state + reason, provider, daemon id. "daemon not running" vs "daemon is not responding" are different failures (no socket vs wedged process).
- `vapor doctor` (add `--json` for scripts) — sanity probes: `vapor_dir` writable/private, `ipc_socket_path` budget and relocation, `vapord_binary` discoverable (sibling, bundle, PATH), `secret_store` persistence, `throttle_inputs` source, and `host_launch_agent_plist` (host state, not the sandbox).
- `vapor diagnostics --json` — every queued or in-flight intent with its stage, attempt count, last error, and blocker reason ("why is this stuck"), in lease order. Stage `Held` with blocker "waiting for decision #N" is not stuck: the daemon is asking.
- `vapor decisions list` (`show <id>`, `resolve <id> --choose <key>`) — the questions the daemon parked: a deletion burst held by the mass-deletion guard (`mass-deletion`), a sync root that is gone (`root-missing`) or swapped for a folder the profile never adopted (`root-replaced`; the hidden `.vapor-root` marker in each root is the identity), a name that is a file on one side and a folder on the other (`type-mismatch`). `vapor status` counts them as `decisions_pending`; they work with the daemon stopped, and the daemon applies the answer on its next tick or start. A held batch is the product working as designed; the finding, if any, is why the evidence was ambiguous.
- `vapor support-bundle` — one redacted archive with status, diagnostics, timeline, config, and log tail; the first thing to ask a user for.
- `vapor logs --tail 100` — recent daemon log lines, already redacted.
- `vapor timeline --json` — diagnostics activity timeline (real events; an empty list means nothing has been recorded yet, not that the feature is missing).
- `launchctl list | grep sh.arn.vapor` and `ps aux | grep vapord` — is the service loaded / process alive? (The LaunchAgent label is `sh.arn.vapor.daemon`; a headless install also has `sh.arn.vapor.supervisor`, and `vapor service status` says whether it is installed.)

### Step 2 — Gather logs and crash reports

- **Daemon log:** `$VAPOR_DIR/logs/vapord.logs`. Line format is `unix_millis [LEVEL] (component): message. key=value key=value`, levels `DEBUG`/`INFO`/`WARNING`/`ERROR`. Grep `\[ERROR\]` and `\[WARNING\]` first, then read the surrounding context.
- **Crash reports:** `~/Library/Logs/DiagnosticReports/Vapor*.{crash,ips}` and `vapord*.{crash,ips}` — both process names matter. `.ips` files are JSON (parse exception type, termination reason, faulting thread); `.crash` files are plain text.
- **Durable state:** query read-only, never mutate: `sqlite3 -readonly "$VAPOR_DIR/state/vapor.sqlite" 'SELECT path_text, kind, failure_kind, last_error FROM failed_intents;'` — permanently failed intents carry their final error. `queue_intents` shows what's stuck pending/leased, and the rows in state `held` name the decision they wait on (`decision_id`); `pending_decisions` holds the question, its options, and its JSON evidence.

**Time-sensitive:** log timestamps are Unix **milliseconds**. Get the current time in both forms — `date +"%Y-%m-%d %H:%M:%S"` and `date +%s000` — and focus on a ±15 minute window around the incident, widening to ±1 hour, then ±4 hours only if needed. Prioritize crash reports by modification time.

### Step 3 — Review the source code

Once you have the error sites (symbols, component names, file paths) from logs and crash reports:

- Read the corresponding sources: the Rust runtime lives in `core/daemon` (tick loop, scheduler, throttle, executor, state DB), `core/ipc` (framed-JSON UDS server/client), `core/lifecycle` (crash-loop guard, daemon lifecycle), `core/shared` (config, runtime paths, logging), `core/platform` (fs-watch, secrets, metrics); the macOS app is `apps/macos`.
- Trace the call path that led to the failure; classify transient vs permanent per the retry taxonomy.
- Daemon-exit context: a profile is suspended after 5 consecutive tick failures or a panic; the daemon exits only when every profile failed at runtime. A daemon whose every profile is misconfigured (bad provider, overlapping roots, missing Google Drive client id) stays up serving status with the per-profile `suspended_reason`, and logs "serving status only". A second daemon on the same `VAPOR_DIR` exits with "daemon already running" (singleton lock).
- Throttle context: on macOS the inputs are real (CPU, power, thermal, memory, keyboard presence); "user activity is active" means someone typed within 30 s. `VAPOR_THROTTLE_INPUTS=static` pins neutral inputs (the e2e harness sets it); `vapor doctor`'s `throttle_inputs` row says which is in force.
- Repeated-crash context: the crash-loop guard schedule is crash 1 → restart immediately, crash 2 → 2s, crash 3 → 4s, crash 4 → 8s, paused on the 5th. "Daemon won't come back" may be the guard doing its job.
- Path-length context: macOS caps UDS paths at ~104 bytes. With a deep `VAPOR_DIR`, daemon and CLI relocate the socket to a deterministic `vapor-<hash>` directory under the OS temp dir (INFO log line "relocated under the OS temp directory"; `vapor doctor`'s `ipc_socket_path` probe reports it). If `vapor status` cannot reach a running daemon, check that both processes resolve the same `VAPOR_DIR` — the socket location is derived from it.

### Step 4 — Reproduce safely (never against `~/.vapor`)

To reproduce a bug hands-on, use the sandboxed manual environment instead of the real runtime dir:

```
./scripts/e2e.sh --sandbox
```

It provisions a disposable `VAPOR_DIR` under `<repo>/.vapor/e2e/sbx-<id>/`, starts a daemon, and prints a command cheat-sheet (see the `vapor-e2e` skill); `./scripts/e2e.sh --sandbox-stop` removes it. To reproduce a scripted scenario's failure by hand, run it alone with `./scripts/e2e.sh --only Sxx --keep` and work inside the preserved sandbox. Do not guess either — if you need missing context (repro steps, the user action right before the crash, recent code changes, whether vapord is running), ask. A precise diagnosis beats a fast wrong one.

### Step 5 — Write a diagnostic report

Present your findings in this format:

```
## Diagnostic Report

### Summary
One or two sentence description of what went wrong.

### Error Source
- **Component:** Vapor app | vapord daemon | both
- **File(s):** list the relevant source files
- **Log evidence:** quote the key log lines, crash report fields, or state-DB rows

### Root Cause Analysis
Explain why the crash or error happened. Be specific — reference
exact code paths, line numbers, and the conditions that triggered it.

### Affected Functionality
Describe what user-facing behavior is broken or degraded.

### Fix Plan
Numbered steps describing exactly what code changes you will make
and why each change addresses the root cause.

### Risks & Side Effects
Note anything the fix might affect, any edge cases to watch for,
and which e2e scenario (`tools/e2e/src/scenarios/`, run with
`./scripts/e2e.sh --only Sxx`) should cover the regression: an
existing one, or a new one written per the `vapor-e2e` skill.
```

### Step 6 — Wait for confirmation

After presenting the report, **stop and wait** for the user to:

- Approve the fix plan
- Ask questions or request changes to the plan
- Provide additional context that changes the diagnosis

**Do not start writing code until the user explicitly confirms.**

## Important notes

- Always check both the app and daemon sides — issues in one often manifest as errors in the other.
- Logs are already redacted (tokens, auth headers, sensitive keys become `[REDACTED]`); if you see a secret in a log line, that itself is a bug worth reporting.
- If logs are very large, summarize overall health first (error/warning counts, restart markers like "vapord started"), then zero in.
- Never mutate `state/vapor.sqlite`; always open it with `-readonly`. The durable queue is the product's source of truth for intent state. The one sanctioned write from outside the daemon is `vapor decisions resolve`, which records an answer the daemon then applies.
