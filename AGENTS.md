# AGENTS.md

This file defines the operating rules for contributors (human and AI) working on `vapor`.

## 1) Product intent and non-negotiables

- `vapor` is an invisible-first background sync product. The first shipping surface is the macOS app; Windows, Linux, and a CLI (`vapor`) follow and consume the same portable Rust runtime.
- The Rust core (`core/*`) is the single portable runtime that powers every surface. Apps under `apps/*` and the `vapor` CLI are UI + OS-integration shims over that runtime; no business logic lives in them.
- Platform-specific code is allowed and encouraged inside `core/platform` when it unlocks native performance; the engine consumes traits, not OS APIs directly.
- Primary priority is user device impact, not strict real-time sync.
- Vapor sync scope is a user-selected local directory replicated bidirectionally with a user-selected cloud directory.
- Vapor is not a full-device backup product and must never broaden scope beyond configured sync roots.
- If configured sync roots are missing, Vapor should create the local root on-device and ensure the cloud root exists provider-side before regular sync work proceeds.
- Core guarantees:
  - Never lose intent state.
  - Recover safely after crash/restart.
  - Defer under pressure and converge eventually.
- Bidirectional behavior is in MVP for Google Drive and must be safety-first.
- Feature parity is mandatory for the invariants above on every OS that currently ships a surface. Autolaunch, crash-loop protection, durable queue, throttle discipline, secret storage, and resource budgets must be delivered via the matching `core/platform` trait implementation; "skip it on the shipping OS" is never acceptable. An OS that is not yet a shipping surface (currently Windows and Linux) may have `unimplemented!()` stubs behind the trait, provided the engine continues to compile on that OS so the door stays open.

## 1.1) Project maturity and compatibility policy

- Vapor is pre-GA and under heavy active development.
- Backward compatibility is not guaranteed yet for local config/state/schema formats, app/daemon internal contracts, or developer-facing interfaces.
- Prefer clear, simple implementations over temporary legacy/migration shims unless the project owner explicitly requests compatibility preservation.

## 2) System boundaries

- Rust daemon (`core/daemon`)
  - Fs-watch ingest, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retry/backoff, reconcile, provider execution.
- Providers (`core/providers`)
  - Cloud API integration via provider trait/capabilities.
  - No provider-specific assumptions in core engine.
- Shared contracts (`core/shared`)
  - IPC schemas, error taxonomies, settings models, version contracts.
- Platform layer (`core/platform`)
  - Traits + per-OS native implementations for fs-watch, service install,
    secret store, metrics sampling, idle detection, filesystem capabilities,
    and process supervision. Engine code consumes traits, not OS APIs.
- Daemon lifecycle (`core/lifecycle`)
  - `CrashLoopGuard`, `DaemonLifecycleManager`, `AutoLaunchSettingStore`.
    Every surface (macOS app, Windows app, Linux app, `vapor` CLI) consumes
    this layer; nothing reimplements it.
- `vapor` CLI (`core/cli`)
  - Headless-first binary that exposes the full runtime on every OS.
- SwiftUI app (`apps/macos`)
  - UX, onboarding, settings, diagnostics, menubar state.
  - Delegates autolaunch, crash-loop, and daemon control to
    `core/lifecycle` by invoking the bundled `vapor` CLI
    (`Contents/Helpers/vapor`) as a subprocess with `--json` output;
    no lifecycle policy is implemented in Swift.
  - Calls the macOS-native `SecretStore` implementation for Keychain access.
- Future apps (`apps/windows`, `apps/linux`) consume the same `core/*`
  stack; no business logic in UI code.

Do not move heavy compute into an app process or the fs-watch callback path.

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
- The CLI binary name must be `vapor` (crate `core/cli`).
- The daemon binary name must be `vapord`.
- The daemon package/crate name should remain `vapor-daemon` for naming consistency.
- Brand/domain identifiers are fixed: brand `ARN`, domain `arn.sh`, bundle ID `sh.arn.vapor`.
- The macOS app bundle identifier must remain `sh.arn.vapor` unless the project owner explicitly changes it.
- Reverse-DNS identifiers used by Vapor (bundle IDs, helper IDs, launch labels, and related service identifiers) must be scoped under `sh.arn.vapor.*`.
- New binaries, CLIs, apps, and libraries must use consistent vapor naming (`vapor*`/`vapor-*`) and avoid unrelated names.

