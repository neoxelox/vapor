# AGENTS.md

This file defines the operating rules for contributors (human and AI) working on `vapor`.

## 1) Product intent and non-negotiables

- `vapor` is an invisible-first macOS background sync product.
- Primary priority is user device impact, not strict real-time sync.
- Core guarantees:
  - Never lose intent state.
  - Recover safely after crash/restart.
  - Defer under pressure and converge eventually.
- Bidirectional behavior is in MVP for Google Drive and must be safety-first.

## 2) System boundaries

- SwiftUI app (`apps/macos`)
  - UX, onboarding, settings, diagnostics, menubar state.
  - Keychain access and auth orchestration UI.
  - Auto-launch and daemon lifecycle controls.
- Rust daemon (`daemon`)
  - FSEvents ingest, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retry/backoff, reconcile, provider execution.
- Providers (`providers`)
  - Cloud API integration via provider trait/capabilities.
  - No provider-specific assumptions in core engine.
- Shared contracts (`shared`)
  - XPC schemas, error taxonomies, settings models, version contracts.

Do not move heavy compute into app process or FSEvents callback path.

## 2.1) Naming conventions

- The macOS app/program name must be `Vapor`.
- The daemon binary name must be `vapord`.
- The daemon package/crate name should remain `vapor-daemon` for naming consistency.
- New binaries, CLIs, apps, and libraries must use consistent vapor naming (`vapor*`/`vapor-*`) and avoid unrelated names.

## 3) Performance and throttle invariants

- FSEvents callback may only normalize/filter/record event metadata.
- No DB/hash/network work in callback.
- All expensive work must be throttle-state gated.
- Throttle states: `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- Under `Suspended`, uploads/hashing stop; only lightweight intent coalescing continues.
- Reconcile scans are deferred and interruptible; run only when policy permits.

## 4) Bidirectional sync safety requirements

- Implement loop prevention (`self_write_cache`, operation IDs, TTL discipline).
- Handle local/remote races deterministically.
- Default conflict policy: keep both (never silent overwrite).
- Maintain tombstones and deletion semantics with durable replay.
- Remote poll/apply pipeline must obey throttle and retry constraints.

## 5) Data durability and migrations

- Queue/state storage must provide at-least-once intent semantics.
- Schema versions are explicit and migration-tested.
- Forward migration and rollback behavior must be defined before schema changes ship.
- Corruption recovery path must be documented and observable.

## 6) Security and privacy

- Secrets/tokens only in Keychain (or platform-secure equivalent for tests).
- Logs must redact secrets, tokens, auth headers, and sensitive identifiers.
- Telemetry is local-only unless explicitly designed otherwise.
- Any permissioned feature must degrade safely when denied.

## 7) macOS distribution and trust chain

- Distribution model must include:
  - code signing
  - hardened runtime
  - notarization
  - entitlement review
- macOS app distribution must be script-first and CI-runnable, producing `Vapor.app` and zip artifacts without requiring Xcode UI archive workflows.
- Xcode project/workspace support is optional convenience for debugging and must not become the release source of truth.
- LaunchAgent and login item behavior must be stable across upgrades.
- App/daemon version compatibility rules must be maintained and tested.

## 8) Engineering standards

- Swift
  - Prefer structured concurrency and explicit actor boundaries.
  - Keep UI/state surfaces deterministic and reason-first.
  - macOS UI must follow Apple Human Interface Guidelines and platform conventions.
  - Visual direction should be minimalist, sleek, and polished; prefer native controls, spacing, typography, and motion over heavy custom chrome.
- Rust
  - Use explicit error enums and classify transient vs permanent failures.
  - Keep async/task lifetimes bounded and cancellation-aware.
- API/contracts
  - Version XPC payloads and avoid breaking changes without migration.
  - Provider trait changes require capability and behavior review.

## 8.1) Toolchain and platform version policy

- Target latest stable versions by default for:
  - macOS runner/image in CI
  - Xcode and Swift toolchain
  - Rust toolchain and required components
- Avoid pinning old versions unless there is a documented blocker.
- If temporary pinning/downgrade is required, document the reason, owner, and removal criteria.

## 8.2) Known-good local baseline (reference)

Current verified contributor baseline (Mar 2026):

- macOS `26.3` (Tahoe)
- `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- `cargo 1.93.1 (083ac5135 2025-12-15)`
- `swift-driver 1.127.15`
- `Apple Swift 6.2.4 (swiftlang-6.2.4.1.4 clang-1700.6.4.2)`
- target `arm64-apple-macosx26.0`

This section is informational and should be updated when contributor baseline shifts materially.
It does not override the "latest stable" policy above.

## 9) Required test matrix

Every substantial change must include relevant test updates.

- Rule: any non-trivial new feature or logic change must ship with tests.
- Trivial changes (for example typo fixes, copy edits, or purely mechanical renames) may skip tests when behavior is unchanged.

- Unit tests
  - debounce/coalescing behavior
  - scheduler superseding semantics
  - throttle state transitions
  - provider error mapping
- Integration tests
  - local->remote and remote->local paths
  - restart recovery with pending queue
  - retry/backoff behavior
  - auto-launch toggle and daemon lifecycle
- Bidirectional race tests
  - simultaneous local/remote file edits
  - rename+modify races
  - delete/restore races
  - loop-prevention verification
- Performance tests
  - synthetic event storms
  - load/thermal/battery transition behavior
  - CPU and I/O budget checks

## 10) Pull request checklist

PRs should answer:

1. What user or reliability problem does this solve?
2. How does it preserve low-impact behavior?
3. What durability or failure paths were validated?
4. What tests were added/updated?
5. What docs/contracts were updated?

Documentation update policy:

- Non-trivial feature/logic changes must update required documentation in the same change set.
- `README.md` should be updated when behavior, setup, operational workflow, or developer commands change.
- Non-trivial UI/UX changes must document the intended user experience and note alignment with Apple design conventions.
- `AGENTS.md` should be updated when a new durable engineering rule, safety invariant, or contributor policy should be remembered for future work.
- If docs are intentionally not updated, PR description must explain why no documentation changes were needed.

Required in PR description:

- Scope and non-goals.
- Risk assessment and rollback plan.
- Any migration/compatibility implications.

Commit and push policy:

- Create one git commit per feature or per tightly related change group.
- Keep commits small, cohesive, and rollback-friendly.
- Use commit messages that explain why the change exists.
- Do not push commits to GitHub unless the project owner explicitly asks.

## 11) Definition of done

A change is done when:

- Behavior works in happy and failure paths.
- No regression of throttle/impact invariants.
- Crash/restart recovery is preserved.
- Observability is sufficient to explain current state/reason.
- Docs and contracts are updated.

## 12) Incident playbooks (minimum)

Maintain runbooks for:

- daemon crash loops
- auth/token refresh failures
- provider rate-limit storms
- schema migration failures
- reconcile backlog non-convergence

Each runbook must include detection, mitigation, user-visible state, and recovery verification.
