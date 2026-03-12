# vapor macOS task list

Plan reference: `docs/plans/vapor-macos-plan.md`

Distribution foundation reference: `docs/plans/vapor-macos-distribution-foundation-plan.md`

Original source: `docs/plans/vapor-original-plan-verbatim.md`

Status legend:

- [ ] pending
- [~] in progress
- [x] complete

## Phase 0 - Repository foundation and planning docs

- [x] P0-1 Define monorepo layout (`apps/macos`, `core/daemon`, `core/providers`, `core/shared`, `docs`).
- [x] P0-2 Keep root `README.md` concise/product-facing and move detailed operator/developer guidance into `docs/`.
- [x] P0-3 Upgrade `.gitignore` for Rust + Swift/Xcode + macOS + runtime/state/secrets artifacts.
- [x] P0-4 Create comprehensive `AGENTS.md` with boundaries, standards, tests, and safety policy.
- [x] P0-5 Add `docs/architecture/` skeleton and baseline diagrams/contracts placeholders.
- [x] P0-6 Add repository scripts for Rust lint/format/test.
- [x] P0-7 Add repository scripts for Swift lint/format/test.
- [x] P0-8 Add local developer runbook section documenting script usage.
- [x] P0-9 Add GitHub Actions lint workflow for Swift + Rust on PR/push.
- [x] P0-10 Add GitHub Actions test workflow for Swift + Rust on PR/push.
- [x] P0-11 Configure required CI checks and branch protection guidance.
- [x] P0-12 Define distribution trust-chain plan (signing, hardened runtime, notarization, entitlements).
- [x] P0-13 Define OAuth/provider operations plan (PKCE flow, token refresh failure handling).
- [x] P0-14 Define state schema versioning, migration test strategy, and rollback posture.
- [x] P0-15 Define app-daemon compatibility matrix and upgrade/rollback policy.
- [x] P0-16 Define measurable acceptance budget categories and benchmark harness approach.
- [x] P0-17 Convert budget categories into explicit numeric SLO thresholds for idle/load/storm/recovery scenarios.
- [x] P0-18 Define benchmark/perf CI gating policy (release-only invocation, thresholds, and failure behavior).

Exit gate:

- New contributor can clone repo, understand architecture, run local lint/test scripts, and see equivalent CI checks configured.

## Phase 1 - App shell, daemon lifecycle, auto-launch

- [x] P1-1 Build SwiftUI app shell (onboarding/settings/menubar status placeholders).
- [x] P1-2 Implement LaunchAgent lifecycle manager with default ON behavior.
- [x] P1-3 Add optional SMAppService integration for login-item UX.
- [x] P1-4 Implement auto-launch toggle semantics (enable/disable + optional stop-now).
- [x] P1-5 Add crash-loop detection and exponential relaunch delay.

Exit gate:

- Daemon reliably starts at login and avoids restart-loop meltdown.

## Phase 1.5 - Native app bundle and distribution foundation

- [x] P1D-1 Convert `Vapor` executable target to a native SwiftUI app entry (`@main App`) with no CLI-style entrypoint conflicts.
- [x] P1D-2 Add initial native macOS window structure (`ContentView`) with toolbar/menu command so the app does not look skeletal.
- [x] P1D-3 Create script-first packaging entrypoint `apps/macos/scripts/package.sh`.
- [x] P1D-4 Implement deterministic `.app` bundle assembly in `dist/Vapor.app` (binary copy, bundle structure, executable permissions).
- [x] P1D-5 Generate required `Info.plist` metadata, including latest-only `LSMinimumSystemVersion=26.0` and deterministic version/build derivation.
- [x] P1D-6 Generate `AppIcon.icns` from a 1024x1024 PNG using built-in tooling (`sips`, `iconutil`) and install it into bundle resources (source artwork in `assets/icon.png`).
- [x] P1D-7 Add optional resource copy convention from `apps/macos/Resources/**` to `Contents/Resources`.
- [x] P1D-8 Implement signing modes in packaging script: ad-hoc default, Developer ID + hardened runtime when `VAPOR_SIGN_IDENTITY` is set, optional entitlements injection.
- [x] P1D-9 Add bundle verification steps (`plutil`, `codesign --verify`, `spctl` best-effort).
- [x] P1D-10 Produce zip artifact `dist/Vapor.zip` using `ditto --keepParent`.
- [x] P1D-11 Add optional notarization + stapling flow triggered when `VAPOR_NOTARY_PROFILE` is provided.
- [x] P1D-12 Integrate packaging into repo build scripts via `./scripts/build.sh package` while preserving build-only default behavior.
- [x] P1D-13 Add optional Xcode convenience workflow (open package/workspace for debugging) without changing script-first release source of truth.
- [x] P1D-14 Update `apps/macos/README.md` with local build/package/signed/notarized distribution commands.
- [x] P1D-15 Reintroduce daemon lifecycle bootstrap in a non-blocking startup path (background/async), preserving fast app launch while keeping default auto-launch semantics.
- [x] P1D-16 Implement close-window behavior so closing the main Vapor window removes Dock presence and leaves Vapor running as menubar-only.
- [x] P1D-17 Ensure window-close and daemon lifecycle are decoupled: closing UI window must not stop `vapord` or remove menubar status/control.
- [x] P1D-18 Add explicit menubar lifecycle controls for `Open Vapor` (reopen/focus main window) and `Quit Vapor` (request daemon stop then terminate app).
- [x] P1D-19 Add lifecycle coverage tests for: window close keeps daemon alive, reopen from menubar works, and menubar quit executes daemon stop path before app termination.
- [x] P1D-20 Update app lifecycle docs (`README.md`, `apps/macos/README.md`, `AGENTS.md`) to define app-window vs menubar vs daemon responsibilities.
- [x] P1D-21 Add packaging/lifecycle assertions that `dist/Vapor.app` always contains `Contents/MacOS/Vapor` and `Contents/MacOS/vapord`, and runtime daemon launch resolves the bundled sibling binary only.

