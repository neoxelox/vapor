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
5. Provider-extensible engine: Google Drive is the MVP external cloud target, but every provider-neutral mechanic (error taxonomy, op-id correlation, self-write cache, remote-to-local apply, durable provider cursor) is implemented and validated against a local filesystem reference provider first; first release focuses on provider-ready architecture with Google Drive integration deferred until the engine and abstractions are stable.

## 2) Hard constraints

- Idle overhead must stay near-zero most of the time.
- Under active coding/load, only cheap bookkeeping/coalescing is allowed.
- Pressure-aware throttling is mandatory and drives all heavy work.
- Eventual consistency is required; strict real-time is not required.
- Intent durability is mandatory even if uploads are deferred for long periods.
- Bounded memory/backpressure is mandatory; storm paths must compact/coalesce instead of growing unbounded in-memory maps.
- Performance acceptance budgets must be explicit and enforceable in script/CI gates (not narrative-only).
- Multiple profiles must preserve exact sync-root safety and state isolation; same-folder fan-out must not widen scope or cross-contaminate provider/account state.
- User-configured resource ceilings (`resourceLimits`) are hard caps on daemon CPU/memory/bandwidth and must never be overshot by the throttle controller or auto-tuner; `idleBoost` may only dynamically raise ceilings when the device is genuinely idle with measured headroom, and must never preempt a `Suspended` decision.

## 3) Product architecture

1. SwiftUI app
   - Onboarding, profile management, provider auth, and root folder selection.
   - Settings: app-global controls plus per-profile overrides for sync-affecting options.
   - Menubar status: Idle, Queued, Syncing, Throttled, Suspended, Error.
   - Controls: Pause/Resume, Flush now, diagnostics.
   - Keychain secrets and launch configuration management.
2. Rust daemon (`core/daemon`, LaunchAgent)
   - FSEvents ingest, debounce/coalescing, keyed scheduler, storm handling.
   - Bounded planner/hashing/uploader stages controlled by throttle state.
   - Durable queue/state and retry/backoff.
   - Local metrics + impact-first auto-tuning.
3. Providers (`core/providers`, Rust)
     - Provider trait + capabilities.
     - `FilesystemStubProvider` is the current pre-GA default — an inert stub that satisfies the trait surface, reports no remote-changes-feed and no server-side-rename, and exists so the daemon does not run against `GoogleDriveProvider` until the credentials pipeline is ready.
     - `provider_filesystem` is the Phase 3 reference provider that replaces the stub and uses a second local directory as the remote side so bidirectional flow, self-write cache, op-id correlation, and the provider-neutral error taxonomy are exercised against a deterministic backing store before any external provider is introduced (see milestone 6).
     - `provider_gdrive` is the first external cloud target but is deliberately deferred (see milestone 12) so it integrates into an already-validated runtime.
     - Later adapters (for example iCloud, S3, R2, Proton Drive) are enabled by the extensibility hardening pass (see milestone 11) without shipping extra providers in first release.
     - Provider auth/account bindings must be profile-scoped so one device can target multiple providers or multiple accounts safely.
4. Shared contracts (`core/shared`)
   - App/daemon versioned contract models and shared schema types.

## 4) Auto-launch and lifecycle

- Default ON at install/first run.
- LaunchAgent as per-user runtime anchor (`RunAtLoad=true`, `KeepAlive=false`); see `docs/operations/launchagent-policy.md` for the full plist template and the crash-loop interaction contract.
- Optional SMAppService integration for modern login-item UX.
- Crash-loop safety is owned by the daemon and app lifecycle coordinator, not `launchd`: exponential restart delay, durable `consecutive_crashes` counter, and `CrashLoopPaused` state after 5 crashes in 10 minutes with a reasoned menubar surface.
- Toggle semantics:
  - ON: enable launch mechanism and ensure daemon running.
  - OFF: disable launch mechanism and optionally stop daemon now.

## 5) Core sync pipeline

1. Watching
   - Recursive FSEvents on root.
   - Callback does only lexical path normalization, watch-root prefix check, and ignore-rule filtering before pushing to a bounded incoming-events queue. Per-component symlink resolution runs on the runtime thread before events enter the scheduler. See `docs/architecture/data-flow.md` §"Local to remote" for the full callback discipline.
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
- The concrete suffix template, deviceId derivation, collision-avoidance fallback, and "data preservation wins over deletion" rule live in `docs/architecture/data-flow.md` §"Conflict handling".

## 7) Reliability, safety, observability

- Durable at-least-once queue semantics.
- Exponential backoff with jitter and rate-limit-aware slowdowns.
- Local-only bounded metrics (60s windows) for load and sync outcomes.
- Auto-tuning cadence 60-120s, one safe adjustment per cycle, always bounded by the user resource ceilings defined in §7.2.
- Diagnostics must always expose the current throttle reason, sync blockers, effective user resource ceilings, current utilization, and idle-boost state with a human reason.

