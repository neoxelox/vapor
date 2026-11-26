---
name: vapor-debug
description: Diagnoses Vapor app and vapord daemon failures by correlating live CLI diagnostics, daemon logs, macOS crash reports, and the durable state DB against the source, states the root cause, then proposes a fix plan and waits for approval before changing code. Use when the daemon crashed, will not start, or keeps restarting; when sync is stuck or not converging; when `vapor status` or `vapor doctor` reports an unexpected state, reason, or throttle; when a `./scripts/e2e.sh` run left a failed sandbox to investigate; when a bug, a regression, or a failing test has no obvious cause; or when daemon log output needs a systematic review. The crash-report and launchd steps are macOS only.
license: GPL-3.0-only
---

# Debug Vapor

Do not start with a guessed fix. Collect the evidence (live CLI
diagnostics, logs, crash reports, the durable state), read it against
the source, state the root cause, then propose a fix plan and wait for
the owner's go-ahead before changing code.

A bug with no runtime to inspect (a failing test, a regression found in
review) starts at step 3.

## Where Vapor's state lives

Everything runtime lives under one directory root, `VAPOR_DIR`. Resolve it first; reading the wrong instance's logs wastes the whole session:

1. `VAPOR_DIR` environment variable, if set (explicit override)
2. `./.vapor` (repo-local) when running via repository scripts or `VAPOR_ENV=dev`; this is what dev, test, CI, and e2e workflows use
3. `~/.vapor`, the real user install (the project owner's machine)

E2E sandboxes live under `<repo>/.vapor/e2e/<run-id>/<scenario id>/`: `home/` is that scenario's `VAPOR_DIR`, `local/` and `cloud/Vapor/` are its two trees, `<label>-daemon.out` is the daemon's stdout/stderr, and a scenario with several daemons has `<label>-home/` siblings. A failed run keeps the sandbox, prints its path, and has already printed `vapor status --json`, `vapor diagnostics --json`, the queue rows, and the log tail for every daemon; start from that output, then open the directory. The run's `e2e-result.json` next to the scenario directories carries every verdict and note.

Inside `VAPOR_DIR`:

| Artifact | Path |
|----------|------|
| Config | `vapor.json` (a running daemon applies the live keys within seconds, and starts or stops a profile that was added, enabled, disabled, or removed; the roots, the provider, and the sync mode of a profile that runs need a restart and show up in `vapor status` as `restartRequired`) |
| Daemon log | `logs/vapord.log` (rotates at 8 MiB, three generations); `logs/vapord.stdout.log` / `.stderr.log` are the service manager's redirects; `logs/vapor-supervisor.log` is the supervisor's (`vapor service check --loop` under the service manager, which every install registers; it is what restarts a crashed daemon) |
| Durable queue/state DB | `state/vapor.sqlite` for the implicit `default` profile; `state/profiles/<id>/vapor.sqlite` per configured profile (tables: `queue_intents`, `failed_intents`, `pending_decisions`, `name_aliases`, `state_entries`, `sync_index`, `sync_folders`, `name_carries`, `schema_meta`; schema 10. `sync_index.remote_id` and `sync_folders` hold the provider's id of each synced file and folder, which is what pairs a rename in the cloud; `name_carries` holds each move that the cloud is still owed) |
| DB that was set aside | `vapor.sqlite.corrupt-<ms>` (a damaged file) or `vapor.sqlite.schema-v<n>-<ms>` (another schema version) next to the DB: the daemon moved it aside and started fresh; a startup reconcile rebuilt the index. Removed after 30 days |
| Lifecycle state | `state/lifecycle.json` (crash-loop bookkeeping shared by the CLI and the app) |
| Trash | `trash/<profile>/<entry>/` (what Vapor removed on this device: the payload under its original name plus `meta.json`; `vapor trash list` reads it) |
| IPC socket (framed JSON over UDS; Vapor does not use XPC) | `vapord.sock`, relocated under the OS temp dir when the path exceeds ~104 bytes (`vapor doctor` reports where) |
| Singleton lock | `vapord.lock` |

## 1. Live diagnostics first (when a daemon is running)

The `vapor` CLI is the fastest signal. Use it before reading raw files:

- `vapor status --json`: run state, throttle state + reason, provider, daemon id. "daemon not running" vs "daemon is not responding" are different failures (no socket vs wedged process).
- `vapor doctor` (add `--json` for scripts): sanity probes. `vapor-dir` writable/private, `ipc-socket` budget and relocation, `vapord-binary` discoverable (sibling, bundle, PATH), `secret-store` persistence, `throttle-inputs` source, `gdrive-oauth-client`, and `launch-agent` (host state, not the sandbox).
- `vapor diagnostics --json`: every queued or in-flight item with its `action`, `stage`, `attempts`, last `error`, and the `reason` it waits ("why is this stuck"), in lease order, plus `droppedEvents` and `suppressedEchoes`.
- `vapor decisions list` (`show <id>`, `resolve <id> --choose <key>`): the questions the daemon parked. A file with keep-both conflict copies (`conflict`, one option per version keyed by its file name, applied by the command itself; the versions not kept go to the trash), a sync root that is gone (`root-missing`) or swapped for a folder the profile never adopted (`root-replaced`; the hidden `.vapor-root` marker in each root is the identity), a name that is a file on one side and a folder on the other (`type-mismatch`), a local file whose name cannot be a sync path (not valid UTF-8, or with a backslash in it) and so never syncs (`unsyncable-name`, answer `skip` or rename the file), a `pull-only` or `push-only` profile that waits for its data-loss confirmation and syncs nothing until then (`one-way-mode`, answer `enable`). `vapor status` counts them as `decisions`; they work with the daemon stopped, and the daemon applies the answer on its next tick or start. An open decision is the product working as designed; the finding, if any, is why the evidence was ambiguous.
- `vapor report`: one redacted archive with status, diagnostics, timeline, config, and log tail; the first thing to ask a user for.
- `vapor logs --tail 100`: recent daemon log lines, already redacted.
- `vapor timeline --json`: diagnostics activity timeline (real events; an empty list means nothing has been recorded yet, not that the feature is missing).
- `launchctl list | grep sh.arn.vapor` and `ps aux | grep vapord`: is the service loaded, is the process alive? (The LaunchAgent label is `sh.arn.vapor.daemon`; every install also has `sh.arn.vapor.supervisor`, the job that restarts a crashed daemon, and `vapor service status` says whether it is installed.)

## 2. Gather logs and crash reports

- **Daemon log.** `$VAPOR_DIR/logs/vapord.log`. Line format is `unix_millis [LEVEL] (component): message. key=value key=value`, levels `DEBUG`/`INFO`/`WARNING`/`ERROR`. Grep `\[ERROR\]` and `\[WARNING\]` first, then read the surrounding context.
- **Crash reports.** `~/Library/Logs/DiagnosticReports/Vapor*.{crash,ips}` and `vapord*.{crash,ips}`. Both process names matter. `.ips` files are JSON (parse exception type, termination reason, faulting thread); `.crash` files are plain text.
- **Durable state.** Query read-only, never mutate: `sqlite3 -readonly "$VAPOR_DIR/state/vapor.sqlite" 'SELECT path_text, kind, failure_kind, last_error FROM failed_intents;'`. Permanently failed intents carry their final error. `queue_intents` shows what's stuck pending/leased; `pending_decisions` holds the question, its options, and its JSON evidence. `state_entries` rows under `provider.follow_up.` are checks for a replaced version that a Google Drive profile still owes; a row that stays means the check cannot run yet (no sign-in, no route to the cloud).
- **Time.** Log timestamps are Unix milliseconds. Get the current time in both forms, `date +"%Y-%m-%d %H:%M:%S"` and `date +%s000`, and focus on a ±15 minute window around the incident, widening to ±1 hour, then ±4 hours only if needed. Prioritize crash reports by modification time.
- **Volume.** If logs are very large, summarize overall health first (error/warning counts, restart markers like "vapord started"), then zero in.

Logs are already redacted (tokens, auth headers, sensitive keys become `[REDACTED]`). A secret in a log line is itself a bug worth reporting. Real logs, reports, and state from `~/.vapor` stay out of the repository (`AGENTS.md` §6.1); quote a redacted excerpt.

## 3. Review the source code

Once you have the error sites (symbols, component names, file paths) from logs and crash reports:

- Read the corresponding sources: the Rust runtime lives in `core/daemon` (tick loop, scheduler, throttle, executor, state DB), `core/ipc` (framed-JSON UDS server/client), `core/lifecycle` (crash-loop guard, daemon lifecycle), `core/shared` (config, runtime paths, logging), `core/platform` (fs-watch, secrets, metrics); the macOS app is `apps/macos`.
- Trace the call path that led to the failure; classify transient vs permanent per the retry taxonomy.
- Check both the app and daemon sides. A fault in one often shows up as an error in the other.
- Daemon-exit context: a profile is suspended after 5 consecutive tick failures or a panic, and composed again after a wait (1 minute, then 5, then 30; a profile that fails again soon after starts from the wait it had reached). A suspended profile never takes the daemon down, and neither do all of them. A daemon whose every profile is misconfigured (bad provider, overlapping roots, missing Google Drive client id) stays up serving status with each profile marked `suspended: true` and the cause in its `run.reason`, and logs "serving status only". A second daemon on the same `VAPOR_DIR` exits with "daemon already running" (singleton lock).
- Sign-in context: a profile in `error` with the reason "Vapor needs to sign in to <provider> again" and `signInRequired: true` in `vapor status --json` is the sign-in hold, not a failure. The provider refused the token (expired, revoked, none stored); nothing is leased, the queue is intact, and `vapor auth login --profile <id>` ends it within one 15 s root check. A Google Drive client in Testing status drops refresh tokens every 7 days. A profile suspended with "no Google Drive OAuth client" is a build without the client; `vapor doctor`'s `gdrive-oauth-client` row says which client a binary carries. Details: `docs/operations/provider-auth-operations.md`.
- Throttle context: on macOS the inputs are real (CPU, power, thermal, memory, keyboard presence); "user activity is active" means someone typed within 30 s. `VAPOR_THROTTLE_INPUTS=static` pins neutral inputs (the e2e harness sets it); `vapor doctor`'s `throttle-inputs` row says which is in force.
- Repeated-crash context: the crash-loop guard schedule is crash 1 → restart immediately, crash 2 → 2s, crash 3 → 4s, crash 4 → 8s, paused on the 5th. "Daemon won't come back" may be the guard doing its job.
- Path-length context: macOS caps UDS paths at ~104 bytes. With a deep `VAPOR_DIR`, daemon and CLI relocate the socket to a deterministic `vapor-<hash>` directory under the OS temp dir (INFO log line "relocated under the OS temp directory"; `vapor doctor`'s `ipc-socket` probe reports it). If `vapor status` cannot reach a running daemon, check that both processes resolve the same `VAPOR_DIR`; the socket location is derived from it.

## 4. Reproduce safely (never against `~/.vapor`)

To reproduce a bug hands-on, use the sandboxed manual environment instead of the real runtime dir:

```
./scripts/e2e.sh --sandbox
```

It provisions a disposable `VAPOR_DIR` under `<repo>/.vapor/e2e/sbx-<id>/`, starts a daemon, and prints a command cheat-sheet (see the `vapor-e2e` skill); `./scripts/e2e.sh --sandbox-stop` removes it. To reproduce a scripted scenario's failure by hand, run it alone with `./scripts/e2e.sh --only Sxx --keep` and work inside the preserved sandbox.

Do not guess here either. If you need missing context (repro steps, the user action right before the crash, recent code changes, whether vapord is running), ask. A precise diagnosis beats a fast wrong one.

## 5. State the cause before the fix

Check the neighbours of the failing case: a retry, a duplicate event,
a crash between two steps, a stale index row, a throttle transition in
the middle of the work, the other sync direction, a second profile.

Before proposing a change, be able to state:

- the observable problem and the conditions that cause it;
- the root cause and the assumption that failed;
- the smallest safe fix and the crate that owns it;
- the test or e2e scenario that proves the fix and catches a
  regression.

If the evidence is incomplete, state the hypothesis and name the
evidence that is missing. Never present a hypothesis as a fact.

## 6. Write a diagnostic report

Present the findings in this format, in Simplified Technical English
(`AGENTS.md` §8.11):

```
## Diagnostic report

### Summary
One or two sentences: what went wrong.

### Error source
- Component: Vapor app | vapord daemon | both
- Files: the relevant source files
- Evidence: the key log lines, crash report fields, or state-DB rows

### Root cause
Why the crash or error happened. Name the exact code paths, the line
numbers, and the conditions that triggered it. Mark anything that is
still a hypothesis.

### Affected functionality
What user-facing behavior is broken or degraded.

### Fix plan
Numbered steps: each code change, and why it addresses the root cause.

### Risks and side effects
What the fix might affect, the edge cases to watch, and which e2e
scenario (`tools/e2e/src/scenarios/`, run with
`./scripts/e2e.sh --only Sxx`) covers the regression: an existing
one, or a new one written per the `vapor-e2e` skill.
```

## 7. Wait for confirmation

When the owner asked for a diagnosis, stop after the report. Wait for
the owner to approve the fix plan, ask questions, request changes, or
add context that changes the diagnosis. Do not write code until the
owner confirms.

When the failure belongs to work the owner already asked for (a red
check or a review finding on a PR, a failing test in the change being
built), that request covers the fix. State the cause as in step 5, fix
it, and put the cause in the report.

## Never

- Never mutate `state/vapor.sqlite`; always open it with `-readonly`. The durable queue is the product's source of truth for intent state. The one sanctioned write from outside the daemon is `vapor decisions resolve`, which records an answer the daemon then applies.
- Never reproduce against `~/.vapor`.
