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
- [ ] P1-6 Validate LaunchAgent plist and crash-loop interaction per `docs/operations/launchagent-policy.md`: plist audit (expected keys exactly, `KeepAlive=false`), SIGKILL scenario (daemon killed with `kill -9`, no auto-restart for 30s, next user action restarts via coordinator not `launchd`), crash-loop pause scenario (5 crashes in 10 minutes reach `CrashLoopPaused` with reasoned menubar surface), clean shutdown scenario (menubar-quit exits cleanly and `launchd` stays passive until next login or explicit app launch).

Exit gate:

- Daemon reliably starts at login and avoids restart-loop meltdown.
- LaunchAgent and in-process crash-loop protection do not collide, and P1-6 scenarios pass under automated test.

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
- [ ] P3-7 Implement self-write loop prevention (`self_write_cache`) per `docs/architecture/data-flow.md` §"Loop prevention". The `self_write_cache` constants module already exists in `core/shared/src/constants.rs` with `DEFAULT_TTL_MILLIS: u64 = 30_000`, `MIN_TTL_MILLIS: u64 = 5_000`, `MAX_ENTRIES: usize = 10_000`, `MIN_ENTRIES: usize = 1_000`; this task implements the runtime cache that consumes those constants. On every provider write (upload, download, delete, rename) record `(remote_path, op_id, content_hash, expiry_monotonic_ms)` into an LRU-on-insert cache. Match inbound provider change events first by op-id (xattr primary; side-file `{path}.vapor-meta.json` fallback on `ENOTSUP`/`EACCES`/`EROFS`), then by content-hash. Hits suppress intent creation and emit a diagnostic. Under memory pressure TTL may shorten toward `MIN_TTL_MILLIS` and entries trim toward `MIN_ENTRIES` but never below. The filesystem provider must hide side-files from enumeration so they do not surface as independent intents.
- [ ] P3-8 Apply ensure-remote-root semantics to the filesystem provider. If the configured remote root is missing, create it before regular sync work proceeds (mirroring P2-2e local-root behavior). If the remote root is invalid (not a directory, no permission, escapes a safe area) the daemon must refuse to start normal sync and must surface an actionable configuration error the app can render.
- [ ] P3-9 Wire provider selection into the daemon entrypoint (`core/daemon/src/main.rs`). Accept the provider kind from resolved configuration/environment, default to `filesystem` pre-GA, validate at startup, and fail fast with a classified configuration error on invalid input. Keep `GoogleDriveProvider` compiled in as an inert option so the trait surface and capability model remain visible and compilable, but do not allow the daemon to run against it until Phase 9.
- [ ] P3-10 Add integration tests that drive the composed daemon through the filesystem provider. Minimum coverage: local→remote propagation with real hashing and real provider writes; remote→local propagation through the provider changes feed; self-write loop prevention verified by asserting no echo-upload occurs after a local write is applied remotely; restart recovery with in-flight uploads and downloads with no intent loss and no duplicated writes; throttle transitions during real work (Suspended halts new stage admission; running work completes or yields cleanly); scope safety (the filesystem provider refuses to touch paths outside its remote root under symlink-escape and traversal inputs).
- [ ] P3-11 Add microbench/regression coverage for provider-backed execution. A 10,000-file fixture synced end-to-end through the filesystem provider must stay within defined engine budgets; stage admission under burst must not serialize into a hot spot; remote-to-local apply cost must scale linearly with changed-file count.
- [ ] P3-12 Update `docs/architecture/system-overview.md`, `docs/architecture/data-flow.md`, `docs/plans/vapor-macos-plan.md`, the root `README.md` **Configuration** section, and the `CHANGELOG.md` `Unreleased` section to describe the filesystem provider as the pre-GA default, document the `provider` config field and the `cloudSyncDirectory` reinterpretation, and state that Phases 4 through 8 are validated against this provider before Phase 9 introduces Google Drive.
- [ ] P3-13 Phase 2.5 simulator removal validation (blocks Phase 4). Assert the following before Phase 4 opens: (a) `core/daemon/src/executor.rs` contains no `stage_duration` placeholders or timed-stage simulation logic, (b) every `WorkClass` admission path does real work via the provider trait (Planner: stat+compare; Hash: read+digest; Upload: provider upload; Download: provider download; Delete/Rename: provider delete/rename), (c) no production code path calls a simulator-shaped function, (d) no test fixtures use fake staged-executor timing behavior, (e) Phase 2.5 integration tests pass unmodified against the real executor, (f) benchmark runs show no placeholder-timing artifacts (no sub-1ms stage-completion outliers from simulated sleep).
- [ ] P3-14 Happy-path bidirectional race smoke test. Run integration tests with concurrent local and remote writes into the filesystem provider (two processes writing into the local sync root and into the configured remote root simultaneously during a single sync cycle). Assert: (a) no data loss — every committed write either lands at the canonical path or is preserved at a temporary `~conflict-pending-{intent_id}` path that Phase 4 (P4-1) later renames to the canonical conflict-suffix template, (b) no infinite loops — the sync converges within `30s`, (c) outcome determinism across 5 runs under identical inputs. The minimal `~conflict-pending-{intent_id}` scaffolding is a Phase 3 deliverable so Phase 3 can verify "no fundamental safety holes" without depending on Phase 4 conflict-policy work; the full canonical conflict-suffix template, deviceId derivation, and "data preservation wins over deletion" rule remain Phase 4 (P4-1). The full race/conflict matrix (simultaneous edits, rename+modify, delete/restore, tombstone corruption) remains Phase 4 (P4-3, P4-4).