Exit gate:

- `open dist/Vapor.app` launches a normal app window without Terminal.
- `apps/macos/scripts/package.sh` produces `dist/Vapor.app` and `dist/Vapor.zip`.
- CI can run the packaging path non-interactively.
- Closing the main window leaves menubar + daemon running, and quitting from menubar performs full shutdown semantics.
- `dist/Vapor.app` always embeds both `Contents/MacOS/Vapor` and `Contents/MacOS/vapord`, and runtime launch targets the bundled daemon path.

## Phase 2 - Low-impact local engine core + durability substrate

- [x] P2-1 Implement FSEvents recursive watcher with minimal callback work only.
- [x] P2-2 Implement default excludes + `.vaporignore` parser and matcher.
- [x] P2-2a Add optional `.gitignore` ingestion/matching in daemon local filtering (default enabled).
- [x] P2-2b Add app setting to toggle `.gitignore` usage (`useGitIgnore`, default `true`).
- [x] P2-2c Add user-level ignore rules configured via Vapor app UI and apply them in daemon filtering.
- [x] P2-2d Add tests for ignore precedence/merge across defaults, `.vaporignore`, `.gitignore`, and UI rules.
- [x] P2-2e Enforce sync scope strictly to configured `localSyncDirectory`/`cloudSyncDirectory` roots (including missing-root auto-create behavior) and never fall back to whole-device scanning.
- [x] P2-2f Add explicit safety tests that invalid local sync path and missing-root creation flows do not trigger any broad/root filesystem watch fallback.
- [x] P2-3 Implement bounded in-memory event/intent maps with deterministic caps and compaction/backpressure behavior.
- [x] P2-4 Implement debounce/coalescing loop (250ms tick, conservative windows).
- [x] P2-5 Implement keyed superseding scheduler (latest intent wins per path).
- [x] P2-6 Implement throttle controller inputs and 4-state model.
- [ ] P2-7 Gate planner/uploader worker caps strictly by throttle state.
- [ ] P2-8 Implement durable queue/state DB with at-least-once semantics.
- [ ] P2-9 Implement retries with exponential backoff + jitter + rate-limit-aware slowdown.
- [ ] P2-10 Implement storm detection triggers and deferred `RECONCILE_SUBTREE` scheduling.
- [ ] P2-11 Implement interruptible reconcile with idle-biased execution.
- [ ] P2-12 Add microbench/regression tests for FSEvents callback, debounce/coalescing loop, and scheduler superseding paths.
- [ ] P2-13 Add load/stress tests for memory/backpressure caps under large pending-intent storms.

Exit gate:

- Under synthetic load, CPU and I/O impact stay bounded while queue converges eventually.
- Under storm load, in-memory event/intent structures remain bounded and backpressure behavior is deterministic.

## Phase 3 - Google Drive provider plus bidirectional flow