## 3) Performance and throttle invariants

- The fs-watch callback (FSEvents on macOS, ReadDirectoryChangesW on Windows, inotify on Linux) may only normalize/filter/record event metadata.
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
- Sync direction is selected by `syncMode` (`two-way` default; one-way
  `pull-only` / `push-only`), resolved per profile with the top-level value as
  the default. Vapor's "never lose data" / keep-both guarantee applies **only
  to `two-way`**. The one-way modes are an explicit, opt-in, per-profile
  exception: they are **strict mirror** (the non-authoritative side is driven
  to exactly match the declared source of truth, permanently overwriting
  divergent edits and removing extra content). They must never be enabled
  silently or inferred, and must surface an up-front data-loss warning before
  activation. There is no recoverable quarantine — the warning is the
  safeguard. Full design: `docs/architecture/sync-modes.md`.

## 5) Data durability and migrations

- Queue/state storage must provide at-least-once intent semantics.
- Schema versions are explicit and migration-tested.
- Forward migration and rollback behavior must be defined before schema changes ship.
- Corruption recovery path must be documented and observable.
- Pre-GA exception: contributors may make intentional breaking changes to config/state formats without migration when the change is documented and validated in the same change set.

## 6) Security and privacy

- Secrets/tokens only via `core/platform/secrets::SecretStore` — Keychain on macOS, Credential Manager on Windows, Secret Service on desktop Linux, age-encrypted file or external command shim on headless Linux. Tests use the in-memory fake.
- Logs must redact secrets, tokens, auth headers, and sensitive identifiers.
- Telemetry is local-only unless explicitly designed otherwise.
- Any permissioned feature must degrade safely when denied.

## 7) Distribution and trust chain

Each platform owns its own trust chain. Cross-platform principles live here;
concrete per-platform policy lives under `docs/operations/<platform>/`.

### 7.1) Shared principles

- Release artifacts are produced from a script-first pipeline, not an IDE
  archive flow. Every platform packaging script is CI-runnable.
- Each platform gets an isolated GitHub Environment holding its secrets
  (`release-macos`, `release-windows`, `release-linux`). Secrets never
  cross-leak between platform release jobs.
- App/daemon version compatibility rules must be maintained and tested
  per OS.
- Product release version source-of-truth is the repository root `VERSION`
  file.
- `scripts/version.sh` is the supported entrypoint for version bumps,
  Cargo workspace version sync, and release-prep commit/tag creation.
- Release tags must exactly match `v$(cat VERSION)`.
- Release preparation via `scripts/version.sh` must run from a clean `main`
  branch with only `CHANGELOG.md` allowed to be dirty beforehand.
- Build provenance must keep semantic version and git commit SHA separate.
- AI contributors must never auto-open packaged apps (for example `open
  dist/Vapor.app`); app launch verification is performed manually by the
  project owner.

### 7.2) macOS distribution

Full policy: `docs/operations/macos/distribution-trust-chain.md`.

- Distribution model must include: code signing, hardened runtime,
  notarization, entitlement review.
- `Vapor.app` is the single distributable package and must contain the
  three executables:
  - `Contents/MacOS/Vapor`
  - `Contents/MacOS/vapord`
  - `Contents/Helpers/vapor` (the CLI the app shim drives; it lives in
    `Helpers/` because the default macOS filesystem is case-insensitive
    and `vapor` would collide with `Vapor` inside `MacOS/`)
- GitHub Releases must publish file assets, so release uploads should use a
  zip that contains `Vapor.app`; the raw `.app` bundle directory remains a
  local packaging/validation artifact rather than a direct release asset.
- Runtime daemon launch must target only the bundled sibling binary
  (`Contents/MacOS/vapord`) and must not rely on global install paths.
- Xcode project/workspace support is optional convenience for debugging and
  must not become the release source of truth.
- LaunchAgent and login item behavior must be stable across upgrades.
- Apple bundle metadata (`CFBundleShortVersionString`, `CFBundleVersion`)
  uses Apple-valid version fields; commit SHA is stored in dedicated
  `VaporVersion` / `VaporGitCommit` app/daemon build-info fields for logs,
  UI, and `--version` output.

