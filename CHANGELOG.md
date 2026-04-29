# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with pre-GA policy details defined in `docs/operations/release-process.md`.

## [Unreleased]

### Added

- New `./scripts/hooks.sh` installs a git `pre-commit` hook that runs the local validation pipeline (`clean.sh` → `lint.sh` → `test.sh` → `build.sh`) before every commit, so agentic and human contributors share one bar for "this commit is correct". The installer is idempotent, preserves any pre-existing non-vapor `pre-commit` hook as `.git/hooks/pre-commit.bak`, and `./scripts/hooks.sh uninstall` only removes hooks carrying the `vapor-managed-hook` marker. Documented in `README.md` Scripts list and `docs/development/runbook.md` (entry point list + dedicated "Pre-commit hook" section explaining the trade-off of full re-build per commit vs catching stale-artifact regressions).
- macOS LaunchAgent and crash-loop interaction now have automated coverage of the four policy scenarios from `docs/operations/macos/launchagent-policy.md`: an installed-plist audit asserts the exact policy-approved key set with `KeepAlive=false` / `RunAtLoad=true` / `ProcessType=Background`; a SIGKILL scenario test asserts the lifecycle manager performs no spontaneous restart for 30s after an unclean exit and only resumes on a coordinator-driven trigger; the crash-loop pause now surfaces a reasoned banner and a menubar `Acknowledge and resume` action via a new `AppShellState.crashLoopPaused` field plus `AppShellViewModel.refreshCrashLoopPauseState` / `acknowledgeCrashLoopPause`; and a clean-shutdown coordinator test asserts menubar-quit only stops the daemon and terminates the app (no install/start that would re-engage launchd). Closes the macOS Wave 0 task list (M1-6).
- Documentation is reframed around a portable Rust core with per-platform app surfaces: new plans (`docs/plans/core.md`, `docs/plans/cli.md`), a per-surface task directory (`docs/tasks/core.md`, `docs/tasks/macos.md`, `docs/tasks/cli.md`) with a roadmap-orchestrator `README.md`, a `docs/architecture/platform-abstractions.md` reference for the planned `core/platform` trait catalog, and `README.md` entrypoints in every docs group (`architecture`, `ci`, `development`, `operations`, `performance`, `plans`, `product`, `tasks`) plus per-platform subdirectories (`architecture/macos`, `operations/macos`).
- `docs/plans/original.md` is removed after confirming every section is fully absorbed into the successor plans, task lists, code, and architecture/operations references. Deliberate evolutions from the original intent (cross-platform reframing, `KeepAlive=false` with in-process crash-loop protection, filesystem-first provider phasing, bidirectional in MVP) are each recorded in the successor doc that took over that section.
- Cross-surface roadmap in `docs/tasks/README.md`, the core plan execution sequence in `docs/plans/core.md §10`, the CLI distribution plan in `docs/plans/cli.md §4`, and `AGENTS.md §1` + `§9` + `§11` are reprioritized to make the focus explicit: the primary deliverable is a polished core runtime, `vapor` CLI on macOS, and macOS app. Engine portability fixes and the `core/platform` trait layer (with macOS-native impls + Windows/Linux stubs) stay in the primary path as mandatory foundation. Windows and Linux platform implementations, `apps/windows`, `apps/linux`, and full cross-OS CLI distribution move into a clearly-deferred / optional section (waves 12+) gated on the project owner explicitly opting into a non-macOS surface.
- Testing strategy is codified explicitly to support autonomous-agent development: `docs/architecture/testing-strategy.md` (new authoritative reference) defines the taxonomy — unit / integration / property / platform-trait contract / concurrency / snapshot / guard-rail / fuzz — the "do not test" list (trivial getters, `Default` mirroring constants, UI rendering on every app surface, interactive TTY behavior on the CLI, third-party internals, policy restatements), the per-surface scope (`core/*` heavy, apps logic-only, CLI no TTY), discipline rules (fast, deterministic, independent, scoped, no network, no real `~/.vapor`), a flaky-test policy (no retry decorators), and a two-tier CI model with an explicit 5-minute-per-OS Tier-1 budget. `AGENTS.md §9` is expanded into seven subsections (`9.1`–`9.7`) that match the strategy. New `Phase CT` cross-cutting tasks in `docs/tasks/core.md` adopt `proptest`, add a CI timing guard, audit the existing suite for trivial-test smell, schedule nightly fuzz + higher-case-count property runs, and establish platform-trait contract-test and IPC skew-matrix requirements. `docs/tasks/macos.md` gains `Phase MT` (audit Swift suite, retire duplicates when lifecycle moves to Rust, explicit "no UI tests" rule). `docs/tasks/cli.md` gains `Phase LT` (snapshot every `--json` command via `insta`, explicit "no interactive TTY tests" rule). `docs/plans/{core,macos,cli}.md`, `docs/development/runbook.md`, `docs/ci/README.md`, and the `core/*` + `apps/macos` READMEs are updated to point at the authoritative reference.
- Vapor settings now expose editable baseline and override ignore-rule lists, persisting them for daemon filtering on the next launch.
- Daemon event/intent bookkeeping now stays bounded in memory by compacting over-cap subtrees into a single reconcile intent instead of letting callback path growth run unbounded.
- Daemon debounce/coalescing now runs on a 250ms tick with conservative quiet windows so config edits settle faster while lockfiles and other unmatched paths wait longer before stabilization.
- Daemon scheduling now keeps one latest intent per path, supersedes stale actions, and requeues dirty paths when new changes arrive during in-flight work.
- Daemon throttle control now evaluates 1s power, thermal, load, disk, network, and activity samples to choose `IdleDrain`, `Light`, `Throttled`, or `Suspended` with deterministic worker caps.
- Daemon planner, hash, upload, and reconcile stages now acquire strict throttle-gated work permits so new work cannot exceed the active state's caps.
- Daemon startup now initializes a SQLite durable queue/state DB, recovers leased intents after restart, and persists queue/state metadata with explicit schema versioning.
- Daemon retry scheduling now applies exponential backoff with deterministic jitter, persists the longest rate-limit slowdown window across restarts, and durably finalizes terminal failures.
- Daemon storm detection now converts noisy subtrees into deferred reconcile markers once per-directory or global burst thresholds trip, keeping callback-side path growth bounded earlier.
- Daemon tests now include micro-regression guards for filesystem callback bursts, debounce/coalescing ticks, and scheduler superseding hot paths.
- Daemon reconcile control now starts only in `IdleDrain`, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion.
- Daemon tests now stress large per-subtree, global-cap, and multi-subtree storm scenarios so bounded memory/backpressure behavior stays covered under heavy pending-intent bursts.
- Daemon startup now composes a real runtime loop that advances watcher ingest, debounce, scheduler draining, durable queueing, throttle-gated work, and idle-biased reconcile progression on each tick.
- Daemon restart recovery now inserts a prioritized whole-scope reconcile and re-prioritizes any existing root reconcile so volatile pre-DB intent loss is reconstructed conservatively before older durable work resumes.
- Daemon callback scope checks now canonicalize the watch root and reject lexically-out-of-scope paths before they can enter bounded ingest state. Per-component symlink resolution runs on the runtime thread (see the Fixed entry below) so the callback stays hot-path safe.