- [ ] P3-1 Implement provider trait/capabilities and provider-neutral error taxonomy.
- [ ] P3-2 Implement `provider_gdrive` auth/refresh + remote root initialization.
- [ ] P3-2a Add authenticated Google Drive folder lookup/create for the configured `cloudSyncDirectory` before regular sync starts.
- [ ] P3-2b Block normal sync startup until the configured cloud root exists or the provider returns an actionable initialization error.
- [ ] P3-3 Implement upload paths (multipart small, resumable large).
- [ ] P3-4 Implement remote changes polling (low frequency, throttle-aware).
- [ ] P3-5 Implement remote-to-local apply pipeline using durable queue/state intents.
- [ ] P3-6 Implement self-write loop prevention (`self_write_cache`, op IDs, TTL rules).
- [ ] P3-7 Add adaptive remote polling cadence/request budgeting tied to throttle state and recent change rates.
- [ ] P3-8 Add provider metadata caching and resumable upload chunk auto-sizing to improve throughput without impact spikes.

Exit gate:

- Bidirectional Drive sync works end-to-end under normal conditions with durable recovery and throttle-safe provider behavior.

## Phase 4 - Bidirectional safety, conflicts, and deletion semantics

- [ ] P4-1 Implement bidirectional conflict policy (keep both copies; no silent overwrite).
- [ ] P4-2 Implement tombstone/delete reconciliation and restart-safe replay.
- [ ] P4-3 Implement deterministic race-resolution rules for simultaneous edits, rename+modify, and delete/restore paths.
- [ ] P4-4 Add bidirectional race integration tests for conflict handling, tombstones, and loop-prevention behavior.
- [ ] P4-5 Add corruption-recovery validation for queue/tombstone state under crash + restart.

Exit gate:

- No lost intent across restarts and deterministic behavior under conflict/delete/race scenarios.

## Phase 5 - Profile model, multi-provider accounts, and settings overrides

- [ ] P5-1 Define the durable profile model (`profile_id`, display name, provider kind, authenticated account identity, enabled state) and classify settings into app-global vs profile-override-capable.
- [ ] P5-2 Refactor persisted config/shared models to store app-global settings plus a profile list with explicit per-profile overrides, without pre-GA compatibility shims.
- [ ] P5-3 Namespace Keychain secrets, auth refresh state, and provider connection metadata by profile/account so multiple provider accounts can coexist safely.
- [ ] P5-4 Add app UI flows to create, rename, select, enable/disable, and delete profiles, and bind each profile to a provider plus authenticated account.
- [ ] P5-5 Implement profile-scoped override resolution for sync roots, ignore rules, and other sync-affecting settings while keeping global-only settings (for example `languageCode`) singular.
- [ ] P5-6 Support multiple enabled profiles concurrently, including same local root fan-out to multiple providers/accounts and different local roots to different profiles.
- [ ] P5-7 Make watcher routing, scheduler intents, durable queue/state, tombstones, and conflict handling profile-aware with no cross-profile leakage.
- [ ] P5-8 Deduplicate shared local-root watches and preserve low-impact budgets when multiple profiles point at the same directory.
- [ ] P5-9 Extend startup/runtime config loading and provisional app-daemon/shared contracts so enabled profiles bootstrap together and disabled profiles stay inactive.
- [ ] P5-10 Add safe profile disconnect/delete flows that remove only the targeted profile's auth/state and leave other profiles untouched.
- [ ] P5-11 Add integration/perf tests for override resolution, same-folder multi-provider sync, different-folder parallel sync, restart recovery, and multi-profile budget adherence.

Exit gate:

- User can create multiple named profiles, each bound to one provider plus authenticated account, and enable/disable them independently.
- Same local folder can fan out to multiple providers/accounts, and different folders can sync in parallel through different profiles.
- Global settings and profile-scoped overrides resolve predictably; settings that do not make sense per profile remain app-global.
- Credentials, queues, tombstones, conflicts, and failures stay isolated per profile, and deleting/disconnecting one profile does not affect others.
- Multi-profile runtime preserves exact sync-root safety and low-impact budgets, including shared-root watch deduplication and bounded state.

## Phase 6 - XPC contract and diagnostics UX

- [ ] P6-1 Finalize XPC schema for state, queue, auth, auto-launch, and reasons.
- [ ] P6-2 Add control endpoints (pause/resume, flush-now, toggle auto-launch, excludes).
- [ ] P6-3 Implement full menubar state model and reasoned status messages.
- [ ] P6-4 Implement diagnostics panel (throttle reason, queue depth, conflicts, failures).
- [ ] P6-5 Add daemon activity event stream (search/hash/upload and related work stages) to app diagnostics via XPC.
- [ ] P6-6 Implement a diagnostics timeline tab in Vapor app UI showing live daemon activity events (non-persistent across app relaunch).
- [ ] P6-7 Implement bounded in-memory timeline buffer with configurable max length (default `1000` events) and safe bounds.
- [ ] P6-8 Add tests for timeline ordering, truncation at max length, and UI/event-stream integration behavior.