### 7.3) Windows distribution (future)

Lands with `apps/windows`. Expected controls: EV code-signing certificate
(Azure Key Vault or USB HSM), WiX or MSIX packaging, `signtool`. Isolated
`release-windows` GitHub Environment.

### 7.4) Linux distribution (future)

Lands with `apps/linux`. Expected controls: GPG-signed AppImage first;
`.deb`/`.rpm` as demand surfaces; Flathub/Snap later. Isolated
`release-linux` GitHub Environment.

### 7.5) CLI (`vapor`) distribution

Pure Rust binaries per supported target triple, zstd-compressed, checksummed.
Signing follows the host-OS policy (Developer ID on macOS, EV cert on
Windows, GPG signature on Linux). Published alongside platform installers
under the same GitHub Release tag.

## 8) Engineering standards

- Swift (macOS app only)
  - Prefer structured concurrency and explicit actor boundaries.
  - Keep UI/state surfaces deterministic and reason-first.
  - macOS UI must follow Apple Human Interface Guidelines and platform
    conventions.
  - Visual direction should be minimalist, sleek, and polished; prefer
    native controls, spacing, typography, and motion over heavy custom
    chrome.
  - Swift code is macOS-only by policy. Do not attempt to make Swift code
    cross-platform; reach for Rust in `core/*` when logic needs to be
    shared across surfaces.
- Rust (runtime + CLI + platform layer)
  - Use explicit error enums and classify transient vs permanent failures.
  - Keep async/task lifetimes bounded and cancellation-aware.
  - Platform-sensitive code lives under `core/platform/<trait>/<os>.rs`
    behind a trait the engine consumes. Do not sprinkle `#[cfg(target_os)]`
    through engine code.
  - Native-optimal per OS is encouraged; portable-but-slow is not an
    acceptable final state.
- Future app shells (Windows, Linux)
  - UI frameworks are per-app discretion (see `docs/plans/core.md §8` for
    recommendations). Each app shell is a thin client over `core/*`.
- API/contracts
  - Version IPC payloads; pre-GA breaking changes are allowed with
    coordinated updates.
  - Provider trait changes require capability and behavior review.
  - Platform-trait changes require a parity review so every supported OS
    either adopts the change or has a tracked task to do so.

## 8.1) Observability and diagnostics

- Contributors may add structured file logging when needed to diagnose reliability or lifecycle issues.
- Logging should favor actionable context (state, reason, identifiers) and avoid secrets or tokens.
- Temporary debug-heavy logging should be easy to dial down via log levels and should not violate low-impact goals.

## 8.2) Toolchain and platform version policy

- Target latest stable versions by default for every supported host:
  - macOS runner/image in CI + Xcode and Swift toolchain (macOS app).
  - Ubuntu (`ubuntu-latest`) and Windows (`windows-latest`) runners in CI
    for every `core/*` Rust crate as soon as the engine portability fixes
    land.
  - Rust toolchain and required components on every runner.
- Avoid pinning old versions unless there is a documented blocker.
- If temporary pinning/downgrade is required, document the reason, owner,
  and removal criteria.

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
  1. `./scripts/format.sh`
  2. `./scripts/lint.sh`
  3. `./scripts/test.sh`
- For changes that alter runtime behavior a user would observe through
  the daemon or CLI, additionally run `./scripts/e2e.sh` after the
  steps above (Tier E2E; see §9.8).
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

## 8.6) Shared constants policy

- Runtime/config/environment constants must be centralized in language-level constants modules and treated as source-of-truth.
- Current source-of-truth files are:
  - Rust shared constants: `core/shared/src/constants.rs` (authoritative for
    the portable runtime and every Rust-side consumer, including the
    `vapor` CLI and future Windows/Linux apps).
  - Swift app constants: `apps/macos/Sources/VaporCore/VaporConstants.swift`
    (macOS app mirror; must stay in sync with the Rust source of truth).
