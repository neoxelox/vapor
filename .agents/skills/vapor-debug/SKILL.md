---
name: vapor-debug
description: Debug Vapor app and vapord daemon by analyzing logs, crash reports, live CLI diagnostics, and durable state to diagnose errors and propose fixes
license: MIT
compatibility: opencode
metadata:
  audience: developers
  platform: macos
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

E2E sandboxes live under `<repo>/.vapor/e2e/<run-id>/home`. When debugging a failed `./scripts/e2e.sh` run, that preserved sandbox is your `VAPOR_DIR`.

Inside `VAPOR_DIR`:

| Artifact | Path |
|----------|------|
| Config | `vapor.json` |
| Daemon log | `logs/vapord.logs` |
| Durable queue/state DB | `state/vapor.sqlite` (tables: `queue_intents`, `failed_intents`, `state_entries`, `schema_meta`) |
| IPC socket (framed JSON over UDS — Vapor does not use XPC) | `vapord.sock` |
| Singleton lock | `vapord.lock` |

## How I work

### Step 1 — Live diagnostics first (when a daemon is running)

The `vapor` CLI is the fastest signal — use it before reading raw files:

- `vapor status --json` — run state, throttle state + reason, provider, daemon id. "daemon not running" vs "daemon is not responding" are different failures (no socket vs wedged process).
- `vapor doctor` — sanity probes (vapor_dir writable/private, `vapord` binary discoverable, LaunchAgent plist present).
- `vapor logs --tail 100` — recent daemon log lines, already redacted.
- `vapor timeline --json` — diagnostics timeline (empty until C8-30 lands; don't be surprised).
- `launchctl list | grep sh.arn.vapor` and `ps aux | grep vapord` — is the service loaded / process alive? (The LaunchAgent label is `sh.arn.vapor.daemon`.)

### Step 2 — Gather logs and crash reports

- **Daemon log:** `$VAPOR_DIR/logs/vapord.logs`. Line format is `unix_millis [LEVEL] (component): message. key=value key=value`, levels `DEBUG`/`INFO`/`WARNING`/`ERROR`. Grep `\[ERROR\]` and `\[WARNING\]` first, then read the surrounding context.
- **Crash reports:** `~/Library/Logs/DiagnosticReports/Vapor*.{crash,ips}` and `vapord*.{crash,ips}` — both process names matter. `.ips` files are JSON (parse exception type, termination reason, faulting thread); `.crash` files are plain text.
- **Durable state:** query read-only, never mutate: `sqlite3 -readonly "$VAPOR_DIR/state/vapor.sqlite" 'SELECT path_text, kind, failure_kind, last_error FROM failed_intents;'` — permanently failed intents carry their final error. `queue_intents` shows what's stuck pending/leased.

**Time-sensitive:** log timestamps are Unix **milliseconds**. Get the current time in both forms — `date +"%Y-%m-%d %H:%M:%S"` and `date +%s000` — and focus on a ±15 minute window around the incident, widening to ±1 hour, then ±4 hours only if needed. Prioritize crash reports by modification time.

### Step 3 — Review the source code

Once you have the error sites (symbols, component names, file paths) from logs and crash reports:

- Read the corresponding sources: the Rust runtime lives in `core/daemon` (tick loop, scheduler, throttle, executor, state DB), `core/ipc` (framed-JSON UDS server/client), `core/lifecycle` (crash-loop guard, daemon lifecycle), `core/shared` (config, runtime paths, logging), `core/platform` (fs-watch, secrets, metrics); the macOS app is `apps/macos`.
- Trace the call path that led to the failure; classify transient vs permanent per the retry taxonomy.
- Daemon-exit context: the tick loop tolerates up to 5 consecutive tick failures before exiting; a second daemon on the same `VAPOR_DIR` exits with "daemon already running" (singleton lock).
- Repeated-crash context: the crash-loop guard schedule is crash 1 → restart immediately, crash 2 → 2s, crash 3 → 4s, crash 4 → 8s, paused on the 5th. "Daemon won't come back" may be the guard doing its job.
- Known trap: macOS caps UDS paths at ~104 bytes — with a deep `VAPOR_DIR`, the daemon logs a WARNING and runs *without* its IPC endpoint, so `vapor status` reports it unreachable while it is actually syncing.

### Step 4 — Reproduce safely (never against `~/.vapor`)

To reproduce a bug hands-on, use the sandboxed manual environment instead of the real runtime dir:

```
./scripts/e2e.sh --sandbox
```

It provisions a disposable `VAPOR_DIR` under `<repo>/.vapor/e2e/`, starts a daemon, and prints a command cheat-sheet (see the `vapor-e2e` skill). Do not guess either — if you need missing context (repro steps, the user action right before the crash, recent code changes, whether vapord is running), ask. A precise diagnosis beats a fast wrong one.

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
and which e2e scenario (scripts/e2e.sh) should cover the regression.
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
- Never mutate `state/vapor.sqlite`; always open it with `-readonly`. The durable queue is the product's source of truth for intent state.