Exit gate:

- User can understand "what is happening" and "why" without CLI access.
- User can inspect a live timeline of current daemon work (for example directory scanning, hashing, uploading).

## Phase 7 - Auto-tuning (impact-first)

- [ ] P7-1 Add bounded 60s metrics aggregation and persistence limits.
- [ ] P7-2 Implement tuning loop cadence (60-120s) with one small change per cycle.
- [ ] P7-3 Tune priority order: impact reduction, rate-limit avoidance, then latency.
- [ ] P7-4 Tune polling/debounce/concurrency/storm thresholds within safe bounds.
- [ ] P7-5 Add hysteresis/min-dwell guardrails and rollback-on-regression safety to avoid oscillation.
- [ ] P7-6 Bind tuning decisions to acceptance SLOs and freeze unsafe adjustments when budgets are violated.

Exit gate:

- Tuned behavior outperforms static defaults without oscillation or instability.

## Phase 8 - Provider-system extensibility hardening

- [ ] P8-1 Finalize provider capability model and trait boundaries so core engine behavior remains provider-neutral.
- [ ] P8-2 Add provider contract tests with a reference/mock provider to validate compatibility across provider semantics (for example iCloud, R2, S3, Proton Drive style constraints).
- [ ] P8-3 Add compatibility validation for bidirectional flows, conflicts, tombstones, retries, and throttle behavior through provider abstractions.
- [ ] P8-4 Add provider-adapter performance checks so abstraction overhead stays low and full-speed sync targets are preserved.
- [ ] P8-5 Document a provider-onboarding checklist and acceptance criteria for future provider implementations.

Exit gate:

- Engine is provider-ready (compatibility + performance validated) without shipping additional providers in first release.

## Phase 9 - Optional safeguards and advanced features

- [ ] P9-1 Add active-coding detection (permissioned) with heuristic fallback.
- [ ] P9-2 Add folder priority classes and temporary flush boost controls.
- [ ] P9-3 Add mass-change/ransomware guard with pause + alert workflow.
- [ ] P9-4 Add richer diagnostics history and support export bundle.

Exit gate:

- Optional features stay default-safe and do not violate low-impact guarantees.

## Cross-phase mandatory validation

- [ ] T-1 Crash/restart during active sync resumes without lost intent.
- [ ] T-2 Throttle transitions follow battery/thermal/load pressure correctly.
- [ ] T-3 FSEvents callback remains lightweight (no DB/hash/network).
- [ ] T-4 Self-write echo suppression works in bidirectional paths.
- [ ] T-5 Conflict policy verified on simultaneous local/remote edits.
- [ ] T-6 Security validation: Keychain-only secrets + redacted logs.
- [ ] T-7 Upgrade compatibility validation across app/daemon/schema versions.
- [ ] T-8 CI parity validation: pull-request lint and tests match local script entry points.
- [ ] T-9 Scope safety validation: daemon only watches configured local sync root and never escalates to full-device sync.
- [ ] T-10 Ignore-rule safety validation: enforce precedence and behavior for `preIgnoreRules` -> `.gitignore` -> `.vaporignore` -> `postIgnoreRules` so low-signal paths stay excluded and user overrides work predictably.
- [ ] T-11 Performance SLO validation: idle/load/storm/recovery benchmarks pass defined thresholds and configured CI gates.
- [ ] T-12 Memory/backpressure validation: bounded `event_map`/intent structures remain within defined caps under storm-scale workloads.
- [ ] T-13 Auto-tuning stability validation: throttle/tuning decisions avoid oscillation and rollback unsafe adjustments.
- [ ] T-14 Multi-profile isolation validation: per-profile overrides, credentials, durable state, sync intents, and failure surfaces do not cross-apply between profiles.

## Deferred onboarding task

- [ ] O-1 Design and implement the production onboarding flow (information architecture, step sequence, copy, and UX states). When this task starts, first run a clarification pass with the project owner to define the onboarding structure and decisions before implementation.

## Deferred performance tuning task

- [ ] PT-1 Tune `./scripts/perf.sh` smoke thresholds using real CI/release baseline history so the `perf` gate becomes stricter at catching regressions without becoming flaky; update the documented defaults and release-gate policy in the same change set.