- Product version source-of-truth is the root `VERSION` file; Cargo workspace version must be synced from it via `./scripts/version.sh`.
- When adding or changing any config keys, environment variables, default values, runtime path names, launch labels, or filtering defaults, contributors must:
  1. update the Rust source-of-truth (`core/shared/src/constants.rs`),
  2. mirror into any platform-specific constants mirror (e.g., the Swift
     mirror for the macOS app),
  3. consume the constant from call sites (avoid re-defining string
     literals), and
  4. update docs/tests in the same change set.
- Avoid duplicated hardcoded literals for `VAPOR_*` keys and shared defaults outside the constants modules unless there is a documented, temporary exception.

## 8.7) Localization and user-facing copy policy

- User-facing UI copy source-of-truth catalogs must live under `assets/locales/*.json`.
- Swift workflow scripts must sync locale catalogs into `apps/macos/Sources/VaporCore/Resources/locales/*.json` before build/test/package.
- Logs and internal diagnostics text may remain English-only.
- Language selection behavior must preserve safe fallback order:
  1. user `languageCode` selection (default `en`),
  2. English (`en`) fallback if that catalog is unavailable.
- When adding or changing user-facing UI text, contributors must update `en.json` (and any other available catalogs) in the same change set.
- If a translation key is missing in a non-English catalog, fallback behavior must remain deterministic and resolve to English.

## 8.8) Code comment policy

- Code comments must never reference internal task-list or roadmap
  identifiers: no task ids (`C8-12`, `M2-1`, `L3-7`, …), no wave or phase
  numbers, and no pointers to `docs/tasks/*`. Task tracking lives in
  `docs/tasks/`; a code comment must stand on its own for a reader who has
  never seen the task lists. The same applies to strings that surface to
  users or logs (e.g. `unimplemented!()` messages).
- Referencing stable documentation (`docs/architecture/*`, `docs/plans/*`,
  `AGENTS.md` sections) is fine — those documents describe the system, not
  the work schedule.
- Keep comments short and direct. A comment earns its place by stating a
  constraint, invariant, or non-obvious "why" that the code cannot express;
  it should not restate what the code does.
- Do not leave useless comments. Delete comments that narrate history
  ("replaces the old X", "retired with Y"), describe the change instead of
  the code, or restate a default/constant defined elsewhere. If a
  historical fact matters (e.g. "this is the only implementation"), keep
  the fact and drop the archaeology.
- A stale comment is a bug: when a change makes a nearby comment wrong or
  obsolete, update or delete it in the same change set.

## 9) Required test matrix

Testing is a non-negotiable part of every change. Vapor is coded
autonomously, so the coding agent's feedback loop is whatever
`./scripts/test.sh` tells it. The full taxonomy, per-module
expectations, and adding-a-test checklist live in
`docs/architecture/testing-strategy.md`. This section is the contract.

### 9.1) Testing philosophy

- **Cover real behavior, not trivial restatements of code.** A
  thousand trivial tests are worse than a hundred well-chosen ones. A
  failing test must catch a real bug.
- **Fast.** `./scripts/test.sh` (Tier 1) must finish in under 2 minutes
  on a contemporary dev machine and under 5 minutes on CI. If a change
  pushes the suite past the budget, split slow tests out to Tier 2.
- **Deterministic.** No `thread::sleep` for timing-dependent
  assertions; use test-injectable clocks. No retry decorators.
- **Independent.** Tests run in any order and in parallel. Each
  integration test uses its own `tempfile::TempDir`. No shared mutable
  state.
- **No network, no `~/.vapor`.** Never contact the real Internet;
  never touch the user's runtime dir.

### 9.2) What must be tested

Every non-trivial logic change in `core/*` must ship with one or more
of the following:

- **Unit tests** for pure logic (debounce, scheduler, throttle,
  workgate, retry, storm, state_db, reconcile, executor, path_filter,
  event_intents, fs_events, runtime_paths, logging).
- **Integration tests** composed through the `DaemonRuntime` tick
  harness for multi-module behavior (local→remote / remote→local
  propagation, restart recovery, throttle transitions during real
  work, storm bursts, self-write-cache suppression, multi-profile
  isolation, config reload mid-work).
- **Property tests** via `proptest` for well-defined invariants where
  random inputs add value (path normalization, scheduler superseding,
  throttle monotonicity, retry backoff monotonicity, conflict-suffix
  determinism, durable-queue FIFO, ignore-rule precedence, IPC
  handshake skew matrix). Each property runs 64–256 cases on CI.
