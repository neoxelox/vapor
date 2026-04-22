# Tasks

Execution checklists mapped to the plans in `docs/plans/`. This directory
serves two purposes:

1. **Per-surface task lists** — one file per deliverable surface, each
   tracking concrete items with `[ ] / [~] / [x]` status.
2. **Cross-surface roadmap** — this README is the orchestrator. It tells
   you **what should be done next** when multiple task lists have pending
   items, which tasks block which, and how the MVP lines up across
   `core`, `macos`, and `cli`.

If you only have time to read one doc before picking up work, read this
one.

## File convention

Tasks are flat and platform-named: one file per deliverable surface.

- `core.md` — portable Rust runtime tasks. The biggest file; everything
  that powers every app surface lives here.
- `macos.md` — macOS app surface tasks.
- `cli.md` — `vapor` CLI tasks.
- `windows.md` — placeholder. Added when `apps/windows` starts.
- `linux.md` — placeholder. Added when `apps/linux` starts.

Every file mirrors its plan in `docs/plans/<surface>.md`. Plans are
*intent*; these files track *execution*.

Status legend (used in every task file):

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

## Cross-surface roadmap (what should be done next)

Vapor is executed in **waves**. A wave is a coherent set of tasks across
surfaces that must land together before the next wave starts. Within a
wave, tasks can be worked in parallel unless a dependency is called out.

### Wave 0 — Finish the macOS MVP invariants (in flight)

Status: active. Do these before starting portability work.

- `macos.md` M1-6 — LaunchAgent plist + crash-loop validation scenarios.
  Automates the last outstanding bullet from the macOS lifecycle
  milestone (plist audit / SIGKILL / crash-loop pause / clean shutdown).

No wave-0 work in `core.md` or `cli.md`. The macOS app closes out its
pre-portability milestones first so the engine fixes that follow do not
disturb a partially-completed lifecycle story.

### Wave 1 — Documentation and naming hygiene (portability foundation)

Status: active. Mostly complete via the docs reorganisation.

- `core.md` C0-1 … C0-10 — plans/tasks reorg, docs group READMEs,
  `xpc-contracts` → `ipc-contracts` rename, platform-abstractions
  reference, `AGENTS.md` reframing, constants / FSEvents vocabulary
  cleanup.

Prerequisite for every later wave because it removes macOS-only
vocabulary and lays out the target directory structure.

### Wave 2 — Engine portability fixes (`core/*` compiles on every OS)

Status: pending.

- `core.md` C1-1 … C1-7 — runtime_paths permission gates,
  `HOME`/`USERPROFILE` resolution, UTF-8 path encoding in `state_db`,
  Windows-prefix handling in `fs_events`, `libc` dep cfg-gate, Windows
  + Linux CI matrix for Rust jobs.

Blocks Wave 3 (the `core/platform` crate needs a cross-OS compilable
workspace). Swift jobs stay macOS-only.

### Wave 3 — Remaining runtime gaps (from prior Phase 2.5)

Status: pending. Can run in parallel with Wave 2 because these are
platform-agnostic fixes.

- `core.md` C2-1 … C2-4 — replace default-`ThrottleInputs` placeholder
  with real input source, throttle permit-id wrap-around hardening,
  `SystemTime` → `Instant` migration for tick-cadence clocks, throttle
  controller hysteresis / min-dwell.

No blockers; these close runtime invariants the product depends on
regardless of platform.

### Wave 4 — Platform abstraction layer

Status: pending. Requires Wave 2.

- `core.md` C3-1 … C3-10 — new `core/platform` crate with trait
  skeletons + macOS-native implementations ported from existing
  Swift/docs (`FsWatcher`, `ServiceInstaller`, `SecretStore`,
  `PlatformMetricsSampler`, `IdleNotifier`, `FilesystemCapabilities`,
  `ProcessSupervisor`). Windows/Linux impls stubbed to
  `unimplemented!()`. Publish `docs/architecture/platform-abstractions.md`
  as the living reference.

macOS daemon behavior must stay byte-for-byte identical before and
after this wave.

### Wave 5 — Daemon lifecycle moves into Rust

Status: pending. Requires Wave 4 (specifically `ServiceInstaller`).

- `core.md` C4-1 … C4-7 — new `core/lifecycle` crate; port
  `CrashLoopGuard` and `DaemonLifecycleManager` from Swift; expose a
  stable surface (C-ABI or CLI subprocess) the macOS app can consume.
- `macos.md` M2-1 … M2-4 — the macOS app starts delegating lifecycle
  to `core/lifecycle` via the `vapor` CLI. End-to-end regression test
  that the macOS UX is unchanged.

These two task groups land **together** — do not merge one without the
other.

### Wave 6 — IPC channel + `vapor` CLI lifecycle commands

Status: pending. Requires Waves 4 and 5.

- `core.md` C5-1 … C5-5 — transport decision (UDS on Unix, named pipe
  on Windows), JSON-RPC 2.0 framing, server-side implementation,
  client library, status/control endpoints, skew-matrix tests.
- `cli.md` L0-1 … L0-5 — `vapor` crate skeleton and `--version`.
- `cli.md` L1-1 … L1-4 — `vapor run`, `vapor config`, `vapor version`,
  `vapor doctor`.
