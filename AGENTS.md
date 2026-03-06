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

## 1.1) Project maturity and compatibility policy

- Vapor is pre-GA and under heavy active development.
- Backward compatibility is not guaranteed yet for local config/state/schema formats, app/daemon internal contracts, or developer-facing interfaces.
- Prefer clear, simple implementations over temporary legacy/migration shims unless the project owner explicitly requests compatibility preservation.

## 2) System boundaries

- SwiftUI app (`apps/macos`)
  - UX, onboarding, settings, diagnostics, menubar state.
  - Keychain access and auth orchestration UI.
  - Auto-launch and daemon lifecycle controls.
- Rust daemon (`core/daemon`)
  - FSEvents ingest, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retry/backoff, reconcile, provider execution.
- Providers (`core/providers`)
  - Cloud API integration via provider trait/capabilities.
  - No provider-specific assumptions in core engine.
- Shared contracts (`core/shared`)
  - XPC schemas, error taxonomies, settings models, version contracts.

Do not move heavy compute into app process or FSEvents callback path.

## 2.1) macOS app component model and lifecycle semantics

- Treat the app window, menubar surface, and daemon as distinct runtime components.
- Login/startup flow should initialize Vapor as menubar-first (no automatic main-window presentation) while keeping daemon lifecycle bootstrap active.
- Closing the main window is a UI action only:
  - close the window
  - remove Dock presence for the UI app surface
  - keep menubar surface active
  - keep daemon runtime active
- Reopen flow (`Open Vapor` from menubar) must focus the existing main window when already open, or restore it when closed, without restarting the daemon.
- Full shutdown (`Quit Vapor` from menubar) must execute daemon stop/shutdown path and then terminate the app process.
- Never couple window-close behavior to daemon termination.

## 2.2) Naming conventions

- The macOS app/program name must be `Vapor`.
- The daemon binary name must be `vapord`.
- The daemon package/crate name should remain `vapor-daemon` for naming consistency.
- Brand/domain identifiers are fixed: brand `ARN`, domain `arn.sh`, bundle ID `sh.arn.vapor`.
- The macOS app bundle identifier must remain `sh.arn.vapor` unless the project owner explicitly changes it.
- Reverse-DNS identifiers used by Vapor (bundle IDs, helper IDs, launch labels, and related service identifiers) must be scoped under `sh.arn.vapor.*`.
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
- Pre-GA exception: contributors may make intentional breaking changes to config/state formats without migration when the change is documented and validated in the same change set.

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
- `Vapor.app` is the single distributable package and must contain both executables:
  - `Contents/MacOS/Vapor`
  - `Contents/MacOS/vapord`
- Runtime daemon launch must target only the bundled sibling binary (`Contents/MacOS/vapord`) and must not rely on global install paths.
- Xcode project/workspace support is optional convenience for debugging and must not become the release source of truth.
- AI contributors must never auto-open packaged apps (for example `open dist/Vapor.app`); app launch verification is performed manually by the project owner.
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
  - Version XPC payloads; pre-GA breaking changes are allowed with coordinated updates.
  - Provider trait changes require capability and behavior review.

## 8.1) Observability and diagnostics

- Contributors may add structured file logging when needed to diagnose reliability or lifecycle issues.
- Logging should favor actionable context (state, reason, identifiers) and avoid secrets or tokens.
- Temporary debug-heavy logging should be easy to dial down via log levels and should not violate low-impact goals.

## 8.2) Toolchain and platform version policy

- Target latest stable versions by default for:
  - macOS runner/image in CI
  - Xcode and Swift toolchain
  - Rust toolchain and required components
- Avoid pinning old versions unless there is a documented blocker.
- If temporary pinning/downgrade is required, document the reason, owner, and removal criteria.

## 8.3) Known-good local baseline (reference)

Current verified contributor baseline (Mar 2026):

- macOS `26.3` (Tahoe)
- `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- `cargo 1.93.1 (083ac5135 2025-12-15)`
- `swift-driver 1.127.15`
- `Apple Swift 6.2.4 (swiftlang-6.2.4.1.4 clang-1700.6.4.2)`
- target `arm64-apple-macosx26.0`

This section is informational and should be updated when contributor baseline shifts materially.
It does not override the "latest stable" policy above.

## 8.4) Script-first validation command policy

- Contributors must run repository wrapper scripts under `scripts/` instead of invoking raw tool commands directly for routine validation.
- Required validation order is:
  1. `./scripts/format.sh apply`
  2. `./scripts/lint.sh`
  3. `./scripts/test.sh`
- Rationale: wrapper scripts set required project environment (for example log routing and other workflow invariants).

## 8.5) Runtime data directory policy

- Runtime artifacts must live under a single vapor directory root (`vapor_dir`): config, logs, and durable state.
- Configuration file path is `vapor_dir/vapor.json`.
- Logs should be written under `vapor_dir/logs/`.
- Durable queue/state DB paths should live under `vapor_dir/state/`.
- Runtime directory selection is code-defined and env-overridable only; it is not a user-configurable `vapor.json` field.
- Runtime directory resolution order is:
  1. `VAPOR_DIR` environment variable (explicit override)
  2. local dev/test/CI default `./.vapor` when `VAPOR_ENV=dev` (and in test/CI contexts)
  3. normal runtime default `~/.vapor`
- Repository scripts must default `VAPOR_DIR` to `./.vapor` so local and CI behavior are consistent.
- Repository scripts should default `VAPOR_ENV` to `dev` (and `prod` for package flow).

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
  - app lifecycle semantics: window close (UI only) vs menubar quit (full shutdown)
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
- `README.md` must keep a complete user-configuration and `VAPOR_*` environment-variable reference (what each option does and its default); any config/env change must update that reference in the same change set.
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