- **Platform-trait contract tests** once `core/platform` lands. Every
  trait runs a parameterized contract suite against both the in-memory
  fake and the real native impl on each shipping OS. Catches
  fake-vs-native drift.
- **Bidirectional race tests** — simultaneous local/remote edits,
  rename+modify, delete/restore, loop-prevention verification.
- **Crash / restart / recovery tests** — pre/post-restart durable
  intent count delta = 0 (excluding completed), retry slowdown
  restored, leases recovered.
- **Performance guard-rails** (Tier 1) — cheap "someone accidentally
  made the callback 100× slower" checks. Distinct from the SLO perf
  suite (Tier 2).
- **Snapshot tests** (`insta`) for every `vapor … --json` command
  once the CLI lands.

### 9.3) What must NOT be tested

As important as §9.2. Refuse tests for:

- Trivial getters / setters that return an inner field.
- `Default` impls whose values are constants mirrored 1:1 from the
  struct definition.
- `Debug` / `Display` impls unless the output is a wire format.
- `serde` derive round-trips of trivial structs.
- Generated code from `build.rs`.
- **UI rendering on any app surface** — SwiftUI views, menubar layout,
  Dock transitions, and the equivalent on future Windows/Linux apps.
  UI correctness is verified by the project owner manually.
- **Interactive TTY behavior** on the `vapor` CLI — color codes, cursor
  positioning, terminal resize, ncurses interactions.
- Third-party crate internals.
- Code that just restates a policy from `constants.rs`.

If a test's failure mode is "I typo'd a default value", skip it.

### 9.4) Per-surface scope

- `core/*` (Rust runtime, CLI, platform layer) — heavy testing. No UI,
  so nothing is carved out.
- `apps/macos` (Swift) — logic tests only (configuration, lifecycle
  coordinator state transitions, view-model state mapping, localization
  fallback, logger redaction). **No UI tests.**
- `core/cli` (`vapor` binary) — logic + snapshot + integration tests.
  `vapor service install` / `run` / `status` round-trip in CI. **No
  interactive TTY tests.**
- Future GUI apps (Windows, Linux) — same rule: logic yes, UI no.

### 9.5) Test tiers

- **Tier 1** — `./scripts/test.sh` via `lint.yml` / `test.yml` /
  `workflow_call`. Unit + integration + platform-trait contract +
  property + snapshot + guard-rail timing tests. Runs on every PR.
  Required check on `main`. Budget: under 5 minutes per OS on CI.
- **Tier 2** — `scripts/perf.sh` via `perf.yml`, which runs only as
  part of the release pipeline (no standalone or scheduled triggers).
  Performance SLO tests, long-running property cases (higher case
  counts), fuzz corpora, `loom`-backed concurrency tests.
  Not a PR gate.
- **Tier E2E** — `./scripts/e2e.sh`, run on every PR in `test.yml`'s
  macOS job and locally by the contributor.
  Black-box verification of the real `vapor` + `vapord` binaries in a
  disposable sandbox. Required locally for runtime-affecting features
  and fixes; contract in §9.8. CI runs it with `--full`, which appends
  the L2-5 service lifecycle round-trip: that phase installs a real
  LaunchAgent, so it is opt-in, meant for disposable CI runners, and
  refuses outright when a `sh.arn.vapor.daemon` LaunchAgent already
  exists. Contributors run the default (host-safe) suite locally.

### 9.6) Flaky-test policy

- A test that fails intermittently on the same input is flaky.
- Flaky tests block merges until fixed or removed. Retry decorators
  are banned.
- A test flagged flaky twice in two weeks is either fixed or removed.
- Removing a flaky test requires an issue describing the invariant
  that is no longer covered.

### 9.7) Platform matrix specifics

- Every trait in `core/platform` must have (a) an in-memory fake used
  by cross-OS unit tests, and (b) a native implementation on every OS
  that currently ships a surface, exercised by the trait contract
  suite in the matching OS-specific CI job (`macos-latest` today;
  `ubuntu-latest` / `windows-latest` once their optional waves in
  `docs/tasks/README.md` land).
