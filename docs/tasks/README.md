# Tasks

Execution checklists mapped to the plans in `docs/plans/`. This directory
serves two purposes:

1. **Per-surface task lists** — one file per deliverable surface, each
   tracking concrete items with `[ ] / [~] / [x]` status.
2. **Cross-surface roadmap** — this README is the orchestrator. It tells
   you **what should be done next**, which tasks block which, and how
   the waves line up.

If you only have time to read one doc before picking up work, read this
one.

## File convention

Tasks are flat and platform-named: one file per deliverable surface.

- `core.md` — portable Rust runtime tasks. The biggest file; everything
  that powers every app surface lives here.
- `macos.md` — macOS app surface tasks.
- `cli.md` — `vapor` CLI tasks.
- `windows.md` — placeholder (only created if `apps/windows` is started).
- `linux.md` — placeholder (only created if `apps/linux` is started).

Every file mirrors its plan in `docs/plans/<surface>.md`. Plans are
*intent*; these files track *execution*.

Status legend (used in every task file):

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

## Scope and prioritization

**The focus is a very good, performant, and polished core runtime, CLI,
and macOS app.** Everything else is deferred or optional.

Concretely:

- **Primary path** (waves 0–11): the core runtime (`core/*`), the
  `vapor` CLI running on macOS, and the macOS app — shipped, polished,
  and maintained.
- **Foundation kept open** inside the primary path: engine portability
  fixes so `core/*` compiles on every OS, and the `core/platform` trait
  layer with macOS-native implementations (Windows/Linux impls stubbed
  to `unimplemented!()`). These stay in the primary path because they
  are good engineering regardless — they remove OS-only assumptions
  from the engine and keep the door open — not because Windows/Linux is
  a committed deliverable.
- **Deferred / optional** (waves 12+): Windows and Linux platform-trait
  implementations, `apps/windows`, `apps/linux`, and full cross-OS CLI
  distribution. None of this is required for the primary deliverable.
  Work in this bucket only starts if and when the project owner
  explicitly decides to ship a non-macOS surface.

## Primary path — cross-surface roadmap (waves 0–11)

A wave is a coherent set of tasks across surfaces that must land
together before the next wave starts. Within a wave, tasks can be
worked in parallel unless a dependency is called out.

### Wave 0 — Finish the macOS MVP invariants

Status: complete. Wave 2 + Wave 3 may begin in parallel.

- `macos.md` M1-6 — LaunchAgent plist + crash-loop validation scenarios
  (plist audit / SIGKILL / crash-loop pause / clean shutdown). Done.

### Wave 1 — Documentation and naming hygiene

Status: complete.

- `core.md` C0-1 … C0-10 — plans/tasks reorg, docs group READMEs,
  `xpc-contracts` → `ipc-contracts` rename, platform-abstractions
  reference, `AGENTS.md` reframing, constants / FSEvents vocabulary
  cleanup. Done.

Prerequisite for every later wave because it lays out the target
directory structure and removes macOS-only vocabulary from the shared
docs.

### Wave 2 — Engine portability fixes

Status: complete. C1-1 … C1-7 all done — C1-5 closed alongside
Wave 4's `ProcessSupervisor` (C3-7). **Foundation** — kept in the
primary path even though macOS-only shipping would technically not
need it, because it removes Unix-only assumptions and keeps the
engine clean.

- `core.md` C1-1 … C1-7 — `runtime_paths` permission gates,
  `HOME`/`USERPROFILE` resolution, UTF-8 path encoding in `state_db`,
  Windows-prefix handling in `fs_events`, `libc` dep cfg-gate, Linux +
  Windows Rust CI matrix for `core/*` (lint + test only; Swift stays
  macOS-only).

Exit gate: `cargo build --workspace` + `cargo test` succeed on macOS,
Linux, Windows CI jobs for `core/*`. Blocks Wave 4 (the trait layer
needs a cross-OS compilable workspace).

### Wave 3 — Remaining runtime gaps

Status: complete. Platform-agnostic runtime fixes; ran in parallel with
Wave 2.

- `core.md` C2-1 … C2-4 — replace default-`ThrottleInputs` placeholder
  with a real input source, throttle permit-id wrap-around hardening,
  `SystemTime` → `Instant` migration for tick-cadence clocks, throttle
  controller hysteresis / min-dwell. Done.

### Wave 4 — Platform abstraction layer

Status: complete. **Foundation** — kept in the primary path. Required
Wave 2.