## 7.1) Initial performance acceptance SLOs

- Idle baseline (10m no-sync window): daemon CPU avg <= 1%, p95 <= 3%.
- Active coding/load: daemon CPU avg <= 5%, p95 <= 12%; no sustained reconcile outside `IdleDrain`.
- `Suspended` state: hashing/uploads remain disabled; only lightweight coalescing and minimal queue bookkeeping run.
- FSEvents callback remains hot-path safe (p99 <= 2ms) and performs no DB/hash/network work.
- Event/intents memory is bounded with deterministic compaction/backpressure behavior under storms.
- After pressure clears, backlog convergence meets benchmark-defined target windows.
- Effective user resource ceilings (§7.2) are honored under every scenario above; no measured overshoot beyond documented tolerance.

## 7.2) User resource budgets and idle boost

Users configure two layered groups in `vapor.json` (globally, with per-profile overrides):

- `resourceLimits` — hard ceilings on daemon-process CPU, memory, and network bandwidth (`cpuPercent`, `memoryPercent`, `bandwidthPercent`). These are hard caps, not targets. The throttle controller and auto-tuner must never drive the daemon above them.
- `idleBoost` — opt-in dynamic headroom that raises effective ceilings when all of the following hold: user-idle for at least `minIdleSeconds`, non-Vapor utilization at or below each `headroom*Percent`, and throttle state is `IdleDrain`. While active, effective ceilings linearly ramp toward `boost*Percent` over `rampUpSeconds`; any condition break ramps back to base ceilings over `rampDownSeconds` (always <= `rampUpSeconds` so activity resumption is non-invasive).

Layering rules:

- User ceilings are a ceiling on top of the existing throttle controller. User ceilings never relax the controller; a `Suspended` decision always wins.
- Auto-tuning (§7) operates strictly inside the current effective ceilings and must react to ceiling changes within one tuning cycle without oscillation.
- Profile overrides resolve by taking the MIN with global values (overrides may only *lower* effective ceilings); `idleBoost.enabled = false` in any enabled profile disables boost daemon-wide.
- Enforcement surfaces: workgate concurrency caps (CPU), provider-neutral bandwidth shaper (network), bounded caches and compaction thresholds that react to RSS (memory). Diagnostics expose current effective ceilings, utilization, and the boost reason code.

Defaults are deliberately conservative (invisible-first principle); advanced users may relax them per profile or per machine.

## 8) Milestone order

1. Repository/documentation foundation and contributor operating model.
2. App shell + daemon lifecycle + auto-launch.
3. Native app bundle/distribution foundation (script-first packaging, signing, notarization path).
4. Low-impact local engine core plus durability substrate (durable queue, retries, storm deferral, interruptible reconcile, and bounded backpressure).
5. Runtime integration and hardening pass: compose the real daemon loop, close local safety/privacy gaps, remove pre-GA transitional code, and prepare bounded concurrency plumbing for later provider-backed worker execution.
6. Local filesystem reference provider and bidirectional runtime shell: replace the Phase 2.5 staged executor simulator with real provider-backed planner/hash/upload/download execution against a loopback filesystem provider, exercising every provider-neutral bidirectional mechanic (self-write cache, remote-to-local apply, op-id correlation, provider-neutral error taxonomy, durable provider cursor) before any external cloud provider is introduced.
7. Conflict/tombstone safety and deterministic race handling hardening, validated against the filesystem reference provider.
8. Multi-profile provider/account model with layered settings, profile isolation, and same-folder multi-provider fan-out.
9. XPC contract hardening and full diagnostics UX.
10. Auto-tuning and user resource-budget enforcement (hard CPU/memory/bandwidth ceilings plus idle-boost dynamic headroom, layered over the existing throttle controller).
11. Provider-system extensibility hardening (provider-ready compatibility and performance for future providers, with the filesystem reference provider serving as the contract-test harness).
12. Google Drive provider integration on top of the already-validated provider-neutral runtime (deferred until the engine, abstractions, and acceptance criteria are stable).
13. Optional advanced safeguards and enhancements.

## 9) Definition of done (applies to every milestone)

- Functional behavior validated for happy path and failure path.
- Crash/restart recovery validated with no lost intent state.
- Pressure transitions validated against throttle states.
- UI exposes status and reason for degraded behavior.
- No heavy work in FSEvents callback.
- Security expectations met (Keychain, redaction, least-sensitive logs).

## 10) Documentation deliverables tied to this plan

- Root `README.md` must remain a concise product-facing index; detailed operator/developer guidance belongs under `docs/` and must stay linked/current.
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
- benchmark/perf workflow should run via scripts as a release gate with threshold-based regression checks.
- CI should report separate checks for lint, tests, and benchmark runs (when enabled) and fail fast on regressions.
- Caching and toolchain pinning should be used to keep CI reliable and reasonably fast.