- For OSes that do not currently ship a surface, a trait may be
  `unimplemented!()` on that OS. The stub must compile (so
  `cargo build --workspace` keeps succeeding on every OS in the CI
  matrix) and must be tracked in `docs/tasks/core.md`.
- `vapor service install` + `vapor run` + `vapor status` round-trip
  must pass on every OS that currently ships a surface before that OS
  is considered shipped.

### 9.8) End-to-end verification (Tier E2E)

Unit and integration tests are not the whole feedback loop. An agent
that ships a runtime-affecting change must also watch the real product
work once, end to end. Full process:
`docs/development/e2e-verification.md`.

- `./scripts/e2e.sh` builds the shipping binaries (`vapor`, `vapord`)
  and drives the daemon black-box through the CLI only — never the
  macOS app — asserting via `vapor status --json`, `vapor doctor`,
  exit codes, daemon logs, and read-only durable-DB queries.
- Sandbox discipline is absolute: everything runs under the repo-local
  `.vapor/e2e/` directory (removed by `./scripts/clean.sh`). Tier E2E
  must never touch `~/.vapor`, install host services (LaunchAgents,
  login items), launch packaged apps, or use the network.
- Required after Tier 1 passes for: features or fixes in `core/*` that
  change daemon/CLI-observable behavior, startup/shutdown/IPC/schema
  changes, and build changes to the shipping binaries. Not required
  for doc-only, UI-only, or test-only changes.
- When a change adds e2e-observable behavior, extend the harness with
  a scenario for it in the same change set — a green run of old
  scenarios proves non-regression, not the new feature.
- UI-affecting changes additionally get a short manual-verification
  handoff checklist for the project owner, since agents never verify
  UI.
- Live cloud-provider E2E (real Google Drive, dedicated test account)
  is a future, explicitly gated tier — never part of the default run,
  never a PR gate, never run implicitly by an agent.

## 10) Pull request checklist

PRs should answer:

1. What user or reliability problem does this solve?
2. How does it preserve low-impact behavior?
3. What durability or failure paths were validated?
4. What tests were added/updated?
5. What docs/contracts were updated?

Documentation update policy:

- Non-trivial feature/logic changes must update required documentation in the same change set.
- Before creating a commit for a non-trivial feature, fix, refactor, build/release change, or other user/reliability-impacting work, contributors must add concise release-note lines to the root `CHANGELOG.md` `Unreleased` section so the next release can roll them up.
- Root `README.md` is intentionally concise and acts as a product-facing index; detailed operational and engineering content belongs under `docs/` in topic-specific files.
- Contributors must preserve and keep current the `README.md` **Features** section whenever capabilities, guarantees, or supported behavior change.
- Contributors must preserve and keep current the `README.md` **Configuration** section (including `vapor.json` keys, defaults, and `VAPOR_*` environment variables) whenever config/env behavior changes.
- When README content is shortened or reorganized, no critical information may be dropped: move it into the corresponding `docs/` file (or create a new one) in the same change set.
- `README.md` should be updated when behavior, setup, operational workflow, or developer commands change.
- Non-trivial macOS UI/UX changes must document the intended user experience and note alignment with Apple design conventions. Equivalent rule applies per OS: Windows changes follow Fluent / WinUI conventions, Linux changes follow GNOME HIG or KDE HIG per the chosen toolkit.
- `AGENTS.md` should be updated when a new durable engineering rule, safety invariant, or contributor policy should be remembered for future work.
- Plans and tasks live under `docs/plans/{core,macos,cli,...}.md` and
  `docs/tasks/{core,macos,cli,...}.md`. Keep the file for the surface you
  touched up to date in the same change set.
- Platform-specific documentation belongs under
  `docs/<group>/<platform>/…`. Common, cross-platform documentation stays
  at the top of each group directory.
- Every documentation group directory (`docs/<group>/` and every
  per-platform subdirectory `docs/<group>/<platform>/`) must contain a
  `README.md` that serves as the entrypoint for that group. The group
  README must: (a) describe what the group covers, (b) describe each
  file inside the group (what it is, what to expect, how to use it),
  (c) link to any adjacent group READMEs that are directly related, and
  (d) be kept current in the same change set as any addition, removal,
  or rename of files in the group. New docs do not land without the
  matching group README update.