- `core.md` C3-1 … C3-10 — new `core/platform` crate with trait
  skeletons + macOS-native implementations ported from existing
  Swift/docs (`FsWatcher`, `ServiceInstaller`, `SecretStore`,
  `PlatformMetricsSampler`, `IdleNotifier`, `FilesystemCapabilities`,
  `ProcessSupervisor`). Windows/Linux impls stubbed to
  `unimplemented!()`. Publish `docs/architecture/platform-abstractions.md`
  as the living reference.

macOS daemon behavior must stay byte-for-byte identical before and
after this wave. If Windows/Linux native impls are never written, the
stubs stay as-is — the primary deliverable is unaffected.

### Wave 5 — Daemon lifecycle moves into Rust

Status: complete. Rust port (C4-1 … C4-4, C4-6) shipped first; the
Swift consumer side (C4-5 subprocess bridge, C4-7 duplicate-logic
removal, `macos.md` M2-1 … M2-6) landed together in the macOS
app-shim follow-up, which also bundled the `vapor` CLI into
`Vapor.app` (`Contents/Helpers/vapor`). Required Wave 4
(`ServiceInstaller`).

- `core.md` C4-1 … C4-7 — new `core/lifecycle` crate; port
  `CrashLoopGuard` and `DaemonLifecycleManager` from Swift; expose a
  stable surface (C-ABI or CLI subprocess) the macOS app can consume.
- `macos.md` M2-1 … M2-4 — the macOS app starts delegating lifecycle
  to `core/lifecycle` via the `vapor` CLI. End-to-end regression test
  that the macOS UX is unchanged.

These two task groups land **together** — do not merge one without the
other.

### Wave 6 — IPC channel + `vapor` CLI lifecycle commands (macOS)

Status: complete. CLI surface (L0/L1/L2) and IPC channel (C5-1 …
C5-5) shipped end-to-end; the macOS Swift app shim (`macos.md` M2-1 …
M2-6 / `core.md` C4-5 + C4-7) and the macOS-CI service round-trip
(`cli.md` L2-5, the `./scripts/e2e.sh --full` phase) landed in the
follow-up PR. Crash-loop state is now durable
(`<vapor_dir>/state/lifecycle.json`) and `vapor service` gained
`bootstrap` / `check` / `acknowledge` + `--json` for the app shim.
Required Waves 4 and 5.

- `core.md` C5-1 … C5-5 — transport decision (UDS on Unix, named pipe
  on Windows — the Windows transport choice is made now even though
  its implementation waits for Wave 12), length-prefixed JSON framing,
  server-side implementation, client library, status/control
  endpoints, skew-matrix tests.
- `cli.md` L0-1 … L0-5 — `vapor` crate skeleton and `--version`.
- `cli.md` L1-1 … L1-4 — `vapor run`, `vapor config`, `vapor version`,
  `vapor doctor` (macOS flavor).
- `cli.md` L2-1 … L2-5 — `vapor service {install,uninstall,start,stop,status}`
  driving the macOS `ServiceInstaller`; round-trip automated on macOS
  CI only.

This is the "ship the CLI on macOS" milestone. The CLI proves
`core/platform` + `core/lifecycle` work against real macOS users.

### Wave 7 — IPC-driven CLI surface + auth flows

Status: complete (against the filesystem-provider no-op auth path; the
OAuth-PKCE browser flow lands with Wave 8 / C8-48). Required Wave 6
(IPC).

- `cli.md` L3-1 … L3-7 — `vapor status / pause / resume / flush-now /
  reconcile / timeline / logs`. All IPC-backed. `--json` stable; never
  hang when no daemon is running.
- `cli.md` L4-1 … L4-3 — `vapor auth login / logout / status` via PKCE.
  Depends on `SecretStore` from Wave 4 and a provider with an OAuth
  flow.

### Wave 8 — Runtime capability completion

Status: **complete** (landed 2026-07 in a single change set on the
portable runtime; see `core.md` Phase C8 for per-task notes). The
runtime now runs real bidirectional sync end to end on the filesystem
reference provider and Google Drive, with sync modes, profiles,
resource budgets, diagnostics, and safeguards. Tier-2 perf fixtures
(10k-file / adapter-overhead microbenches) are the only carve-out,
tracked under `core.md` "Deferred tasks".

Covered the full C8-1 … C8-58 span in `core.md`:

1. **C8-1 … C8-13** — filesystem reference provider + bidirectional
   runtime shell + `self_write_cache` + simulator removal. Blocks
   every later sub-wave.