### Changed

- Persisted app config now uses `autoLaunch` and `languageCode`, with English as the default UI language when no other catalog is selected.
- App startup now preserves malformed `vapor.json` files in place, surfaces the load failure in the UI, and avoids silently overwriting broken config with defaults.
- Ignore toggles and saved ignore rules now refresh the in-memory daemon launch configuration immediately so future lifecycle actions stay aligned with persisted settings.
- Placeholder sync-state cycling controls have been removed from the app and menubar until real daemon-backed pause/flush actions exist.
- Runtime path handling now normalizes `VAPOR_DIR`, applies restrictive local permissions to config/log/state artifacts, and redacts sensitive log metadata without panicking on log-file open failure.
- Durable daemon state now redacts and bounds persisted error text, rejects oversized counters or state values, and guards against out-of-range persisted timestamps.
- Pre-GA daemon state now rejects older on-disk schemas instead of carrying forward compatibility shims, and removes an obsolete deferred-reconcile helper API.
- Daemon startup now injects the selected provider through the provider trait boundary instead of hardcoding the Google Drive type inside core daemon orchestration.
- Daemon runtime now advances durable non-reconcile work through bounded planner, hash, and upload stages under throttle/workgate caps instead of processing one leased intent at a time.
- Project framing shifts from "macOS-only background sync product" to "invisible-first background sync product with macOS shipping first and Windows/Linux/CLI following on the same portable Rust runtime". `AGENTS.md` system boundaries, naming conventions (adds the `vapor` CLI), distribution trust chain, toolchain policy, test matrix, definition of done, and docs-update policy are updated accordingly. The root `README.md` install/features/development/agents sections are updated to point at per-group `README.md` entrypoints. `core/*` and `apps/macos` READMEs are rewritten around the portable-runtime vocabulary.
- `docs/architecture/xpc-contracts.md` is renamed to `docs/architecture/ipc-contracts.md` (transport-agnostic). macOS-specific transport details live in `docs/architecture/macos/ipc-transport.md`. Superseded planning files (`vapor-macos-plan.md`, `vapor-macos-task-list.md`, `vapor-macos-distribution-foundation-plan.md`, and the GitHub release-system feature plan) are removed; `vapor-original-plan-verbatim.md` is renamed to `docs/plans/original.md`. `docs/operations/launchagent-policy.md` moves under `macos/`, and `docs/operations/distribution-trust-chain.md` becomes a cross-platform index delegating to the per-OS docs.
- Roadmap now introduces a Phase 3 local filesystem reference provider that exercises every provider-neutral bidirectional mechanic against a loopback backing store, and defers Google Drive integration to Phase 9 so later runtime, safety, profile, XPC, auto-tuning, and provider-extensibility work stabilizes against the reference provider first.
- Roadmap now specifies user-configurable `resourceLimits` (CPU/memory/bandwidth hard ceilings) and `idleBoost` (dynamic headroom expansion when the device is genuinely idle) as a layer over the internal throttle controller, with profile overrides resolving by MIN-lowering, enforcement at the workgate/bandwidth-shaper/memory-compaction layers, and diagnostics surfacing effective ceilings and boost reason codes.
- Design spec now pins deterministic idle-boost / throttle transition behavior (snap-down on `IdleDrain` exit, fresh up-ramp on return, no auto-resume from stale conditions, in-flight work yields at next slice checkpoint), a concrete bidirectional conflict-suffix template (`{stem}~conflict-{deviceId}-{timestampMs}{ext}` with collision-avoidance fallback and "data preservation wins over deletion"), self-write cache constants and xattr/side-file precedence with memory-pressure floors, multi-profile watcher coordination (one watcher per canonical realpath, per-profile queues, shared workgate, panic-contained per-profile runtimes), XPC version-skew rules (`|N - M| <= 1`, typed default-on-unknown-field, payload-size bounds), LaunchAgent plist policy (`KeepAlive=false` with in-process crash-loop backoff) and validation scenarios, SLO applicability under lowered user ceilings, a Phase 2.5 simulator removal checklist blocking Phase 4, a happy-path bidirectional race smoke test in Phase 3, and per-intent "why stuck" diagnostics.
- Documentation pass closes drift introduced during the recent code/spec changes: `data-flow.md` now describes the lexical-only callback plus runtime-thread symlink resolution, `xpc-contracts.md` per-intent stages match `executor.rs::ExecutionStage` plus the `Download`/`WaitingForDownload` Phase 3 stages and the `dropped_incoming_event_count` backpressure metric, `launchagent-policy.md` documents the practical backoff schedule given the 5-crash pause cap and routes validation to P1-6 instead of phases 5/7, `macos-app-lifecycle.md` lists the new provider order and the `CrashLoopPaused`/`acknowledgeCrashLoopPause` flow, `state-schema-migrations.md` reflects the retry-only `attempt_count` semantics and `LEASE_TIMEOUT_MILLIS` recovery, `compatibility-and-upgrades.md` cross-references the XPC version-skew rules, `status-and-goals.md` reflects current pre-GA phase status and the resource-budget / profile / conflict-policy directions, and the planning surface adds explicit `deviceId` schema (P4-1a) and throttle-controller hysteresis (D-3) tasks while clarifying the Phase 5 ↔ Phase 7 ordering for layered resource-budget overrides.

