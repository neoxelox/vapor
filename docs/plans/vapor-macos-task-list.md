# vapor macOS task list

Plan reference: `docs/plans/vapor-macos-plan.md`

Original source: `docs/plans/vapor-original-plan-verbatim.md`

Status legend:

- [ ] pending
- [~] in progress
- [x] complete

## Phase 0 - Repository foundation and planning docs

- [ ] P0-1 Define monorepo layout (`apps/macos`, `daemon`, `providers`, `shared`, `docs`).
- [ ] P0-2 Expand root `README.md` to production-grade project/operator/developer guide.
- [ ] P0-3 Upgrade `.gitignore` for Rust + Swift/Xcode + macOS + runtime/state/secrets artifacts.
- [ ] P0-4 Create comprehensive `AGENTS.md` with boundaries, standards, tests, and safety policy.
- [ ] P0-5 Add `docs/architecture/` skeleton and baseline diagrams/contracts placeholders.
- [ ] P0-6 Add repository scripts for Rust lint/format/test.
- [ ] P0-7 Add repository scripts for Swift lint/format/test.
- [ ] P0-8 Add local developer runbook section documenting script usage.
- [ ] P0-9 Add GitHub Actions lint workflow for Swift + Rust on PR/push.
- [ ] P0-10 Add GitHub Actions test workflow for Swift + Rust on PR/push.
- [ ] P0-11 Configure required CI checks and branch protection guidance.
- [ ] P0-12 Define distribution trust-chain plan (signing, hardened runtime, notarization, entitlements).
- [ ] P0-13 Define OAuth/provider operations plan (PKCE flow, token refresh failure handling).
- [ ] P0-14 Define state schema versioning, migration test strategy, and rollback posture.
- [ ] P0-15 Define app-daemon compatibility matrix and upgrade/rollback policy.
- [ ] P0-16 Define measurable acceptance budgets and benchmark harness approach.

Exit gate:

- New contributor can clone repo, understand architecture, run local lint/test scripts, and see equivalent CI checks configured.

## Phase 1 - App shell, daemon lifecycle, auto-launch

- [ ] P1-1 Build SwiftUI app shell (onboarding/settings/menubar status placeholders).
- [ ] P1-2 Implement LaunchAgent lifecycle manager with default ON behavior.
- [ ] P1-3 Add optional SMAppService integration for login-item UX.
- [ ] P1-4 Implement auto-launch toggle semantics (enable/disable + optional stop-now).
- [ ] P1-5 Add crash-loop detection and exponential relaunch delay.

Exit gate:

- Daemon reliably starts at login and avoids restart-loop meltdown.

## Phase 2 - Low-impact local engine core

- [ ] P2-1 Implement FSEvents recursive watcher with minimal callback work only.
- [ ] P2-2 Implement default excludes + `.vaporignore` parser and matcher.
- [ ] P2-3 Implement debounce/coalescing loop (250ms tick, conservative windows).
- [ ] P2-4 Implement keyed superseding scheduler (latest intent wins per path).
- [ ] P2-5 Implement throttle controller inputs and 4-state model.
- [ ] P2-6 Gate planner/uploader worker caps strictly by throttle state.

Exit gate:

- Under synthetic load, CPU and I/O impact stay bounded while queue converges eventually.

## Phase 3 - Google Drive provider plus bidirectional flow

- [ ] P3-1 Implement provider trait/capabilities and provider-neutral error taxonomy.
- [ ] P3-2 Implement `provider_gdrive` auth/refresh + remote root initialization.
- [ ] P3-3 Implement upload paths (multipart small, resumable large).
- [ ] P3-4 Implement remote changes polling (low frequency, throttle-aware).
- [ ] P3-5 Implement remote-to-local apply pipeline with durable intents.
- [ ] P3-6 Implement self-write loop prevention (`self_write_cache`, op IDs, TTL rules).

Exit gate:

- Bidirectional Drive sync works end-to-end under normal conditions with durable recovery.

## Phase 4 - Durability, storms, reconcile, conflicts

- [ ] P4-1 Implement durable queue/state DB with at-least-once semantics.
- [ ] P4-2 Implement retries with exponential backoff + jitter.
- [ ] P4-3 Implement storm detection triggers and deferred `RECONCILE_SUBTREE` scheduling.
- [ ] P4-4 Implement interruptible reconcile with idle-biased execution.
- [ ] P4-5 Implement bidirectional conflict policy (keep both copies; no silent overwrite).
- [ ] P4-6 Implement tombstone/delete reconciliation and restart-safe replay.

Exit gate:

- No lost intent across restarts and deterministic behavior under conflict/delete races.

## Phase 5 - XPC contract and diagnostics UX

- [ ] P5-1 Finalize XPC schema for state, queue, auth, auto-launch, and reasons.
- [ ] P5-2 Add control endpoints (pause/resume, flush-now, toggle auto-launch, excludes).
- [ ] P5-3 Implement full menubar state model and reasoned status messages.
- [ ] P5-4 Implement diagnostics panel (throttle reason, queue depth, conflicts, failures).

Exit gate:

- User can understand "what is happening" and "why" without CLI access.

## Phase 6 - Auto-tuning (impact-first)

- [ ] P6-1 Add bounded 60s metrics aggregation and persistence limits.
- [ ] P6-2 Implement tuning loop cadence (60-120s) with one small change per cycle.
- [ ] P6-3 Tune priority order: impact reduction, rate-limit avoidance, then latency.
- [ ] P6-4 Tune polling/debounce/concurrency/storm thresholds within safe bounds.

Exit gate:

- Tuned behavior outperforms static defaults without oscillation or instability.

## Phase 7 - Provider extensibility and S3/R2

- [ ] P7-1 Implement `provider_s3` root/prefix model and upload/delete primitives.
- [ ] P7-2 Implement rename emulation (copy+delete) and capability-aware behavior.
- [ ] P7-3 Validate engine behavior under providers without Drive-like semantics.

Exit gate:

- Engine remains provider-neutral and reliable across at least two providers.

## Phase 8 - Optional safeguards and advanced features

- [ ] P8-1 Add active-coding detection (permissioned) with heuristic fallback.
- [ ] P8-2 Add folder priority classes and temporary flush boost controls.
- [ ] P8-3 Add mass-change/ransomware guard with pause + alert workflow.
- [ ] P8-4 Add richer diagnostics history and support export bundle.

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