2. **C8-14 … C8-18** — conflict policy, `deviceId`, tombstones, race
   resolution, corruption recovery.
3. **C8-19 … C8-26** — multi-profile model, profile-scoped overrides,
   shared-root watch dedup, blast-radius containment.
4. **C8-27 … C8-31** — IPC finalisation + diagnostics UX surface
   (consumed by Wave 9 macOS UX + Wave 7 CLI).
5. **C8-32 … C8-42** — `resourceLimits` + `idleBoost` + auto-tuning +
   bandwidth shaper + memory-ceiling enforcement.
6. **C8-43 … C8-47** — provider-system extensibility hardening.
7. **C8-48 … C8-54** — Google Drive provider on the already-validated
   runtime.
8. **C8-55 … C8-58** — optional advanced safeguards (active-coding
   detection, mass-change guard, support export).

**Prioritized workstream — sync modes / directionality (C8-59 … C8-66).**
The `syncMode` feature (`two-way` default, `pull-only`, `push-only`
strict-mirror one-way modes) layers on the bidirectional runtime shell and is
**prioritized within Wave 8**: it lands right after sub-block 1 (C8-1 … C8-13)
provides the download/apply pipeline, and **must be proven in the core runtime
before Wave 9 exposes it in the app**. Build order is `pull-only` → `two-way`
→ `push-only` so cloud→local download/mirror is validated first. One-way modes
are opt-in per profile and destructive to the subordinate side — see
`docs/architecture/sync-modes.md`. (The high task numbers only keep existing
IDs stable; they do not imply low priority.)

### Sync safety before users (cross-cutting, in progress)

Status: SF-1 (decisions and the two-direction mass-deletion guard),
SF-2 (the local trash), SF-3 (root identity), and SF-4 (offline
deletions through the index) landed 2026-09-06; SF-5 … SF-9 in
`core.md` "Sync safety follow-ups" are next, before Wave 9 exposes any
of it in the app: type-mismatch and collision decisions, headless
supervision, hash-based move detection, and the knowledge base for
each. The testing tiers that prove them
(`core.md` TR-1 … TR-10) are in place; TR-5, TR-8 and TR-10 wait on
SF-6, the Google Drive test account, and the native Linux traits.

### Wave 9 — macOS app UX polish

Status: pending — **now unblocked** (every Wave 8 dependency below has
landed).

- `macos.md` M3-1 … M3-8 — diagnostics UX in the macOS app: real
  IPC-backed controls (`Pause`/`Resume`/`Flush now`), full menubar
  state model, diagnostics panel with throttle reason + queue depth +
  conflicts + failures + effective ceilings + utilization + idle-boost
  reason, per-intent "why stuck" UI, live timeline tab, tests, plus the
  conflicts pane and conflict notifications (M3-7/M3-8, driving the
  shipped `vapor conflicts` CLI per
  `docs/architecture/conflict-resolution.md`). Depends on C8-27 … C8-31
  and the post-Wave-8 follow-ups C8-67 … C8-70 (all landed; C8-71
  per-path timeline detail is the one open core dependency, only for
  naming files inside notifications).
- `macos.md` M4-1 … M4-5 — profiles UX: create/rename/select/enable/
  disable/delete flows, profile-scoped override settings UI (including the
  per-profile `syncMode` toggle and its strict-mirror warning), safe
  disconnect, multi-provider fan-out UI. Depends on C8-19 … C8-26 and the
  sync-modes workstream C8-59 … C8-66.

### Wave 10 — macOS distribution hardening

Status: pending.

- `macos.md` M5-1 … M5-4 — signed + notarized end-to-end verification
  on a clean macOS host every release cycle, entitlements drift check,
  LaunchAgent/login-item stability across upgrades (N-1 → N), rollback
  artifact preservation.

### Wave 11 — CLI distribution (macOS)

Status: pending. Depends on Wave 10 (the macOS signing identity and
the `release-macos` GitHub Environment are shared).

- `cli.md` L5-1 … L5-3 — headless / server ergonomics
  (`--user-activity`, Docker recipe, deployment recipes). Docker +
  Linux systemd recipes are written but validated only on macOS in
  this wave (they run in CI via Docker-on-macOS); first-class Linux
  validation waits for Wave 13.
- `cli.md` L6-1, L6-4 — release-pipeline jobs that build `vapor` for
  `aarch64-apple-darwin` and `x86_64-apple-darwin`, strip + zstd +
  SHA256, sign with Developer ID (shares the macOS trust chain),
  publish alongside the macOS app bundle under the same GitHub Release
  tag.