Exit gate:

- The daemon runs a complete bidirectional sync loop end-to-end against the filesystem provider, with no simulator in the hot path.
- Every provider-neutral bidirectional mechanic required for correctness (self-write cache, remote-to-local apply, provider-neutral error taxonomy, op-id correlation, durable provider cursor) is implemented and exercised by integration tests.
- The provider trait surface is provider-neutral and ready to accept Google Drive in Phase 9 without any core-engine changes.
- The Phase 2.5 timed staged executor simulator is removed from the production runtime, verified by P3-13.
- Happy-path bidirectional smoke test (P3-14) demonstrates no fundamental safety holes before Phase 4 opens.

## Phase 4 - Bidirectional safety, conflicts, and deletion semantics

- [ ] P4-1 Implement bidirectional conflict policy (keep both copies; no silent overwrite) per `docs/architecture/data-flow.md` §"Conflict handling". Suffix template is `{stem}~conflict-{device_id}-{timestamp_ms}{ext}` with `-{seq}` collision-avoidance fallback. `timestamp_ms` is UTC monotonic-within-device. The winner retains the canonical path; the loser is renamed in place. "Data preservation wins over deletion": if one side deletes while the other modifies, the modification wins at the canonical path and the delete is finalized as a completed tombstone without a conflict suffix. The implementer must replace the Phase 3 `~conflict-pending-{intent_id}` scaffolding (P3-14) with this canonical template, including renaming any pre-existing pending conflict files on first run after upgrade.
- [ ] P4-1a Add the `deviceId: String` field to `VaporConfiguration` (Swift) and the equivalent durable state field (Rust) so the conflict suffix template has a stable identifier. On first run, derive it from `gethostname()` normalized to `[a-z0-9-]` (length-capped at 32; UUIDv4-truncated-to-12 fallback if `gethostname()` returns empty after normalization). Persist immediately to `vapor.json`; never silently regenerate, even if the machine hostname changes. Mirror the constant default name in `apps/macos/Sources/VaporCore/VaporConstants.swift` and `core/shared/src/constants.rs`, document the field in the root `README.md` **Configuration** table, and cover it with a unit test that asserts deviceId stability across config save/load cycles and across simulated hostname changes.
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
- [ ] P5-5 Implement profile-scoped override resolution for sync roots, ignore rules, and other sync-affecting settings while keeping global-only settings (for example `languageCode`) singular. The override-resolution mechanism built here is reused by Phase 7 (P7-9a) to layer `resourceLimits` and `idleBoost` once those config groups exist.
- [ ] P5-6 Support multiple enabled profiles concurrently, including same local root fan-out to multiple providers/accounts and different local roots to different profiles.
- [ ] P5-7 Make watcher routing, scheduler intents, durable queue/state, tombstones, and conflict handling profile-aware with no cross-profile leakage per `docs/architecture/data-flow.md` §"Multi-profile watch coordination". Per-profile debounce, scheduler, and durable-queue tables (profile-id-keyed in SQLite). Workgate, throttle controller, bandwidth shaper, and effective resource ceilings remain daemon-level; profile work queues behind shared caps. Profile runtimes are spawned in panic-catching tasks so a panic in one profile marks it `Failed` with a durable diagnostic and suspends only its queue, without killing the watcher or other profiles. Provider op-ids carry the originating `profile_id` so self-write-cache matches stay correctly scoped when two profiles share a provider account + remote subtree.
- [ ] P5-8 Deduplicate shared local-root watches and preserve low-impact budgets when multiple profiles point at the same directory. One FSEvents watcher per distinct canonical realpath; per-callback fan-out dispatches event copies into each matching profile's bounded ingest queue tagged with `profile_id`. Non-matching profiles do not see the event. Profile startup must compute canonical realpaths and reuse watchers across profiles sharing a root.
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