### Fixed

- Packaged `Vapor.app` builds now include the SwiftPM localization resource bundle and no longer crash on launch while the app shell resolves UI copy catalogs.
- Packaging now fails fast if either bundled executable is missing, and runtime daemon resolution stays pinned to the bundled `Contents/MacOS/vapord` sibling binary.
- GitHub Actions macOS workflows now run on `macos-26`, matching Vapor's macOS 26-only app target so SwiftUI app tests load against a supported runtime.
- Daemon runtime no longer creates duplicate durable rows each tick when a reconcile fails to start under non-`IdleDrain` throttle; the upserted scheduler intent is now discarded on failed start so the next flush skips it.
- Startup reconstruction barrier now auto-clears after a bounded deadline so non-reconcile work cannot starve indefinitely if the startup reconcile keeps deferring under persistent non-`IdleDrain` pressure.
- Durable intent `attempt_count` now tracks real retry count instead of lease count: `lease_ready_batch` no longer bumps it, `schedule_retry` is the only increment site, and retry caps are enforced at write time so intents cannot reach `MAX_ATTEMPT_COUNT` from throttle-delayed re-leases.
- Durable state `system_time_to_millis` now validates `u128 → i64` conversion symmetrically with the read-path range check, eliminating silent truncation for far-future wall-clock timestamps.
- Durable lease recovery now resets `attempt_count` for leases older than `LEASE_TIMEOUT_MILLIS` (15 minutes) so stale crash-recovered leases don't carry forward inflated retry counts.
- Runtime paths now create private directories and files with restrictive modes at creation time via `DirBuilder::mode` (Rust) and `createDirectory/createFile attributes:` (Swift), including every intermediate directory under the vapor root; the previous two-step create-then-chmod pattern left intermediates at umask defaults and opened a TOCTOU window where the SQLite DB and `vapor.json` were briefly world-readable.
- Log redaction now covers the expanded auth/secret shape set across both daemon and app loggers, matching inline markers for `access_token=`, `refresh_token=`, `api_key=`, `X-Api-Key:`, `client_secret=`, `set-cookie:`, `id_token=`, `session=`, `password=`, and structured metadata keys containing `api_key`, `client_secret`, `refresh`, `oauth`, or `session`.
- FSEvents callback now performs only lexical path normalization and watch-root prefix check; per-component symlink resolution (previously up to 32 `stat`+`readlink` syscalls per event) has moved to the runtime thread, where stabilized events are validated against the real filesystem before entering the scheduler. Events that resolve outside the watch root are dropped with a diagnostic. This restores callback hot-path discipline per AGENTS §3.
- Daemon timing checks no longer stall or silently bypass budgets on wall-clock rewind: the debounce tick now wakes immediately when `duration_since` fails, the reconcile checkpoint treats an impossible-duration as slice-budget-expired, the throttle sampler re-samples on the next tick, and the staged executor advances stages that cannot be measured. Each path has a regression test that walks the clock backwards and asserts the conservative behavior.
- App shell no longer silently rewrites the user-selected `languageCode` to the fallback catalog language on startup. The user's choice is preserved in `vapor.json` across app restarts; the resolved catalog language is surfaced separately as `effectiveLanguageCode` and drives UI strings, so setting an unavailable locale cleanly degrades to English for display without clobbering the persisted preference.
- Swift logger now caches a per-instance `FileHandle`, calls `synchronize()` after every write, and reopens on error instead of opening a fresh handle per log line; heavy-log paths no longer churn thousands of `open()`/`close()` syscalls and crash-time log loss is bounded by the fsync cadence.
- Daemon workgate permit releases no longer silently leak active counts when a stale or mismatched permit is returned: reconcile pause and completion paths now log a diagnostic if `ThrottleWorkgate::release` rejects the permit, surfacing the accounting discrepancy instead of swallowing it.
- Daemon debounce classifier no longer hardcodes `".vaporignore"` and `"vapor.json"`; both file names come from `core/shared::constants` so the classifier can never diverge from the runtime-level names for Vapor's own config and ignore files.
- Daemon now installs a SIGTERM/SIGINT handler that flips a shutdown flag checked by `run_forever`, so `launchctl unload`, `launchctl kill TERM`, or Ctrl-C exit the runtime loop at the next tick boundary instead of terminating mid-lease. Lease recovery on next start still reclaims any in-flight work; the graceful exit just avoids abruptly killing the daemon during an in-progress tick.
- Default provider is now an inert `FilesystemStubProvider` instead of `GoogleDriveProvider`, so pre-GA daemon runs do not point at Google Drive by default. The stub exposes `name = "filesystem_stub"` and reports no remote-changes-feed / no server-side-rename capability so any caller that treats it as real will fail closed. Google Drive remains compiled in and Phase 9 will flip the default once its credentials pipeline is ready.
- App status surfaces the new default provider display name (`Filesystem (stub)`) instead of the hardcoded `Google Drive` string in `AppShellState.initial`; callers that construct state with a real provider name still override it.
- Path-filter startup discovery now skips the heaviest default-ignored directories (`node_modules`, `.git`, `target`, `dist`, `build`, `coverage`, and similar) when walking the watch tree for `.gitignore`/`.vaporignore` files. Those subtrees are still covered by `DEFAULT_PRE_IGNORE_RULES`, so filtering correctness is unchanged, but a fresh start on a tree with millions of `node_modules` files no longer spends minutes enumerating it before the watcher boots.
- LaunchAgent plist writer now emits `StandardOutPath`, `StandardErrorPath`, and `ProcessType = Background` per `docs/operations/launchagent-policy.md`; `AppShellViewModel` wires the stdout/stderr paths under `<vapor_dir>/logs/` so daemon stdout and stderr survive restarts in the expected location.
- `CrashLoopPolicy` defaults and semantics now match the new policy doc: `failureWindow = 600s`, `baseDelay = 2s`, `maxDelay = 120s`, `delayStartsAfterFailures = 1`, plus a new `maxConsecutiveFailuresBeforePause = 5` that drives a durable `CrashLoopPaused` state. `DaemonLifecycleManager` now returns a `CrashLoopDecision` enum (`noDelay` / `backoff(seconds)` / `paused`), exposes `isInCrashLoopPause` and `acknowledgeCrashLoopPause`, and refuses to start the daemon while paused so runaway crashes do not keep restarting in the background.
- `BoundedFsEventRecorder` now records callback events into a small incoming-events queue behind its own mutex and drains the queue into the bounded event/intent maps only on the runtime thread's next `with_state` / `with_mut_state` call. The FSEvents callback no longer contends with the runtime drain's map mutex; the incoming queue is bounded by `MAX_IN_MEMORY_PENDING_PATHS` and drops with a counter rather than growing without limit under a stalled consumer.

