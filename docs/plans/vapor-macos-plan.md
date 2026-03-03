# vapor macOS implementation plan

Derived from: `docs/plans/vapor-original-plan-verbatim.md`

Execution checklist: `docs/plans/vapor-macos-task-list.md`

## 0) Mission

Build `vapor` as an invisible-first macOS sync product with a SwiftUI app + Rust daemon.
The service must default to auto-launch at login, stay low-impact under user load, and provide eventual consistency.

## 1) Principles (ordered by priority)

1. Do no harm to developer workflow and battery/thermal budgets.
2. Opportunistic sync: aggressively defer under pressure.
3. Durable correctness: never lose intent state, recover after crashes/restarts.
4. Best-effort freshness: seconds when idle, minutes when busy.
5. Provider-extensible engine: Google Drive first, then S3/R2.

## 2) Hard constraints

- Idle overhead must stay near-zero most of the time.
- Under active coding/load, only cheap bookkeeping/coalescing is allowed.
- Pressure-aware throttling is mandatory and drives all heavy work.
- Eventual consistency is required; strict real-time is not required.
- Intent durability is mandatory even if uploads are deferred for long periods.

## 3) Product architecture

1. SwiftUI app
   - Onboarding, provider auth, root folder selection.
   - Settings: excludes, policy, auto-launch toggle, provider selection.
   - Menubar status: Idle, Queued, Syncing, Throttled, Suspended, Error.
   - Controls: Pause/Resume, Flush now, diagnostics.
   - Keychain secrets and launch configuration management.
2. Rust daemon (LaunchAgent)
   - FSEvents ingest, debounce/coalescing, keyed scheduler, storm handling.
   - Bounded planner/hashing/uploader stages controlled by throttle state.
   - Durable queue/state and retry/backoff.
   - Local metrics + impact-first auto-tuning.
3. Providers (Rust)
   - Provider trait + capabilities.
   - `provider_gdrive` first, `provider_s3`/R2 next.

## 4) Auto-launch and lifecycle

- Default ON at install/first run.
- LaunchAgent as per-user runtime anchor (`RunAtLoad=true`, controlled `KeepAlive`).
- Optional SMAppService integration for modern login-item UX.
- Crash-loop safety: exponential restart delay and clear paused-state diagnostics.
- Toggle semantics:
  - ON: enable launch mechanism and ensure daemon running.
  - OFF: disable launch mechanism and optionally stop daemon now.

## 5) Core sync pipeline

1. Watching
   - Recursive FSEvents on root.
   - Callback only: normalize path, exclude check, record event.
2. Debounce/coalesce
   - 250ms stabilization tick.
   - Conservative debounce defaults and adaptive bounds.
3. Scheduling
   - One latest intent per path: upload/delete/rename.
   - Superseding semantics and dirty-while-running replay.
4. Planner + hashing
   - Hashing minimal by default; only for strict/conflict/beneficial-idle paths.
5. Upload/apply
   - Concurrency/rate gates are fully throttle-state driven.
6. Storm management
   - Detect storms and defer subtree reconcile to idle windows.

## 6) Bidirectional-by-default MVP adjustments

This project chooses bidirectional support in MVP (not deferred).

- Add low-frequency remote changes polling in early provider milestones.
- Add remote-to-local apply pipeline with durable intents.
- Implement self-write loop prevention (`self_write_cache` TTL + operation IDs).
- Implement deterministic conflict handling with user-visible outcomes.
- Track delete/tombstone semantics on both sides.
- Expand UI/XPC state model to include conflict and remote-polling health.

Recommended default conflict policy:

- Keep both copies (never silent overwrite).
- Primary winner remains at canonical path; alternate copy gets conflict suffix with device/timestamp.

## 7) Reliability, safety, observability

- Durable at-least-once queue semantics.
- Exponential backoff with jitter and rate-limit-aware slowdowns.
- Local-only bounded metrics (60s windows) for load and sync outcomes.
- Auto-tuning cadence 60-120s, one safe adjustment per cycle.
- Diagnostics must always expose the current throttle reason and sync blockers.

## 8) Milestone order

1. Repository/documentation foundation and contributor operating model.
2. App shell + daemon lifecycle + auto-launch.
3. Low-impact local engine core.
4. Google Drive provider with bidirectional event flow.
5. Durability, retries, storm deferral, deferred reconcile, and conflict safety.
6. XPC contract hardening and full diagnostics UX.
7. Auto-tuning and performance stabilization.
8. Provider extensibility and S3/R2 module.
9. Optional advanced safeguards and enhancements.

## 9) Definition of done (applies to every milestone)

- Functional behavior validated for happy path and failure path.
- Crash/restart recovery validated with no lost intent state.
- Pressure transitions validated against throttle states.
- UI exposes status and reason for degraded behavior.
- No heavy work in FSEvents callback.
- Security expectations met (Keychain, redaction, least-sensitive logs).

## 10) Documentation deliverables tied to this plan

- `README.md` must become an operator/developer guide (not just project title).
- `.gitignore` must cover Rust + Swift/Xcode + macOS + local secret/state artifacts.
- `AGENTS.md` must define contribution rules, boundaries, test requirements, and safety playbooks.

## 11) Operational readiness requirements (must be planned before deep implementation)

- Distribution trust chain
  - Define signing identities, hardened runtime requirements, notarization flow, and entitlement review.
  - Define release artifact validation for app + daemon packages.
- OAuth/provider operations
  - Define Google OAuth app setup, PKCE redirect handling, token refresh/error policy, and local secret bootstrap.
  - Define degraded behavior when auth refresh fails repeatedly.
- Durability evolution policy
  - Define queue/state schema versioning, migration tests, rollback posture, and corruption recovery.
  - Define compatibility expectations between app, daemon, and schema versions.
- Upgrade/rollback policy
  - Define app/daemon compatibility matrix across releases.
  - Define LaunchAgent migration behavior and safe rollback mechanics.
- Measurable acceptance budgets
  - Set explicit CPU/I/O/latency/error targets for representative workloads.
  - Define repeatable benchmark and stress harness for milestone gates.

## 12) Developer tooling and CI automation (early mandatory)

- Repository scripts must exist for both stacks (Swift and Rust):
  - lint
  - format (check and apply modes)
  - test
- Script entry points should be stable and documented so local and CI usage are identical.
- GitHub Actions workflows must run on pull requests and main-branch pushes:
  - lint workflow: executes Swift and Rust lint/format-check paths.
  - test workflow: executes Swift and Rust test suites.
- CI should report separate checks for lint and tests and fail fast on regressions.
- Caching and toolchain pinning should be used to keep CI reliable and reasonably fast.