- [ ] P6-1 Finalize XPC schema for state, queue, auth, auto-launch, and reasons per `docs/architecture/xpc-contracts.md`. Every payload carries top-level `schema_version: u32` and a declared `payload_bytes` hint bounded by `XPC_MAX_PAYLOAD_BYTES` (new constant in `core/shared/src/constants.rs`, default `4 * 1024 * 1024`). On connect, the app sends `Hello { app_schema_version }` and the daemon responds `HelloAck { daemon_schema_version, supported_min_version }`; mismatches beyond `|N - M| <= 1` return `IncompatibleVersion` and both sides log `xpc.handshake.incompatible` with both versions. Unknown fields on the receiving side default to type zero values and are logged at debug level (`xpc.unknown_field`). Test matrix: `app-N ↔ daemon-N`, `app-N ↔ daemon-(N-1)`, `app-(N-1) ↔ daemon-N`, `|N - M| = 2` negative case; plus field-omission, unknown-field, and payload-bounds tests.
- [ ] P6-2 Add control endpoints (pause/resume, flush-now, toggle auto-launch, excludes).
- [ ] P6-3 Implement full menubar state model and reasoned status messages.
- [ ] P6-4 Implement diagnostics panel (throttle reason, queue depth, conflicts, failures).
- [ ] P6-4a Implement per-intent "why stuck" diagnostics per `docs/architecture/xpc-contracts.md` §"Diagnostics (per-intent)". For each pending durable intent expose `intent_id`, `profile_id`, `path`, `action`, current `stage` (matching `executor.rs::ExecutionStage` plus the queue-state values listed in `xpc-contracts.md`: `Queued`, `Planner`, `WaitingForHash`, `Hash`, `WaitingForUpload`, `Upload`, `WaitingForDownload`, `Download`, `Retrying`, `DeferredReconcile`), elapsed-in-stage, attempt count, last-error classification, and a human-readable `blocker_reason` (e.g., "Throttle Suspended: no uploads allowed", "Hash worker cap 2/2 in use", "Rate-limit slowdown until {iso8601}"). Render in the diagnostics panel with a filterable list and per-row drill-down. Surface `BoundedFsEventRecorder::dropped_incoming_event_count` as a top-level diagnostics metric so users can detect prolonged callback-vs-runtime backpressure. Test: queue 10 intents under `ThrottleState::Suspended`; assert each exposes the Suspended reason.
- [ ] P6-5 Add daemon activity event stream (search/hash/upload and related work stages) to app diagnostics via XPC.
- [ ] P6-6 Implement a diagnostics timeline tab in Vapor app UI showing live daemon activity events (non-persistent across app relaunch).
- [ ] P6-7 Implement bounded in-memory timeline buffer with configurable max length (default `1000` events) and safe bounds.
- [ ] P6-8 Add tests for timeline ordering, truncation at max length, and UI/event-stream integration behavior.