## [0.2.0-alpha.3] - 2026-03-10

### Changed

- Simplified GitHub Actions checkout credential handling so release preflight uses the default authenticated checkout session for tag ancestry fetches.
- Added contributor policy requiring concise `CHANGELOG.md` `Unreleased` notes before non-trivial commits.

## [0.2.0-alpha.2] - 2026-03-10

### Changed

- Hardened `lint`, `test`, `perf`, and `release` GitHub Actions workflows with explicit least-privilege permissions.
- GitHub Releases are now documented as the direct installation source for Vapor app builds.

### Fixed

- Release preflight now uses the default authenticated checkout session for its `main` ancestry fetch so tag-triggered releases can complete reliably.
- CI signing and notarization now run behind the protected GitHub `release` environment, verify the imported `Developer ID Application` identity, and pass the temporary keychain into `notarytool` explicitly.
- `./scripts/version.sh` now refreshes `Cargo.lock` through Cargo and keeps workspace package versions aligned with `VERSION` during release preparation.

## [0.2.0-alpha.1] - 2026-03-10

### Added

- Scoped local/cloud sync directory handling, including safe local root creation and strict sync-root boundaries.
- Gitignore-style filtering with recursive `.gitignore` and `.vaporignore` support plus pre/post user ignore rule layers.
- Locale catalogs with deterministic English fallback for app UI text.
- GitHub release automation with lint/test/perf-gated packaging, draft GitHub Releases, checksums, and release runbooks.
- Centralized `VERSION`-driven app + daemon versioning with build commit provenance surfaced in diagnostics and `vapord --version`.

### Changed

- Shared runtime/config constants are centralized across Swift and Rust so script, app, and daemon defaults stay aligned.
- Release preparation now goes through `./scripts/version.sh`, which creates the release commit and matching tag together.

## [0.1.0] - 2026-03-08

### Added

- Initial project foundation for Vapor app and `vapord` daemon.
- Script-first build, lint, test, and packaging workflows.
- Baseline CI workflows for lint and test on `main` and pull requests.

[Unreleased]: https://github.com/neoxelox/vapor/compare/v0.2.0-alpha.3...HEAD
[0.2.0-alpha.3]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.3
[0.2.0-alpha.2]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.2
[0.2.0-alpha.1]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.1
[0.1.0]: https://github.com/neoxelox/vapor/releases/tag/v0.1.0
