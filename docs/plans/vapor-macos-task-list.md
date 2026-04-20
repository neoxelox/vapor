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
- [x] P2-7 Gate planner/uploader worker caps strictly by throttle state.
- [x] P2-8 Implement durable queue/state DB with at-least-once semantics.
- [x] P2-9 Implement retries with exponential backoff + jitter + rate-limit-aware slowdown.
- [x] P2-10 Implement storm detection triggers and deferred `RECONCILE_SUBTREE` scheduling.
- [x] P2-11 Implement interruptible reconcile with idle-biased execution.
- [x] P2-12 Add microbench/regression tests for FSEvents callback, debounce/coalescing loop, and scheduler superseding paths.
- [x] P2-13 Add load/stress tests for memory/backpressure caps under large pending-intent storms.

Exit gate:

- Under synthetic load, CPU and I/O impact stay bounded while queue converges eventually.
- Under storm load, in-memory event/intent structures remain bounded and backpressure behavior is deterministic.

## Phase 2.5 - Runtime integration and hardening pass

- [x] P2.5-1 Compose the real daemon runtime loop end-to-end: watcher -> bounded ingest -> debounce -> scheduler -> durable queue -> workgate -> reconcile.
- [x] P2.5-2 Make compacted/deferred/scheduled intent state durable before execution, or add a documented whole-scope restart reconstruction path that preserves intent safely after crash/restart.
- [x] P2.5-3 Harden callback path scope enforcement by normalizing event paths and rejecting traversal/symlink escape cases outside the configured local sync root.
- [x] P2.5-4 Replace destructive config-load fallback with preserved-invalid-config recovery and actionable app diagnostics instead of silently rewriting defaults.
- [x] P2.5-5 Refresh daemon lifecycle/launch configuration immediately when ignore toggles change so in-memory runtime settings never diverge from persisted config in-session.
- [x] P2.5-6 Replace placeholder app controls (`Pause/Resume`, `Flush now`, demo status cycling) with real daemon-backed behavior, or hide them until the control plane exists.
- [x] P2.5-7 Make runtime path/logging behavior fail-safe and privacy-safe: validated `VAPOR_DIR`, restrictive permissions for config/log/state artifacts, centralized redaction, and no panic on log-file open failure.
- [x] P2.5-8 Bound and sanitize durable diagnostic/state fields (`last_error`, counters, persisted timestamps) and add corruption/tamper guards for malformed local state.
- [x] P2.5-9 Remove pre-GA compatibility shims and transitional APIs that are no longer justified (for example legacy schema migration paths, duplicate deferred-intent helpers, and placeholder app state surfaces).
- [x] P2.5-10 Decouple core daemon orchestration from the concrete `GoogleDriveProvider` type so provider choice is injected at startup and core engine code stays provider-neutral before provider-phase expansion.
- [x] P2.5-11 Introduce bounded staged-admission/executor scaffolding under workgate/throttle caps so later real planner/hash/upload workers have a safe runtime shell.
- [x] P2.5-12 Reduce known serialized hot spots with batched durable leasing and ready-queue/indexed dispatch for debounce/scheduler paths, then add runtime-level regression coverage for the composed engine.

Current note: the composed runtime loop is now real, but its production tick path still feeds default `ThrottleInputs` until later hardening work replaces that placeholder with real system-driven pressure sampling.
Current note: bounded staged admission and batched/indexed dispatch now exist, but the current timed stage simulator intentionally remains until provider-backed upload execution is available in Phase 3.

Exit gate:

- The local engine runs as one real daemon pipeline instead of only unit-tested primitives.
- No path outside the configured local sync root can enter callback state, including relative traversal and symlink-escape cases.
- Malformed/unreadable config is preserved in place and surfaced for recovery; Vapor does not silently reset user intent to defaults.
- Compacted/deferred/scheduled work survives crash/restart without silent loss, either through earlier durability or deterministic whole-scope recovery.
- Logging/runtime-path handling degrades safely, uses restrictive local permissions, and avoids leaking sensitive values in durable logs/state.
- App controls shown to users are real daemon-backed controls, not placeholder/demo state transitions.
- Core daemon orchestration is provider-neutral, pre-GA backcompat shims are removed, and the runtime keeps staged admission/dispatch bounded under explicit caps.

## Phase 3 - Local filesystem provider and bidirectional runtime shell

Introduces a loopback filesystem provider whose "remote" side is a second local directory. The phase has three purposes:

1. Replace the Phase 2.5 timed staged executor simulator with real provider-backed planner/hash/upload/download execution so the runtime is exercised end-to-end without any external dependency.
2. Implement every provider-neutral bidirectional mechanic the core engine needs (provider-neutral error taxonomy, op-id correlation, self-write cache, remote-to-local apply pipeline, durable provider cursor) against a deterministic reproducible backing store before any external cloud provider is introduced.
3. Serve as the reference provider for Phase 8 (provider-system extensibility hardening) and the default integration-test harness for Phases 4 through 8.

Scope boundary with Phase 4: this phase must make bidirectional flow work on the happy path and must not infinite-loop or lose data. Full race and conflict hardening (simultaneous edits, rename+modify, delete/restore, tombstone corruption recovery) belongs to Phase 4 and is validated against this provider.

- [ ] P3-1 Define the provider trait surface in `core/providers` and the provider-neutral error taxonomy in `core/shared`. The trait must expose `enumerate(prefix)`, `stat(remote_path)`, `upload(local_path, remote_path, op_id)`, `download(remote_path, local_path, op_id)`, `delete(remote_path, op_id)`, `rename(remote_old, remote_new, op_id)`, and a changes-feed producer gated by `ProviderCapabilities::supports_remote_changes_feed`. The error taxonomy must classify failures as `Transient`, `RateLimited`, `Authentication`, `PreconditionFailed`, `NotFound`, and `Permanent`; provider-specific types must not leak into `core/daemon` or `core/shared`.
- [ ] P3-2 Add a filesystem-provider configuration surface. Persist the selected provider kind in `vapor.json` as a new `provider` field (default `filesystem` pre-GA; `google_drive` accepted but inert until Phase 9). When `provider = "filesystem"`, interpret `cloudSyncDirectory` as a local absolute path (the filesystem provider's remote root). Document this reinterpretation in `docs/architecture/system-overview.md` and the root `README.md` **Configuration** section, and mirror the new field in `apps/macos/Sources/VaporCore/VaporConstants.swift` and `core/shared/src/constants.rs`.
- [ ] P3-3 Implement `core/providers/src/filesystem/` as a full `Provider` implementation backed by the local filesystem:
  - Atomic writes via temp-file-plus-rename within the remote root; op-id tagging via extended attributes when supported, with a side-file fallback.
  - `enumerate` streams entries with size, mtime, and lazy content-hash.
  - `stat` returns canonical metadata; content-hash is optional and computed on demand.
  - Errors map deterministically onto the P3-1 error taxonomy.
  - Strict scope enforcement: the provider refuses operations outside its configured remote root, including symlink escape, relative traversal, and device crossing.
- [ ] P3-4 Implement a filesystem-backed remote changes feed for the provider. Use FSEvents on the configured remote root with the same low-impact callback discipline as the local watcher and emit `ProviderChangeEvent` items with a monotonic cursor/sequence. The feed must persist the last-applied cursor in the durable state DB and resume from it on restart without re-delivering already-applied events.
- [ ] P3-5 Replace the Phase 2.5 timed staged executor with real planner/hash/upload/download workers driven by the provider and bounded by workgate/throttle caps. Keep the existing `WorkClass` topology (Planner, Hash, Upload, Reconcile) and add `WorkClass::Download`; preserve slice-budget interruptibility so `ThrottleState::Suspended` halts new stage admission while running work either completes cleanly or yields at the next checkpoint.
- [ ] P3-6 Implement the remote-to-local apply pipeline. Provider changes flow into `PendingIntentRecord` entries tagged `IntentSource::Remote`, the provider cursor is carried through durable state, and the pipeline is planner-stage (stat + compare) → download-stage (atomic write, mtime preservation, op-id tagging) → completion. Persist the cursor advance only on durable intent completion so a crash never causes remote events to be skipped.
- [ ] P3-7 Implement self-write loop prevention (`self_write_cache`). On every provider write (upload, download, delete, rename) the daemon records `(remote_path, op_id, content_hash, expiry)` in a time-bounded and size-bounded in-memory cache. Incoming provider change events are matched against the cache and hits are suppressed before they become intents. Primary correlator is the provider's op-id tag (xattr or side-file); content-hash is the fallback. Default TTL and bound are defined in `core/shared/src/constants.rs`; the eviction policy is documented in `docs/architecture/data-flow.md`.
- [ ] P3-8 Apply ensure-remote-root semantics to the filesystem provider. If the configured remote root is missing, create it before regular sync work proceeds (mirroring P2-2e local-root behavior). If the remote root is invalid (not a directory, no permission, escapes a safe area) the daemon must refuse to start normal sync and must surface an actionable configuration error the app can render.
- [ ] P3-9 Wire provider selection into the daemon entrypoint (`core/daemon/src/main.rs`). Accept the provider kind from resolved configuration/environment, default to `filesystem` pre-GA, validate at startup, and fail fast with a classified configuration error on invalid input. Keep `GoogleDriveProvider` compiled in as an inert option so the trait surface and capability model remain visible and compilable, but do not allow the daemon to run against it until Phase 9.
- [ ] P3-10 Add integration tests that drive the composed daemon through the filesystem provider. Minimum coverage: local→remote propagation with real hashing and real provider writes; remote→local propagation through the provider changes feed; self-write loop prevention verified by asserting no echo-upload occurs after a local write is applied remotely; restart recovery with in-flight uploads and downloads with no intent loss and no duplicated writes; throttle transitions during real work (Suspended halts new stage admission; running work completes or yields cleanly); scope safety (the filesystem provider refuses to touch paths outside its remote root under symlink-escape and traversal inputs).
- [ ] P3-11 Add microbench/regression coverage for provider-backed execution. A 10,000-file fixture synced end-to-end through the filesystem provider must stay within defined engine budgets; stage admission under burst must not serialize into a hot spot; remote-to-local apply cost must scale linearly with changed-file count.
- [ ] P3-12 Update `docs/architecture/system-overview.md`, `docs/architecture/data-flow.md`, `docs/plans/vapor-macos-plan.md`, the root `README.md` **Configuration** section, and the `CHANGELOG.md` `Unreleased` section to describe the filesystem provider as the pre-GA default, document the `provider` config field and the `cloudSyncDirectory` reinterpretation, and state that Phases 4 through 8 are validated against this provider before Phase 9 introduces Google Drive.

Exit gate:

- The daemon runs a complete bidirectional sync loop end-to-end against the filesystem provider, with no simulator in the hot path.
- Every provider-neutral bidirectional mechanic required for correctness (self-write cache, remote-to-local apply, provider-neutral error taxonomy, op-id correlation, durable provider cursor) is implemented and exercised by integration tests.
- The provider trait surface is provider-neutral and ready to accept Google Drive in Phase 9 without any core-engine changes.
- The Phase 2.5 timed staged executor simulator is removed from the production runtime.

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

## Phase 9 - Google Drive provider

Integrates Google Drive as the first external cloud provider on top of the provider-neutral runtime already validated against the Phase 3 filesystem provider. This phase introduces no new bidirectional mechanics; those were finalized in Phases 3 and 4. It is deliberately deferred until Phases 4 through 8 stabilize the engine so Google Drive integration does not overlap with runtime or provider-abstraction evolution.

- [ ] P9-1 Implement `provider_gdrive` OAuth (PKCE) auth, Keychain-backed token storage, and refresh handling per `docs/operations/provider-auth-operations.md`; degraded-auth behavior must follow the runbook.
- [ ] P9-2 Add authenticated Google Drive folder lookup/create for the configured `cloudSyncDirectory` before regular sync starts.
- [ ] P9-3 Block normal sync startup until the Google Drive cloud root exists or the provider returns an actionable initialization error.
- [ ] P9-4 Implement upload paths (multipart small, resumable large) behind the provider trait, with chunked retry and rate-limit awareness mapped onto the Phase 3 error taxonomy.
- [ ] P9-5 Implement remote changes polling using the Google Drive changes endpoint (low frequency, throttle-aware), emitting `ProviderChangeEvent` items through the changes-feed producer already consumed by the daemon since Phase 3.
- [ ] P9-6 Add adaptive remote polling cadence and request budgeting tied to throttle state and recent change rates.
- [ ] P9-7 Add provider metadata caching and resumable upload chunk auto-sizing to improve throughput without impact spikes.
- [ ] P9-8 Flip `GoogleDriveProvider` from inert to selectable (via the `provider` config field introduced in P3-2), gated on successful provider contract test runs from Phase 8.

Exit gate:

- Bidirectional Google Drive sync works end-to-end on the runtime validated in Phases 3 through 8, with no Google-Drive-specific regressions to scope safety, durability, throttle behavior, or low-impact guarantees.

## Phase 10 - Optional safeguards and advanced features

- [ ] P10-1 Add active-coding detection (permissioned) with heuristic fallback.
- [ ] P10-2 Add folder priority classes and temporary flush boost controls.
- [ ] P10-3 Add mass-change/ransomware guard with pause + alert workflow.
- [ ] P10-4 Add richer diagnostics history and support export bundle.

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