Exit gate:

- User can understand "what is happening" and "why" without CLI access.
- User can inspect a live timeline of current daemon work (for example directory scanning, hashing, uploading).

## Phase 7 - Auto-tuning and user resource-budget enforcement

Adds two layers on top of the throttle controller already stabilized in earlier phases:

1. Auto-tuning: bounded internal optimization of debounce/concurrency/polling thresholds within safe ranges, driven by local metrics, always bounded by the user-configured ceilings introduced below.
2. User resource budgets: explicit user-facing ceilings on daemon-process CPU, memory, and network bandwidth usage, plus an opt-in idle-boost mechanism that dynamically raises those ceilings when the device is demonstrably idle and has genuinely unused resources. Ceilings are hard caps: the throttle controller and auto-tuner must never push the daemon above them. User ceilings never relax the internal throttle controller — if the controller says `Suspended`, the daemon still suspends regardless of configured ceilings.

### Auto-tuning

- [ ] P7-1 Add bounded 60s metrics aggregation and persistence limits.
- [ ] P7-2 Implement tuning loop cadence (60-120s) with one small change per cycle.
- [ ] P7-3 Tune priority order: impact reduction, rate-limit avoidance, then latency.
- [ ] P7-4 Tune polling/debounce/concurrency/storm thresholds within safe bounds.
- [ ] P7-5 Add hysteresis/min-dwell guardrails and rollback-on-regression safety to avoid oscillation.
- [ ] P7-6 Bind tuning decisions to acceptance SLOs and freeze unsafe adjustments when budgets are violated.
- [ ] P7-7 Bind auto-tuning decisions to the effective user resource ceilings resolved below. The tuner must never select concurrency, polling cadence, or bandwidth usage that would exceed the current effective ceiling; ceiling changes (from config edits or idle-boost transitions) must take effect within one tuning cycle without oscillation.

### User resource-budget config surface

