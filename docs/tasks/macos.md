# Vapor macOS task list

Plan reference: `docs/plans/macos.md`
Core tasks (runtime, engine, platform layer): `docs/tasks/core.md`
CLI tasks: `docs/tasks/cli.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

Scope of this list: macOS app surface work only — SwiftUI shell, menubar UX,
macOS-native lifecycle UI, and macOS distribution trust chain. Runtime /
engine / platform-abstraction work that used to live here has moved to
`docs/tasks/core.md`.

## Phase M0 - Repository foundation (complete)

- [x] M0-1 Define monorepo layout (`apps/macos`, `core/daemon`,
      `core/providers`, `core/shared`, `docs`).
- [x] M0-2 Keep root `README.md` concise/product-facing; detailed guidance
      under `docs/`.
- [x] M0-3 Upgrade `.gitignore` for Rust + Swift/Xcode + macOS + runtime
      artifacts.
- [x] M0-4 Create comprehensive `AGENTS.md` (subsequently updated in
      `docs/tasks/core.md` C0-7 for the portable-runtime reframing).
- [x] M0-5 Add `docs/architecture/` skeleton and baseline references.
- [x] M0-6 Add repository scripts for Rust lint/format/test.
- [x] M0-7 Add repository scripts for Swift lint/format/test.
- [x] M0-8 Add local developer runbook.
- [x] M0-9 GitHub Actions lint workflow (Swift + Rust).
- [x] M0-10 GitHub Actions test workflow (Swift + Rust).
- [x] M0-11 Required CI checks + branch-protection guidance.
- [x] M0-12 Distribution trust-chain plan (moved to
      `docs/operations/macos/distribution-trust-chain.md`).
- [x] M0-13 OAuth/provider operations plan (stays in
      `docs/operations/provider-auth-operations.md` — cross-platform via
      `core/platform/secrets`).
- [x] M0-14 State schema versioning + migration test strategy.
- [x] M0-15 App-daemon compatibility matrix + upgrade/rollback policy.
- [x] M0-16 Measurable acceptance budget categories + benchmark harness.
- [x] M0-17 Numeric SLO thresholds for idle/load/storm/recovery scenarios.
- [x] M0-18 Benchmark/perf CI gating policy.

## Phase M1 - App shell, daemon lifecycle, auto-launch

- [x] M1-1 Build SwiftUI app shell (onboarding/settings/menubar placeholders).
- [x] M1-2 Implement LaunchAgent lifecycle manager with default-ON behavior
      (to be migrated to `core/lifecycle` per `docs/tasks/core.md` C4-7;
      Swift keeps a thin shim).
- [x] M1-3 Optional `SMAppService` integration for login-item UX.
- [x] M1-4 Auto-launch toggle semantics (enable/disable + optional
      stop-now).
- [x] M1-5 Crash-loop detection + exponential relaunch delay (policy moves
      to `core/lifecycle::CrashLoopGuard` per `docs/tasks/core.md` C4-2).
- [x] M1-6 Validate LaunchAgent plist and crash-loop interaction per
      `docs/operations/macos/launchagent-policy.md`:
      - Plist audit (expected keys exactly, `KeepAlive=false`).
      - SIGKILL scenario (daemon killed with `kill -9`; no auto-restart for
        30s; next user action restarts via coordinator, not `launchd`).
      - Crash-loop pause scenario (5 crashes in 10 minutes reach
        `CrashLoopPaused` with reasoned menubar surface).
      - Clean shutdown scenario (menubar-quit exits cleanly and `launchd`
        stays passive).

Exit gate:

- Daemon reliably starts at login and avoids restart-loop meltdown.
- LaunchAgent and in-process crash-loop protection do not collide; M1-6
  scenarios pass under automated test.

## Phase M1.5 - Native app bundle and distribution foundation (complete)

- [x] M1.5-1 Convert `Vapor` executable target to `@main App` with no
      CLI-style entrypoint conflicts.
- [x] M1.5-2 Initial native macOS window structure (`ContentView`).
- [x] M1.5-3 Create `apps/macos/scripts/package.sh`.
- [x] M1.5-4 Deterministic `.app` bundle assembly in `dist/Vapor.app`.
- [x] M1.5-5 `Info.plist` metadata with `LSMinimumSystemVersion=26.0` and
      deterministic version/build derivation.
- [x] M1.5-6 `AppIcon.icns` from `assets/icon.png` via `sips` + `iconutil`.
- [x] M1.5-7 Optional resource copy from `apps/macos/Resources/**`.
- [x] M1.5-8 Signing modes: ad-hoc default; Developer ID + hardened runtime
      when `VAPOR_SIGN_IDENTITY` is set; optional entitlements.
- [x] M1.5-9 Bundle verification (`plutil`, `codesign --verify`, `spctl`).
- [x] M1.5-10 Produce `dist/Vapor.zip` via `ditto --keepParent`.
- [x] M1.5-11 Notarization + stapling flow via `VAPOR_NOTARY_PROFILE`.
- [x] M1.5-12 Integrate into `./scripts/build.sh package`.
- [x] M1.5-13 Optional Xcode convenience workflow.
- [x] M1.5-14 Update `apps/macos/README.md` with local commands.
- [x] M1.5-15 Non-blocking daemon lifecycle bootstrap at startup.
- [x] M1.5-16 Close-window behavior: remove Dock presence, keep menubar.
- [x] M1.5-17 Decouple window-close and daemon lifecycle.
- [x] M1.5-18 Explicit menubar controls (`Open Vapor`, `Quit Vapor`).
- [x] M1.5-19 Lifecycle coverage tests (window close / reopen / quit).
- [x] M1.5-20 Update lifecycle docs across `README.md`,
      `apps/macos/README.md`, `AGENTS.md`.
- [x] M1.5-21 Packaging assertions: both `Vapor` and `vapord` in
      `Contents/MacOS/`; runtime daemon launch targets bundled sibling.

Exit gate (met):

- `open dist/Vapor.app` launches a normal app window.
- `apps/macos/scripts/package.sh` produces `dist/Vapor.app` and
  `dist/Vapor.zip` non-interactively.
- Closing main window leaves menubar + daemon running.
- Bundle embeds both executables; runtime launch targets bundled daemon.

## Phase M2 - Migrate to Rust-backed lifecycle (macOS consumer)

Depends on: `docs/tasks/core.md` C4.

- [x] M2-1 Replace the macOS Swift `LaunchAgentController` internals with a
      shim that invokes `vapor service install / start / stop` as a
      subprocess (or the FFI surface from C4-5). The public
      `LaunchAgentControlling` Swift protocol stays; the default
      implementation changes. *(Shipped as `VaporCLIServiceController`
      driving `vapor service … --json`. The CLI is bundled at
      `Contents/Helpers/vapor` — it cannot sit in `Contents/MacOS/`
      because the default macOS filesystem is case-insensitive and
      `vapor` would collide with the `Vapor` app binary.)*
- [x] M2-2 Replace the Swift `CrashLoopGuard` with a consumer of the
      Rust-backed `core/lifecycle` state (read over IPC / subprocess).
      Remove the duplicate Swift policy once parity tests (C4-6) pass.
- [x] M2-3 Update `apps/macos/README.md` to describe the new model: Swift
      app delegates lifecycle to the Rust core via the `vapor` CLI.
- [x] M2-4 End-to-end regression test: install / uninstall / start / stop /
      crash-loop pause / acknowledge flows through the Rust-backed stack
      with the existing Swift UI unchanged. *(Swift facade + CLI-contract
      tests, Rust dispatch/manager tests, and the black-box
      `./scripts/e2e.sh --full` service round-trip phase (L2-5) that
      walks install → crash-loop supervision → pause → acknowledge →
      stop → uninstall against real launchd on CI. Swift UI itself is
      owner-verified per the standing test carve-out.)*
- [x] M2-5 App-side daemon health tick: periodically detect unexpected
      daemon absence (IPC ping or pid probe) and route the crash through
      the crash-loop guard so it actually fires in production.
      *(Shipped as `DaemonHealthMonitor`, a 30 s timer calling
      `vapor service check`; detection, crash registration, and restart
      policy all run Rust-side in `core/lifecycle`.)*
- [x] M2-6 Persist crash-loop state durably (`last_crash_at_ms`,
      `consecutive_crashes` in the state DB or a lifecycle side-file) so
      backoff survives app restarts and is shared across surfaces.
      *(Shipped as `<vapor_dir>/state/lifecycle.json` owned by
      `core/lifecycle`; every surface hydrates from and persists to it.)*

Exit gate:

- No runtime / lifecycle policy lives in Swift.
- macOS app behavior is identical before and after the migration.

## Phase M3 - Diagnostics UX on macOS (IPC consumer)

Depends on: `docs/tasks/core.md` C5 (IPC channel) and C8-27..31 (diagnostics
surface).

- [ ] M3-1 Replace placeholder app controls (`Pause/Resume`, `Flush now`)
      with real IPC-backed calls; hide any control that does not yet have a
      backing endpoint.
- [ ] M3-2 Implement full menubar state model + reasoned status messages
      consumed via IPC.
      *(Partly landed in the full-repo review: the health tick reads
      `vapor status --json` and maps run state, throttle state, queue
      depth and failed count onto the surface state with the daemon's
      throttle reason; the Dashboard and menu bar show it. Remaining:
      the per-state copy and controls this task specifies beyond that.)*

- [ ] M3-3 Implement diagnostics panel (throttle reason, queue depth,
      conflicts, failures, effective ceilings, utilization, idle-boost
      reason).
- [ ] M3-4 Implement per-intent "why stuck" diagnostics UI per
      `docs/architecture/ipc-contracts.md` diagnostics section (filterable
      list + per-row drill-down; surface
      `dropped_incoming_event_count`).
- [ ] M3-5 Implement the live timeline tab (bounded buffer; default 1000
      events; non-persistent across relaunch).
- [ ] M3-6 Tests for timeline ordering, truncation at max length,
      IPC field-omission.
- [ ] M3-7 Conflicts pane: list unresolved keep-both conflicts by driving
      the bundled CLI (`vapor conflicts list --json` — the same shim
      pattern as lifecycle) with per-row keep-canonical / keep-copy
      actions via `vapor conflicts resolve --json`, plus a Reveal in
      Finder affordance. No scan or resolution logic in Swift; the CLI is
      the single engine (`docs/architecture/conflict-resolution.md`).
- [ ] M3-8 Conflict notification: menubar badge + native user
      notification on `conflict` timeline events, opening the M3-7 pane.
      Timeline events are the trigger, never the ledger — the pane always
      lists from the CLI scan, so the timeline's 1000-event cap can never
      hide a conflict. Naming the file inside the notification depends on
      core.md C8-71.
- [ ] M3-9 Decisions pane: the questions the daemon parked, driving
      `vapor decisions list --json` / `show` / `resolve --json`
      (`docs/architecture/data-flow.md` §Decisions): one row per open
      decision with its plain-language question and its short option
      list as buttons, a menubar badge and a native notification on
      `decision` timeline events, and `decisions_pending` from
      `vapor status --json` as the count. Kinds today: `mass-deletion`,
      `root-missing`, `root-replaced`, `type-mismatch`.
- [ ] M3-10 Trash pane: what Vapor removed on this device, driving
      `vapor trash list --json` / `restore --json` / `empty --json`,
      with the reason and the original path per row and a Restore
      action; the `trash` settings (`enabled`, `retentionDays`,
      `useSystemTrash`) in Settings.
- [ ] M3-11 Root state: a profile held on a missing or replaced sync
      root (`suspended_reason` / the run-state reason naming the
      decision) shows as its own state with the decision's options,
      not as a generic error.

Exit gate:

- User can understand "what is happening" and "why" from the app without
  shell access.
- Unresolved conflicts are discoverable (badge + notification), listable,
  and resolvable entirely from the app.
- Open decisions and the trash are discoverable, listable, and
  answerable or restorable entirely from the app.

## Phase M4 - Profiles UX on macOS

Depends on: `docs/tasks/core.md` C8-19..26 and the sync-modes workstream
C8-59..66.

- [ ] M4-1 App UI flows to create, rename, select, enable/disable, and
      delete profiles; bind each profile to a provider + authenticated
      account.
- [ ] M4-2 Settings UI for profile-scoped override knobs (sync roots,
      ignore rules, resource ceilings, idle-boost, and the per-profile
      `syncMode` selector: `two-way` / `pull-only` / `push-only`). Surface
      each profile's effective mode and, from C8-65, its mirror-driven
      revert/delete counts.
- [ ] M4-3 Safe profile disconnect/delete flows that leave other profiles
      untouched.
- [ ] M4-4 UI flows for multi-provider fan-out (same local root → multiple
      providers/accounts).
- [ ] M4-5 Strict-mirror safety UX for the one-way modes: a clear
      first-enable confirmation that states the subordinate side will be made
      to exactly match the source and that this **permanently** overwrites
      divergent edits and removes local/cloud-only content (there is no
      recoverable copy). Follows Apple HIG for destructive-action
      confirmation. Depends on `docs/architecture/sync-modes.md §Safety`.

Exit gate:

- Profiles can be fully managed from the app without CLI intervention.
- One-way sync modes cannot be enabled without the destructive-action
  warning; the app never silently deletes/reverts to converge a mirror.

## Phase M5 - macOS distribution hardening

- [ ] M5-1 Verify signed + notarized path end-to-end against a clean macOS
      host every release cycle.
- [ ] M5-2 Verify entitlements drift check runs on every release build.
- [ ] M5-3 Verify LaunchAgent/login-item behavior stability across upgrades
      (upgrade from N-1 → N does not leave orphaned plists or duplicate
      login items).
- [ ] M5-4 Preserve rollback artifacts per `docs/operations/release-
      process.md`.

Exit gate:

- Every macOS release cycle validates the macOS trust chain automatically.

## Phase MT - Testing discipline (macOS surface)

Policy: `AGENTS.md §9`. Full taxonomy:
`docs/architecture/testing-strategy.md`. macOS-specific scope lives in
`docs/plans/macos.md §7.1`.

- [ ] MT-1 Audit the existing Swift test suite (`apps/macos/Tests/`)
      for trivial-test smell — `Default` impls mirroring constants,
      `Debug`/`Display` string equality tests, `serde` round-trips of
      trivial structs. Remove or replace with behavior-level
      assertions. Keep the coverage of configuration, lifecycle,
      localization, paths, logger redaction, bundle layout, and
      ViewModel state transitions intact.
- [ ] MT-2 When `core/lifecycle` lands (Phase M2 / `core.md` C4),
      retire the Swift-side `DaemonLifecycleManagerTests` and rely on
      the Rust-side parity tests from C4-6. Do not maintain duplicate
      policy tests.
- [ ] MT-3 **Do not add UI rendering tests.** No SwiftUI view
      snapshots, no menubar layout assertions, no Dock-transition
      tests, no keyboard-focus tests. UI correctness is verified by
      the project owner manually. Any PR adding such tests is
      rejected; the reviewer cites `AGENTS.md §9.3`.

## macOS-specific deferred onboarding task

- [ ] O-1 Design and implement the production onboarding flow (information
      architecture, step sequence, copy, UX states). Run a clarification
      pass with the project owner to define the onboarding structure before
      implementation. This task is deliberately owned here even though the
      runtime underneath is cross-platform, because the onboarding UI is
      macOS-native. Windows/Linux app onboarding will have parallel tasks in
      their own task lists once the apps start.