- `docs/tasks/README.md` additionally serves as the cross-surface
  roadmap orchestrator — the "what should be done next" guide across
  every surface (`core`, `macos`, `cli`, future `windows`, `linux`).
  When a wave opens, closes, or changes dependencies, update this README
  in the same change set.
- If docs are intentionally not updated, PR description must explain why no documentation changes were needed.

### README style and Features section policy

- The rules in this subsection apply only to the root `README.md` (the product-facing README).
- Other markdown docs (including nested/module `README.md` files under `docs/`, `apps/`, or `core/`) may use a more technical style appropriate to their audience.
- Keep root `README.md` user-facing: concise, attractive, and easy to scan.
- Use short, direct one-liners in root `README.md` **Features** with one emoji per bullet.
- Prioritize user outcomes and reliability promises (speed feel, low impact, safety, visibility) over implementation internals in root `README.md` **Features**.
- Avoid specific config key names, file names, env vars, or packaging mechanics in root `README.md` **Features** unless absolutely necessary for user understanding.
- Avoid naming specific cloud providers inside root `README.md` **Features**; keep wording provider-agnostic (for example, "cloud sync").
- Avoid device-specific wording such as "laptop" in root `README.md` **Features`; use "device".
- Keep root `README.md` **Features** aligned with real product status:
  - "Available now" for shipped behavior.
  - "In flight and coming next" for planned roadmap items.
- Do not duplicate nearby root `README.md` section content (for example provider lists in **Cloud Providers**) inside **Features**.
- When changing root `README.md` feature tone/style, preserve factual accuracy and do not overpromise.

Required in PR description:

- Scope and non-goals.
- Risk assessment and rollback plan.
- Any migration/compatibility implications.

Commit and push policy:

- Create one git commit per feature or per tightly related change group.
- Keep commits small, cohesive, and rollback-friendly.
- Use commit messages that explain why the change exists.
- Do not push commits to GitHub unless the project owner explicitly asks.

Commit message convention:

- Use Conventional Commit-style subjects: `<type>: <why-focused summary>`.
- Prefer these types and keep PR labels aligned with the same dominant category for GitHub release notes:
  - `feat`: user-visible capability or additive behavior -> label `feat`/`feature` -> release category `Features`
  - `fix`: bug or reliability correction -> label `fix`/`bug`/`bugfix` -> release category `Fixes`
  - `perf`: performance, battery, thermal, or throughput improvement -> label `perf` -> release category `Performance`
  - `refactor`: internal restructuring without intended behavior change -> label `refactor` -> release category `Refactors`
  - `docs`: documentation-only change -> label `docs` -> release category `Docs`
  - `test`: behavior-preserving test-only change -> label `test` -> release category `Testing`
  - `ci`: GitHub Actions or CI pipeline change -> label `ci` -> release category `Tooling`
  - `build`: build, packaging, signing, or release automation change -> label `build` -> release category `Tooling`
  - `chore`: repository maintenance that does not fit another type -> label `chore` -> release category `Tooling`
  - `release`: changelog/version/release-prep only -> label `release` -> release category `Tooling`
- Use one dominant type per commit; split mixed changes when practical so changelog grouping stays accurate.

## 11) Definition of done

"Every shipping OS" below means the OSes currently shipping a surface.
macOS is the primary shipping OS today; Windows/Linux only count as
shipping OSes once their optional waves in `docs/tasks/README.md` have
landed.

A change is done when:

- Behavior works in happy and failure paths on every shipping OS.
- No regression of throttle/impact invariants on any shipping OS.
- Crash/restart recovery is preserved on every shipping OS.
- Observability is sufficient to explain current state/reason on every
  shipping OS.
- For changes that touch a `core/platform` trait: the native impl on
  every shipping OS passes its CI job; on non-shipping OSes the trait
  may be `unimplemented!()` but must compile and must be tracked in
  `docs/tasks/core.md`.
- Docs and contracts are updated (common docs at the top level,
  platform-specific docs under the matching `docs/<group>/<platform>/`).

## 12) Incident playbooks (minimum)

Maintain runbooks for:

- daemon crash loops
- auth/token refresh failures
- provider rate-limit storms
- schema migration failures
- reconcile backlog non-convergence

Each runbook must include detection, mitigation, user-visible state, and recovery verification.