- [ ] P7-8 Add the `resourceLimits` config group to `vapor.json`, `core/shared/src/constants.rs`, and `apps/macos/Sources/VaporCore/VaporConstants.swift`. Fields: `cpuPercent` (default `15`, range `1..100`), `memoryPercent` (default `10`, range `1..100`), `bandwidthPercent` (default `25`, range `1..100`). Semantics: each field is the maximum share of the corresponding device resource the daemon process may consume under non-boost conditions. `cpuPercent` is expressed against a single logical core (so `100` means "up to one full core"); `memoryPercent` is against total device physical RAM; `bandwidthPercent` is an absolute cap against a rolling estimate of measured link capacity (NOT a relative share of free bandwidth): Vapor's combined upload+download throughput may not exceed this fraction of link capacity, so non-Vapor traffic always retains at least `100 - bandwidthPercent` of the link by construction and everyday activities like browsing and streaming stay unaffected. Invalid values must clamp to the documented range and surface a classified configuration warning to the app.
- [ ] P7-9 Add the `idleBoost` config group with fields: `enabled` (default `true`), `minIdleSeconds` (default `600`), `headroomCpuPercent`/`headroomMemoryPercent`/`headroomBandwidthPercent` (defaults `40`/`40`/`40`), `boostCpuPercent`/`boostMemoryPercent`/`boostBandwidthPercent` (defaults `50`/`30`/`90`), `rampUpSeconds` (default `60`), `rampDownSeconds` (default `20`). Semantics: when `enabled` is true, the device has been user-idle (no HID input and no app foreground change) for at least `minIdleSeconds`, non-Vapor CPU/memory/network utilization are each at or below the respective `headroom*Percent` value, and throttle state is `IdleDrain`, the effective ceiling is linearly ramped from `resourceLimits.*Percent` toward `boost*Percent` over `rampUpSeconds`. When any condition breaks, the ceiling ramps back to `resourceLimits.*Percent` over `rampDownSeconds` (the down-ramp must always be shorter than or equal to the up-ramp so activity resumption is non-invasive). Each `boost*Percent` must be greater than or equal to the corresponding `resourceLimits.*Percent`; lower values are treated as equal to the base ceiling (no boost). Add a config-load validation that rejects (or clamps with a classified warning) any `boost*Percent < resourceLimits.*Percent` so the invariant is enforced at the boundary, not just by runtime behavior.
- [ ] P7-9a Extend the Phase 5 layered-override resolution to cover the `resourceLimits` and `idleBoost` groups introduced above. Because the daemon is a single process, effective daemon-level caps resolve by taking the MIN across global value and every enabled profile's override (only *lowering* is allowed; higher per-profile values have no effect). For `idleBoost.enabled`, any enabled profile setting `false` disables boost daemon-wide. Document the reduction semantics in `docs/architecture/data-flow.md` and cover it with a profile-resolution unit test matrix. (Moved here from Phase 5 P5-5a because the override mechanism cannot extend groups that do not yet exist; Phase 5's `P5-5` builds the generic override mechanism, this task wires the resource-budget groups into it.)
- [ ] P7-10 Document both groups in the root `README.md` **Configuration** table (keys, defaults, ranges, one-line semantics) and in `docs/architecture/data-flow.md` under a new "User resource budgets" section that captures the layering relationship between throttle state, auto-tuner, user ceilings, and idle boost.

### Enforcement plumbing

- [ ] P7-11 Add a `ResourceBudget` runtime component in `core/daemon` that (a) resolves effective ceilings every tick from global config plus any enabled profile overrides using MIN-lowering semantics from P5-5a, (b) samples device-level CPU/memory/network utilization and user-idle state once per second (same cadence as the throttle controller), (c) runs the idle-boost state machine, and (d) publishes the current effective ceilings and reason codes to the workgate, provider I/O shaper, memory compaction paths, and diagnostics.
- [ ] P7-12 Extend the workgate to consume `ResourceBudget` effective ceilings: planner/hash/upload/download concurrency caps are the MIN of the throttle-state worker caps and the CPU-ceiling-derived cap (cpu_percent / 100 scaled against idle-drain baseline concurrency). When the effective CPU ceiling drops below the current in-flight concurrency, no new work is admitted but running work is allowed to reach its next slice checkpoint. Add unit tests for the admission-and-yield matrix across all throttle states and boost on/off conditions.
- [ ] P7-13 Add a provider-neutral bandwidth shaper in `core/providers` (bytes-per-second token bucket with burst-aware refill) applied uniformly to upload and download paths. The shaper's rate is driven by the effective `bandwidthPercent` ceiling against a rolling link-capacity estimate maintained in `core/daemon`. The shaper is a read-only dependency for the provider trait; provider implementations must call a single acquire-permit-for-bytes API rather than implementing their own pacing. Add tests that verify effective bytes/sec stays within tolerance under sustained and bursty traffic.
- [ ] P7-14 Add memory-ceiling enforcement by making existing bounded caches and pending-intent maps react to `ResourceBudget` pressure: when the daemon process RSS crosses the effective memory ceiling, storm compaction thresholds are lowered, `self_write_cache` TTL is reduced, and timeline/diagnostics buffers trim oldest-first. All knob movements are bounded and hysteresis-guarded to avoid flapping; document the knobs and their floors in `docs/architecture/data-flow.md`.
- [ ] P7-15 Expose effective ceilings, current utilization, idle-boost state, and the human-readable reason for the active boost/no-boost decision through the XPC diagnostics channel (Phase 6 surface) and the app diagnostics panel. The UI must render: the three current caps, the three current utilizations, whether boost is active (and if not, why), and the user's configured values with a clear distinction between global and effective (post-profile-MIN) values.
- [ ] P7-16 Add integration tests that drive full runtime loops against the filesystem reference provider (Phase 3) under representative scenarios per `docs/architecture/data-flow.md` §"Ceiling transitions" and `docs/performance/acceptance-budgets-and-benchmark-harness.md` §"SLO applicability under user resource ceilings": (a) steady active load with caps at defaults, (b) user-idle transition into boost and back out on HID input, (c) profile-override lowering of caps, (d) `idleBoost.enabled = false` disabling boost daemon-wide even with other profiles enabling it, (e) config reload mid-work without losing in-flight intents, (f) throttle-controller `Suspended` override of a boosted ceiling, (g) `IdleDrain → Suspended → IdleDrain` round-trip while boost is active — assert snap-down on exit, no boost auto-resume on return, fresh up-ramp restart from base ceiling, and no ceiling overshoot at any tick, (h) config reload mid-ramp lowering `resourceLimits.*Percent` clamps the current ramped value immediately. Run SLO-1 and SLO-2 thresholds under `resourceLimits.cpuPercent = 5` and assert the same thresholds still hold. Cover the cross-product `{default, cpuPercent=5, memoryPercent=5}` x `{idle, active, storm}` x `{boost-enabled, boost-disabled}` x `{global-only, profile-override-lowered}`.