- `cli.md` L2-1 … L2-5 — `vapor service {install,uninstall,start,stop,status}`
  driving the macOS `ServiceInstaller`; round-trip automated on macOS
  CI.

This is the "ship the CLI on macOS" milestone. The CLI proves
`core/platform` + `core/lifecycle` actually work against real macOS
users before we generalize to other OSes.

### Wave 7 — Windows + Linux platform implementations

Status: pending. Requires Waves 4, 5, 6.

- `core.md` C6-1 … C6-8 — Windows native impls for every trait
  (ReadDirectoryChangesW, Task Scheduler / SCM, Credential Manager,
  power/thermal signals, `GetLastInputInfo`, NTFS ADS,
  SCM / console-ctrl). Windows distribution trust chain doc.
- `core.md` C7-1 … C7-7 — Linux native impls (inotify/fanotify,
  systemd user/system units, Secret Service / age fallback,
  `/proc/pressure` PSI, X11/Wayland idle, xattr, systemd unit policy
  doc).
- `cli.md` L2-6, L2-7 — `vapor service install` round-trip automated
  on Linux and Windows CI.
- `cli.md` L5-1 … L5-3 — headless / server ergonomics
  (`--user-activity`, Docker recipe, deployment recipes).

After this wave, the `vapor` CLI runs with full native-optimal
behavior on every supported OS.

### Wave 8 — IPC-driven CLI surface + auth flows

Status: pending. Requires Wave 6 (IPC) and whichever provider wave ships
first.

- `cli.md` L3-1 … L3-7 — `vapor status / pause / resume / flush-now /
  reconcile / timeline / logs`. All IPC-backed. `--json` stable; never
  hang when no daemon is running.
- `cli.md` L4-1 … L4-3 — `vapor auth login / logout / status` via PKCE.
  Depends on `SecretStore` from Wave 4 and the provider from Wave 9
  first touchpoint (Google Drive) or Wave 9-filesystem's no-op auth.

### Wave 9 — Runtime capability completion (was macOS Phases 3–10)

Status: pending. Parallelizable with Waves 6–8 except where noted.

Covers the full C8-1 … C8-58 span in `core.md`:

1. **C8-1 … C8-13** — filesystem reference provider + bidirectional
   runtime shell + `self_write_cache` + simulator removal. Blocks
   every later sub-wave.
2. **C8-14 … C8-18** — conflict policy, `deviceId`, tombstones, race
   resolution, corruption recovery.
3. **C8-19 … C8-26** — multi-profile model, profile-scoped overrides,
   shared-root watch dedup, blast-radius containment.
4. **C8-27 … C8-31** — IPC finalisation + diagnostics UX (consumed by
   `macos.md` M3 on the app side).
5. **C8-32 … C8-42** — `resourceLimits` + `idleBoost` + auto-tuning +
   bandwidth shaper + memory-ceiling enforcement.
6. **C8-43 … C8-47** — provider-system extensibility hardening.
7. **C8-48 … C8-54** — Google Drive provider on the already-validated
   runtime.
8. **C8-55 … C8-58** — optional advanced safeguards (active-coding
   detection, mass-change guard, support export).

Each of these opens matching macOS UX work in `macos.md`:

- C8-27 … C8-31 ⇒ `macos.md` M3-1 … M3-6 (diagnostics UI).
- C8-19 … C8-26 ⇒ `macos.md` M4-1 … M4-4 (profiles UX).

### Wave 10 — macOS distribution hardening

Status: pending. Depends on at least one release cycle in the
multi-platform world so the entitlement drift and upgrade-from-N-1
checks have real data.

- `macos.md` M5-1 … M5-4 — signed + notarized end-to-end verification,
  entitlements drift check, upgrade stability, rollback artifact
  preservation.

### Wave 11 — CLI distribution

Status: pending. Depends on Wave 7 (Windows/Linux trust chains) so the
per-target-triple release jobs have signing identities to consume.

- `cli.md` L6-1 … L6-6 — cross-target-triple binary release jobs,
  strip + zstd + SHA256 per asset, publish alongside app releases,
  per-OS signing.

## Cross-phase validation (runs continuously)

`core.md` T-1 … T-15 are standing invariants validated on every CI run,
not a wave:

- Crash/restart, throttle correctness, fs-watch callback discipline,
  self-write echo suppression, conflict policy, security posture,
  upgrade compatibility, CI parity, scope safety, ignore-rule
  precedence, performance SLOs, memory bounds, auto-tuning stability,
  multi-profile isolation, autolaunch round-trip on every OS.

These must pass before any wave's exit gate is declared met.

## How to pick up a task

1. Read `docs/plans/README.md` and the matching `docs/plans/<surface>.md`
   to understand intent.
2. Consult this README's wave table to see which waves are currently
   open and which tasks are unblocked.
3. Move the task to `[~]` in its task file when you start; `[x]` when
   merged.
4. Update this README's wave statuses if your work closes a wave or
   unblocks a new one.

## Deferred / parking-lot items

Each task file maintains its own deferred section for work that is
intentionally out of the current wave:

- `core.md` "Deferred tasks" — perf threshold tuning, production
  onboarding clarification pass.
- `macos.md` "macOS-specific deferred onboarding task" — onboarding UI
  design.

Items in those sections are not part of any current wave; promote them
into a wave above when they become active.