After this wave, the primary deliverable is complete: polished core +
CLI + macOS app, all under one release tag.

## Deferred / optional — Windows and Linux (waves 12+)

**Start condition:** the project owner explicitly decides to ship a
non-macOS surface. Until then, these waves do not progress. Nothing in
the primary path is blocked by any of them.

The foundation laid in Waves 2 and 4 means picking any of these up
later is a matter of filling in native implementations behind an
already-stable trait surface, not a rewrite.

### Wave 12 — Windows platform implementations (optional)

- `core.md` C6-1 … C6-8 — Windows native impls for every trait
  (ReadDirectoryChangesW, Task Scheduler / SCM, Credential Manager,
  power/thermal signals, `GetLastInputInfo`, NTFS ADS, SCM /
  console-ctrl). Windows distribution trust chain doc. Add the named
  pipe IPC transport implementation on the Windows side.
- `cli.md` L2-7 — `vapor service install` round-trip automated on
  Windows CI.
- Extend the existing `windows-latest` Rust job (it already runs the
  whole test suite) with the `vapor service` round-trip.

### Wave 13 — Linux platform implementations (optional)

- `core.md` C7-1 … C7-7 — Linux native impls (inotify / fanotify,
  systemd user/system units, Secret Service / age fallback,
  `/proc/pressure` PSI, X11/Wayland idle, xattr, systemd unit policy
  doc).
- `cli.md` L2-6 — `vapor service install` round-trip automated on
  Linux CI.
- Extend the existing `ubuntu-latest` Rust job (it already runs the
  whole test suite) with the `vapor service` round-trip.

### Wave 14 — Cross-OS CLI distribution (optional)

Depends on whichever of Waves 12, 13 has shipped.

- `cli.md` L6-1 remaining targets (`x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
  `aarch64-pc-windows-msvc`).
- `cli.md` L6-5 — Windows EV-cert signing.
- `cli.md` L6-6 — GPG-signed Linux binaries + `Checksums.txt.asc`.
- Full end-to-end validation on Docker + Linux + Windows (closes
  items left open in Wave 11).

### Wave 15 — `apps/windows` app shell (optional)

Only if the project owner decides to ship a Windows GUI. Creates
`docs/plans/windows.md` and `docs/tasks/windows.md` as new surfaces
with their own plan + task list; picks the UI tech (WinUI 3, WPF, or
Tauri — see `docs/plans/core.md §8`); `release-windows` GitHub
Environment configured with EV cert secrets.

### Wave 16 — `apps/linux` app shell (optional)

Only if the project owner decides to ship a Linux GUI. Creates
`docs/plans/linux.md` and `docs/tasks/linux.md` as new surfaces; picks
the UI tech (GTK4-rs, Qt, or Tauri); `release-linux` GitHub
Environment configured with GPG key secrets; AppImage first, then
`.deb` / `.rpm` / Flatpak / Snap as demand surfaces.

## Cross-phase validation (runs continuously)

`core.md` T-1 … T-15 are standing invariants validated on every CI
run, not a wave:

- Crash/restart, throttle correctness, fs-watch callback discipline,
  self-write echo suppression, conflict policy, security posture,
  upgrade compatibility, CI parity, scope safety, ignore-rule
  precedence, performance SLOs, memory bounds, auto-tuning stability,
  multi-profile isolation, autolaunch round-trip.

T-15 (autolaunch round-trip on every OS) is interpreted against the
shipping OSes — macOS only during Waves 0–11; macOS + whichever
Windows/Linux surfaces have shipped if the optional waves land. This
must pass before any wave's exit gate is declared met.

## How to pick up a task

1. Read `docs/plans/README.md` and the matching `docs/plans/<surface>.md`
   to understand intent.
2. Consult the wave list above to see which primary-path waves are
   currently open and which tasks are unblocked.
3. Only touch Wave 12+ work if the project owner has explicitly
   opted into a non-macOS surface.
4. Move the task to `[~]` in its task file when you start; `[x]` when
   merged.
5. Update this README's wave statuses if your work closes a wave or
   unblocks a new one.

## Deferred / parking-lot items

Separate from the Windows/Linux optional bucket above, each task file
maintains its own deferred section for work that is intentionally out
of the current wave:

- `core.md` "Deferred tasks" — perf threshold tuning, production
  onboarding clarification pass.
- `macos.md` "macOS-specific deferred onboarding task" — onboarding UI
  design.

Items in those sections are not part of any current wave; promote them
into a wave above when they become active.