Exit gate:

- User-configurable `resourceLimits` and `idleBoost` exist end-to-end: persisted in `vapor.json`, layered via profile overrides with MIN-lowering semantics, enforced at workgate/bandwidth-shaper/memory-compaction layers, and surfaced in diagnostics.
- Auto-tuning decisions respect effective ceilings in all throttle states and recover from ceiling changes within one tuning cycle without oscillation.
- Idle-boost engages only when the device is genuinely idle with headroom, ramps up slowly and down quickly, and never preempts `Suspended`.
- Integration tests cover the cross-product of throttle state, boost on/off, profile overrides, and config reload without intent loss.

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

## Deferred correctness tasks

- [ ] D-1 Harden `ThrottleWorkgate` permit-ID allocation against `u64::MAX` saturation. Today `next_permit_id = saturating_add(1)` stalls at `u64::MAX`; after that every new permit gets the same id and `release` silently leaks active counts on the second holder. The failure is astronomical (~1.8e19 permits, centuries at any realistic acquire rate) but the failure mode is silent, so switch to wrapping allocation with reuse of freed ids (e.g., a free-list of released ids, or wrap when the active-permits map shows the slot is free) and add a stress test that walks past the boundary in-process.
- [ ] D-2 Migrate local elapsed-time measurements in the daemon runtime from `SystemTime` to `Instant` so clock rewind cannot impact tick cadence, slice budgets, throttle sampling, or staged-executor timing at all (today the conservative `Err` arm papers over the symptom but still lets a rewound wall-clock prematurely trigger `SliceBudgetExpired`, extra throttle samples, and one debounce wake). This refactor requires a test-injectable clock abstraction on `DebounceLoop`, `ReconcileController`, `DaemonRuntime`, and `StagedExecutor`; keep `SystemTime` for persisted/durable fields (`DurableIntentRecord::available_at`, `DeferredReconcileRecord::available_at`, event observed-at) because those cross process boundaries.
- [ ] D-3 Add hysteresis and min-dwell to the throttle controller itself, not just to the auto-tuner (Phase 7 P7-5 handles tuner hysteresis but `ThrottleController::evaluate` is currently stateless and will flap state when input metrics oscillate near a threshold). Fold a small history buffer into `ThrottleController`, add a min-dwell-per-state setting (suggest 5s for `Light`/`Throttled` and 1s for `Suspended` since it is a safety state), and add a regression test that drives oscillating CPU samples and asserts state flips do not exceed once per `MIN_DWELL_*_SECONDS`.
