# Vapor full-repository review — REPORT.md (temporary)

Date: 2026-09-06. Branch: `yerevan`. Baseline commit: `47be741`.

This file is a working task list. Findings are appended as the review
progresses, grouped by area. Each finding is a checkbox task; `[ ]` means
proposed (awaiting owner review), `[x]` means implemented.

Severity legend: **P0** = data-loss / safety / security; **P1** = bug or
wrong behavior a user or CI would hit; **P2** = inconsistency, drift, or
doc bug; **P3** = polish / nice-to-have.

Verification status legend: `CONFIRMED` (reproduced or verified in code
with certainty), `LIKELY` (strong code reading, not executed).

---

## 0. Review scope and method

- [x] Read every file under `docs/`, `scripts/`, `core/`, `apps/`,
      `.agents/`, `.github/`, root files (all Rust crates incl. tests,
      Swift sources + tests, workflows, scripts, skills).
- [x] Run `./scripts/format.sh check`, `./scripts/lint.sh`,
      `./scripts/test.sh` on this worktree to establish baseline — all
      green (details in §13).
- [x] Run `./scripts/e2e.sh` (host-safe default) — S1–S19 green — and a
      throwaway-`VAPOR_DIR` CLI probe (no service install) to exercise the
      CLI UX (findings in §13).

## 1. Documentation — index, product, architecture

- [x] **P2 CONFIRMED** `docs/README.md` lists a standalone `REVIEW.md`
      ("One standalone file lives at this level") but `docs/REVIEW.md` was
      deleted in PR #9 (`737fb74`). Remove the stale entry.
- [x] **P2 CONFIRMED** `docs/product/status-and-goals.md` is stale on
      several shipped facts: says the default provider is the inert
      `FilesystemStubProvider` and that Google Drive is "intentionally
      deferred"; says multiple profiles are "planned in Phase 5",
      conflict-suffix paths "planned in Phase 4", self-write cache
      "planned in Phase 3". All of these shipped (README Features / Providers
      say "Available now"). Rewrite the status section against current
      reality (filesystem + gdrive selectable, profiles, conflicts, loop
      prevention all shipped; Windows/Linux pending).
- [x] **P2 CONFIRMED** `docs/architecture/system-overview.md` module sketch
      says `provider_gdrive (deferred)` and "Planned implementation
      sequence" lists steps 1–7 that have all landed. Update the sketch and
      turn the sequence into "implementation history / remaining" or drop it.
- [x] **P3** `docs/architecture/data-flow.md` §"Local safeguards" headings
      carry task ids `(C8-55)`, `(C8-56)`, `(C8-57)` and §Loop prevention
      says "The filesystem provider (Phase 3)". AGENTS §8.8 bans task ids in
      code/user strings only, but the docs policy says stable docs describe
      the system, not the schedule — strip the ids for consistency.
- [x] **P2 CONFIRMED** `docs/architecture/platform-abstractions.md` has a
      stale "Wave 4 status (`core/platform` v0)" paragraph ("The runtime
      currently consumes `ProcessSupervisor` end-to-end; the remaining
      traits are wired progressively as Waves 5–8 land"). Waves 5–8 landed.
      Replace with present-tense status per trait.
- [x] **P2 LIKELY** `docs/architecture/platform-abstractions.md` documents
      CLI flags `--secrets-backend=keyring|file|command` and
      `--user-activity=always|never|auto` on the `vapor` CLI. Verify they
      exist in `core/cli`; if not, mark them as future or remove.
- [x] **P2 LIKELY** `docs/architecture/platform-abstractions.md` §Adding a
      new platform says "Add parity tests under `core/platform/tests/`" but
      no `core/platform/tests/` directory exists (contract tests live
      inline). Point at the real location.
- [x] **P2 CONFIRMED** `AGENTS.md §2` describes `core/ipc` as "Framed
      JSON-RPC transport", while `docs/architecture/ipc-contracts.md`
      explicitly says the envelopes are "Vapor-specific tagged enums …, not
      JSON-RPC". Fix AGENTS.md wording ("framed JSON").
- [x] **P2 CONFIRMED** `docs/architecture/sync-modes.md` is written in
      future tense about shipped work ("`syncMode` *will be* carried on the
      sync scope … the field lands with C8-59", "once profiles land",
      "Build / rollout order … ships in Wave 8"). Rewrite as present-tense
      design; strip task ids (C8-xx, M4-5, CT-7) from the architecture doc.
- [x] **P2 CONFIRMED** All three per-platform IPC transport docs
      (`docs/architecture/{macos,windows,linux}/ipc-transport.md`) describe
      the frame body as "UTF-8 JSON-RPC 2.0", and `macos/ipc-transport.md`
      says NSXPC could wrap "the same JSON-RPC contract". The contract doc
      says explicitly it is not JSON-RPC. Align wording to "UTF-8 JSON
      envelope (`{kind,payload}` / `{outcome,value}`)".
- [x] **P2 CONFIRMED** `docs/architecture/macos/app-lifecycle.md` §Core
      architecture still says "Pre-GA default is `FilesystemStubProvider`
      (inert…)" and "`provider_filesystem` … is the Phase C8 reference
      provider". Also a typo: "Macintosh transport is a Unix domain socket".
      Update the provider bullet to current reality and fix the typo.
- [x] **P2 LIKELY** `docs/architecture/macos/app-lifecycle.md` claims
      native macOS impls for `PlatformMetricsSampler (IOKit / NSProcessInfo)`
      and `IdleNotifier (CGEventSource)`, while `data-flow.md` says
      "real system-driven throttle metrics sampling … still uses
      conservative placeholders on some hosts". Verify against
      `core/platform/src/{metrics,idle}.rs` and make the two docs agree.
- [x] **P3** `docs/architecture/provider-onboarding.md` carries task ids
      (C8-47, C8-50, C8-54) in an architecture doc; strip.
- [x] **P2 LIKELY** `docs/architecture/linux/ipc-transport.md` says "the
      lint-only Linux CI leg keeps cross-OS compilation gated" and
      `windows/ipc-transport.md` says a `windows-latest` test job arrives
      with Wave 12. Verify against `.github/workflows/{lint,test}.yml`
      (AGENTS §8.2 says ubuntu/windows test runners should exist for every
      `core/*` crate once portability fixes land).

## 2. Documentation — operations, development, CI, performance

- [x] **P2 LIKELY** `docs/operations/runtime-logging-and-localization.md`
      says logs are `<vapor_dir>/logs/vapor.logs` / `vapord.logs` and the
      state DB is `<vapor_dir>/state/vapor.sqlite` only. Verify file names
      against `core/shared/src/constants.rs`, and mention the per-profile
      `state/profiles/<id>/vapor.sqlite` layout + `lifecycle.json`.
- [x] **P2 CONFIRMED** `docs/operations/release-process.md` §Post-run
      verification lists only `Contents/MacOS/Vapor` and
      `Contents/MacOS/vapord` as required package contents; AGENTS §7.2
      requires three executables including `Contents/Helpers/vapor`.
- [x] **P3** `docs/operations/macos/launchagent-policy.md` §Validation and
      `docs/operations/macos/README.md` reference task ids M1-5 / M1-6 and
      describe them as future work ("M1-6 … is the work item that ships
      these scenarios"). Reconcile with `docs/tasks/macos.md` status and
      drop the ids.
- [x] **P3** `docs/operations/provider-auth-operations.md` carries task id
      `(C8-50)`; strip.
- [x] **P2 CONFIRMED** `docs/development/e2e-verification.md` still says
      "`cloud/` ← … (a label for the stub provider today)" and "No network.
      Today's provider is the filesystem stub". The real filesystem provider
      is what S10–S17 exercise. Update both sentences.
- [x] **P2 CONFIRMED** `docs/development/runbook.md` references
      "`AGENTS.md` Bash safety" (no such section exists), says snapshot
      updates happen "once [the CLI] ships" via `cargo insta review` (insta
      is not adopted; CLI shipped), and keeps the bootstrap-era note
      "Scripts intentionally skip missing stack artifacts … (no
      `Cargo.toml` yet)". Clean up.
- [x] **P2 LIKELY** `docs/ci/README.md` says "A CI timing guard (tracked as
      `core.md` CT-2) fails the job if Tier 1 exceeds the 5-minute budget".
      Verify `test.yml` actually enforces it (`timeout-minutes` or an
      elapsed check); otherwise reword as pending.
- [x] **P3** `docs/ci/overview.md` and `docs/ci/required-checks.md`
      duplicate the runner/toolchain/pinned-action/cache lists verbatim.
      Keep one authoritative copy (overview) and link from required-checks.
- [x] **P3** `docs/performance/acceptance-budgets-and-benchmark-harness.md`
      says "Phase 7 (P7-16) integration tests must cover the following
      cross-product". Verify whether such tests exist; either point at them
      or mark as open work in `docs/tasks/core.md` and drop the id here.

## 3. Documentation — plans and tasks

- [x] **P2 CONFIRMED** `docs/plans/README.md` says "Start with `original.md`
      for product intent" — no `docs/plans/original.md` exists. Point at
      `docs/product/status-and-goals.md` instead.
- [x] **P2 CONFIRMED** `docs/plans/core.md` §5 says "No IPC code exists yet"
      and "Protocol: **JSON-RPC 2.0**"; §2 labels crates "(NEW)"; §2.2 says
      "A new crate (or module inside `core/shared`)". All shipped; the
      protocol is a Vapor-specific JSON envelope. Update to present tense.
- [x] **P2 CONFIRMED** `docs/plans/cli.md` is internally inconsistent about
      wave numbering: §4 says Linux/Windows CLI binaries are "gated on the
      optional waves 12–14" while §4.2 says "(Windows ⇒ wave 9; Linux ⇒
      wave 10)". Align with `docs/tasks/README.md`.
- [x] **P2 LIKELY** `docs/plans/core.md` §7 and `docs/plans/cli.md` §3 list
      the CLI command surface without `conflicts`, `diagnostics`,
      `support-bundle`, `auth status`, `service bootstrap` flags, etc.
      Compare against `core/cli/src/main.rs` and update both plans (or make
      one the source and link from the other — the two lists are verbatim
      duplicates today).
- [x] **P2 LIKELY** `docs/plans/cli.md` §3.1 and §8 reference
      `--user-activity=always|never|auto` and `cargo insta review` snapshot
      flow. Verify; neither appears to exist. Mark as planned or drop.

## 4. CI workflows

- [x] **P1 LIKELY** Concurrency-group collision in the release pipeline.
      `release.yml` uses `group: release-${{ github.ref }}`; `lint.yml` and
      `test.yml` use `group: ${{ github.workflow }}-${{ github.ref }}` with
      `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}`. In a
      called reusable workflow the `github` context belongs to the caller,
      so `github.workflow` = `release` and `github.ref` = the tag: both
      called workflows resolve to the *same* group as the caller
      (`release-refs/tags/vX`) with `cancel-in-progress: true`. This has not
      been exercised (no release since the groups were added). Defensive
      fix: give each reusable workflow a unique group
      (`${{ github.workflow }}-lint-${{ github.ref }}` etc.) and keep
      `cancel-in-progress` false for tag refs.
- [x] **P2 CONFIRMED** `CHANGELOG.md` Unreleased claims "main pushes and
      release-invoked runs are never cancelled", but with the expression
      above, release-invoked runs (tag refs) evaluate `cancel-in-progress`
      to `true`. Fix the expression (`!startsWith(github.ref,'refs/tags/')
      && github.ref != 'refs/heads/main'`) and the changelog line.
- [x] **P2 CONFIRMED** `test.yml` has no 5-minute Tier 1 timing guard
      (`timeout-minutes: 45` on the whole job incl. e2e). `docs/ci/README.md`
      presents one as existing ("A CI timing guard … fails the job"); either
      add an elapsed-time check inside `scripts/test.sh` (env-tunable like
      perf.sh) or reword the doc as pending (CT-2).

## 5. Runtime — headline product gaps (core/platform)

- [x] **P0 CONFIRMED** `core/platform/src/secrets.rs`: `NativeSecretStore`
      is a process-local `InMemorySecretStore` on every OS
      (`is_persistent() == false`). `vapor auth login gdrive` therefore
      stores the OAuth token in the CLI process and loses it at exit; the
      daemon (a separate process) can never read it, so the Google Drive
      provider cannot sync in production. The CLI does print a warning, but:
      README **Providers** lists Google Drive as "Available now"; AGENTS §6,
      SECURITY.md, `docs/operations/provider-auth-operations.md`, and
      `docs/architecture/macos/app-lifecycle.md` all state tokens live in
      the macOS Keychain; `docs/tasks/core.md` C3-4 and C8-48 are `[x]`
      with no open task tracking the Keychain bridge. Fix: implement the
      macOS Keychain-backed `SecretStore` (`security-framework` or
      `keyring`, per plan §3.3) with `is_persistent() == true`, add a
      contract test against the native impl, and until it lands mark
      Google Drive as "in flight" in README and open a tracked task.
- [x] **P1 CONFIRMED** `core/platform/src/metrics.rs`:
      `NativePlatformMetricsSampler` forwards to
      `StaticPlatformMetricsSampler` with `ThrottleInputs::default()`
      (`has_native_sampling() == false`) on every OS, and
      `core/platform/src/idle.rs::NativeIdleNotifier::idle_for()` always
      returns `Duration::ZERO`. Consequences on the shipping OS: the
      throttle controller never sees real CPU/battery/thermal/network
      signals (only the active-coding heuristic can raise caution), and
      idle boost can never engage (idle time is always 0). README
      **Features** advertises "Pressure-aware throttle modes that adapt sync
      intensity to real device load" and "Smart idle boost" as "Available
      now"; `docs/architecture/macos/app-lifecycle.md` claims native
      IOKit/NSProcessInfo/CGEventSource impls; `docs/tasks/core.md` C3-5 /
      C3-6 are `[x]` with the bridges deferred to "follow-ups" that have no
      task id. Fix: (a) open tracked tasks for the macOS
      `PlatformMetricsSampler` (host_statistics64/task_info,
      IOPSCopyPowerSourcesInfo, NSProcessInfo thermal/low-power) and
      `IdleNotifier` (CGEventSourceSecondsSinceLastEventType /
      IOHIDSystem HIDIdleTime) bridges, (b) move the two README bullets to
      "In flight and coming next" (or reword honestly), (c) fix the
      app-lifecycle doc and the data-flow caveat ("on some hosts" → on
      every host today).
- [x] **P1 CONFIRMED** `core/cli/src/commands/doctor.rs::locate_daemon_binary`
      only checks the CLI's sibling and `PATH`; it does not check the
      bundled layout `../MacOS/vapord` that `core/cli/src/main.rs::
      locate_daemon_binary` handles. Run from `Vapor.app/Contents/Helpers/
      vapor`, `vapor doctor` reports `vapord_binary` **FAIL** (exit 1) even
      though `vapor service install` works. Fix: one shared resolver
      (e.g. in `vapor_cli::commands` or `core/lifecycle`) used by both.

## 6. CLI (`core/cli`)

- [x] **P2 CONFIRMED** `vapor config set` accepts any string for the
      object/array keys (`profiles`, `resourceLimits`, `idleBoost`,
      `safeguards`) and writes it verbatim as a JSON string; the daemon
      loader then reports "ignoring invalid `profiles`" and silently falls
      back to defaults. Either parse the value as JSON for those keys (and
      validate shape) or reject them with "edit vapor.json directly".
- [x] **P2 CONFIRMED** `vapor doctor` has no `--json` flag although
      `docs/plans/cli.md §2` says "Every command supports `--json`" and the
      doctor module doc says the report can be rendered "as `--json`". Add
      `--json` (locked shape test) or drop the claim.
- [x] **P2 CONFIRMED** `vapor config get <unset-key>` prints an empty line,
      while the doc comment in `config.rs::get` says "the binary renders
      that as the documented default value". Render the compiled default
      (from `VaporConfig::default()`) for unset keys, or fix the comment.
- [x] **P2 CONFIRMED** Docs name the command `vapor support bundle`
      (`CONTRIBUTING.md` §Reporting bugs, `SECURITY.md` §Scope) but the CLI
      is `vapor support-bundle`. Fix the docs.
- [x] **P3** `vapor auth login` warns "native secret store is not yet wired
      in on this OS" — the wording implies other OSes have it; none do.
      Reword once the Keychain bridge lands (or now: "not yet available").
- [x] **P3** `scripts/cli/{build,test}.sh` are referenced only from
      `core/cli/README.md`; they are thin duplicates of `cargo` commands
      and not part of the documented script surface (`docs/development/
      runbook.md`, root README). Either list them in the runbook or remove
      them (and the L0-3 reference).

## 7. `core/lifecycle` and `core/ipc`

- [x] **P3 CONFIRMED** Broken doc comments left behind by the task-id purge
      (dangling fragments): `core/lifecycle/src/durable.rs` module doc
      ("//!(tracked there because the gap was observed …"),
      `core/ipc/tests/skew_matrix.rs` ("//!by exercising every supported
      version pair"), `core/platform/src/service/macos.rs` ("…bundled
      `vapor` CLI\n//!, so this is the single writer…"). Rewrite the three
      sentences.
- [x] **P3** `core/lifecycle/src/durable.rs::save` stages via
      `path.with_extension("vapor-tmp")` (one shared name); two concurrent
      `vapor service` invocations (app health tick + user CLI) can race on
      the same temp file. Use `runtime_paths::unique_temp_path` like the
      other writers.
- [x] **P3** `core/ipc/src/lib.rs` module doc still calls the wire format
      "JSON-RPC 2.0-style"; align with `ipc-contracts.md` ("not JSON-RPC").
- [x] **P3** `core/ipc/tests/skew_matrix.rs::payload_bounds_oversized_first_
      frame_drops_connection_cleanly` asserts nothing (reads and discards).
      Assert the `PayloadTooLarge` error frame and the subsequent EOF.

## 8. Providers (`core/providers`)

- [x] **P2 CONFIRMED** `FilesystemStubProvider` / `default_provider()` still
      carry the bootstrap-era doc comment claiming the stub is "the default
      provider". The runtime selects `FilesystemProvider` through
      `select_provider_for_profile`; `default_provider()` survives only as
      the inert fallback for a suspended profile (`multi_runtime.rs`
      overlap / invalid-provider arms), `DaemonApp::default()`, and tests.
      Rename to `inert_stub_provider()`, fix the comment, and keep it out of
      the public "pick a provider" surface.
- [x] **P2 CONFIRMED** `GoogleDriveProvider::enumerate` silently skips
      remote entries whose name contains `/` (unrepresentable in
      `RemotePath`). A file that never syncs is undiagnosable. Log once per
      name at WARNING, the way the reconcile walk logs non-UTF-8 local names.
- [x] **P2 LIKELY** gdrive download session: a range GET that answers with
      an empty body reports `Progressed { bytes_transferred: 0 }`; the
      worker loop (`provider_jobs::run_transfer`) refunds the whole grant
      and re-steps immediately, so a misbehaving/proxying endpoint can spin
      a worker hot without ever failing the intent. Count consecutive
      zero-byte steps and surface a transient error after N.
- [x] **P2** Renames are never renames: `PendingIntentKind::Rename` is
      planned as an Upload of the new path plus a Delete of the old one
      (`executor.rs::plan_intent`), so `Provider::rename` and
      `supports_server_side_rename` are dead surface and renaming a large
      file re-uploads it in full (and briefly doubles its cloud footprint).
      Either implement move detection → `rename` (FSEvents delivers
      `RenameMode::Both` pairs; gdrive supports server-side move), or
      document the capability as reserved and drop it from the contract
      suite until then. When implementing, note that
      `FilesystemProvider::rename` overwrites an existing destination.
- [x] **P3** Verify the gdrive out-of-scope negative cache (parent-chain
      walk memo, `a_busy_out_of_scope_file_is_walked_once_…`) is bounded;
      on a shared Drive with thousands of out-of-scope files it grows for
      the daemon lifetime.
- [x] **P3** `core/providers/README.md` still describes the stub as the
      default and omits the contract suite's three fixtures
      (filesystem, filesystem-no-xattr, object-store mock).

## 9. Daemon engine (`core/daemon`)

### 9.1 Correctness / durability

- [x] **P2 CONFIRMED** Offline same-size local edits never sync. The
      reconcile walk compares by size only (`reconcile_walk.rs` header:
      "Fast-path equality is size-based") and the macOS watcher starts
      "since now" (`fs_watch/macos.rs` via `notify::recommended_watcher`,
      no persisted FSEvents id), so an edit that keeps the byte count and
      happens while `vapord` is not running is invisible to both paths.
      This breaks the "durable eventual consistency" promise. Cheap fix:
      the walk already collects `LocalEntry.modified_at` and the sync
      index stores `local_modified_at`/`size_bytes` — treat
      `(size == index.size) && (mtime != index.local_modified_at)` as
      divergence (rsync quick check) and enqueue an Upload; the planner's
      hash/precondition path then converges identical content silently.
      Add a `DaemonRuntime` integration test (edit file while runtime is
      dropped, rebuild, converge) and an e2e scenario. Optional follow-up:
      persist the FSEvents event id and replay history on start.
- [x] **P2 LIKELY** A panicking provider call wedges its intent forever.
      `provider_jobs::worker_main` has no `catch_unwind`: the worker thread
      dies, `in_flight` never decrements, the pool never respawns (the dead
      handle stays in `workers`), the execution stays in
      `PlannerProbe`/`UploadRunning`, the runtime renews its lease on every
      sweep, and the workgate permit leaks for the process lifetime. Release
      builds abort on panic (crash-loop guard takes over), but dev/test and
      any future non-abort profile hang silently. Wrap `run_job` in
      `catch_unwind` and report a terminal `TransferFailed`-shaped outcome
      ("provider panicked: …"). Add a test with a panicking provider under
      `ProviderJobPool::threaded`.
- [x] **P2 CONFIRMED** Unbudgeted whole-file hashing on the tick thread:
      `apply_downloaded_payload_keep_both` always hashes the displaced
      local file (no size/mtime pre-check), and
      `deletion_loses_to_local_state` / `child_delete_is_safe` hash when
      the mtime fast-path misses. A multi-GB download-apply or remote
      directory delete stalls every profile's tick (IPC status, debounce,
      other transfers) and runs even under `Suspended`, violating
      AGENTS §3. Reuse the size+mtime fast path against the pre-apply
      index for the aside file, and route residual large-file hashes
      through `StreamingFileHash` slices (or the job pool).
- [x] **P2 CONFIRMED** Provider I/O still blocks the tick thread outside
      the executor: `RemotePoller::poll_if_due` → `poll_changes` (one
      network round trip per cadence, up to every 5 s in IdleDrain),
      `ReconcileWalker::compare_directory` → `enumerate` (one
      uninterruptible call per directory; the slice deadline only applies
      *between* calls), `retry_cloud_root_if_needed` / `build_with_app` →
      `ensure_cloud_sync_directory`. PR #9 moved only executor calls. With
      Google Drive every RTT freezes all profiles. Dispatch poll and
      enumerate through `ProviderJobPool` and harvest on a later tick, or
      document the residual explicitly in `data-flow.md`.
- [x] **P2 LIKELY** A single-profile daemon whose only profile is suspended
      at composition (invalid `provider`, overlapping roots, unopenable DB)
      exits immediately: `MultiProfileRuntime::run_forever` returns `Err`
      when every slot is failed → launchd / `service check` restarts it →
      crash-loop pause, and `vapor status` can never show the actionable
      `suspended_reason`. Keep the process alive (idle tick, status/IPC
      served, reason visible) when the suspension is a configuration
      error; exit only on runtime failures. Add an e2e scenario
      (`provider: gdrvie` → `vapor status --json` names the reason).
- [x] **P2 CONFIRMED** No config hot-reload exists. `vapor.json` edits
      (`vapor config set`, app Settings) apply only after a daemon
      restart; the CLI prints no hint, the app says so only under the
      ignore-rules editor, and there is no `vapor service restart`.
      Meanwhile `AGENTS.md §9.2` lists "config reload mid-work" as an
      expected integration test, `testing-strategy.md` promises "Config
      reload mid-work does not lose in-flight intents", and
      `data-flow.md §Ceiling transitions` rules 3–4 specify live-reload
      semantics with no implementation. Decide one of: (a) implement
      reload for the safe subset (`resourceLimits`, `idleBoost`,
      `safeguards`, ignore toggles/rules, `timelineLimit`) with
      restart-required for roots/provider/profiles; or (b) make
      `config set` print "restart the daemon to apply", add
      `vapor service restart`, and fix the three documents.
- [x] **P2 CONFIRMED** Resource ceilings are only partly real:
      `resource_budget_status` hard-codes `memory_utilization_percent: 0`;
      `apply_memory_ceiling` budgets *cache entries* (`memoryPercent × 400`),
      not device memory; the CPU ceiling only scales workgate caps and no
      CPU is ever measured (static sampler, §5). README **Features** and
      the `resourceLimits` row promise "hard caps on its share of CPU,
      memory, and network". Implement the platform metrics bridge (§5) or
      soften the wording until it lands.
- [x] **P2** `EffectiveBudgetConfig::resolve` honors only `enabled` and
      `boost*Percent` from a profile's `idleBoost` override;
      `minIdleSeconds`, `headroomCpuPercent`, `rampUpSeconds`,
      `rampDownSeconds` are silently ignored although README documents
      per-profile `idleBoost` overrides. Honor them (MAX for `minIdle`,
      MIN for headroom/boost, MIN for ramp-down) or document the subset.
- [x] **P3** `remote_deletion_wins` returns `true` when the local mtime is
      unreadable; the "data preservation wins" rule argues for `false`.
- [x] **P3** Two-way file/directory type mismatch in the walk only logs a
      WARNING on every pass; push one timeline entry and surface it in
      `vapor conflicts list` so the user can act.
- [x] **P3** `UploadPreflight` NotFound arm re-dispatches with the stale
      `RemotePrecondition::None`; set `Absent` for the fresh create.
- [x] **P3** `intent_diagnostics` rows for active executions report
      `attempt_count: 0` and an empty `last_error` although the executor
      holds the `DurableIntentRecord`; pass both through
      `active_stages()`.
- [x] **P3** `list_queue_intents` orders by `(available_at_ms, id)` while
      leasing orders by `(priority_rank, available_at_ms, id)`, so the
      diagnostics list disagrees with lease order; order the same way (an
      index for it already exists) and drop the stale `RawIntentRow` doc
      comment that omits `priority_rank`.

### 9.2 Hygiene and consistency

- [x] **P3 CONFIRMED** `DaemonApp::set_throttle_state` logs routine
      throttle transitions at WARNING; a healthy e2e run ends with 6
      warnings (S8 tolerates them). Log at INFO; reserve WARNING for
      entering `Suspended`.
- [x] **P3** Four writers of `vapor.json` with three behaviours: CLI
      `config set` and `device_id::resolve_or_persist` write 0600 (the
      latter without `with_config_lock`), `auto_launch.rs::write` and
      `ipc_service::write_config_key` use plain `fs::write` (0644, lock
      held). Add one `runtime_paths::write_config_atomic_private` and use
      it everywhere.
- [x] **P3** `reconcile.rs`: `let _ = now;` leftovers in `try_start_next`
      and `checkpoint`; `reconcile_walk.rs` uses `SystemTime::now()` for
      temp-file reaping instead of the injected `now`;
      `mark_cloud_root_unavailable` reads `clock.now_system()` while its
      callers hold `now`.
- [x] **P3** `StreamingFileHash::step` allocates a fresh 64 KiB buffer per
      step; keep it on the struct.
- [x] **P3** `MassDeleteGuardSettings::resolve` logs a WARNING whenever it
      runs with the guard disabled — fine at composition, noisy if ever
      called per tick; assert it is composition-only (it is today).
- [x] **P3** `core/daemon/README.md` says older schemas are rejected and
      mentions `proptest`; v3/v4 now migrate forward and the workspace has
      no `proptest` dependency.

## 10. macOS app (`apps/macos`)

- [x] **P2 (UX; tracked as M3-2 but worth calling out)** The Dashboard and
      menubar "Sync status" is static: `syncState` only ever flips to
      `.error` on a config-load or lifecycle failure, so the app shows
      "Idle · Filesystem — No pending work" while the daemon is paused,
      throttled, mid-transfer, or holding failed intents. Until M3 lands,
      either hide the detail line or read `vapor status --json` on the
      existing 30 s health tick (same subprocess seam as lifecycle) and map
      `run_state`/`throttle_state` onto `SyncSurfaceState`.
- [x] **P2 (UX)** Settings changes (ignore toggles, rules, language) are
      persisted but need a daemon restart; the app offers no restart
      action and mentions the restart only under the ignore-rules editor.
      Add "Restart daemon" (stop + start on the lifecycle queue) or wire
      config reload (§9.1). Pairs with the CLI hint task.
- [x] **P3** Dead code + trivial tests: `SyncSurfaceState.detail` (English
      strings, unused by any view — views use `detailLocalizationKey`) and
      `AppShellState.statusLine` exist only for
      `syncStateDetailsAreNonEmpty` / `statusLineIncludesStateAndProvider`,
      the kind of test AGENTS §9.3 says to refuse. Remove all four.
- [x] **P3** `VaporConstants.Provider` and `VaporConstants.Providers`
      duplicate the provider ids; `Runtime.daemonStdoutLogFileName` /
      `daemonStderrLogFileName` have no Swift consumer (verify they mirror
      a Rust constant; else drop). Consolidate.
- [x] **P3** `Package.swift` sets `platforms: [.macOS("26.0")]` while
      `SMAppServiceLoginItemController` keeps `@available(macOS 13.0, *)`
      and an `if #available(macOS 13.0, *)` guard — dead availability
      checks.
- [x] **P3** `ProcessVaporCLIRunner.run`: if `process.run()` throws, the
      two drain closures and the `DispatchGroup` are left running until the
      pipes deinit; close the write ends in a `defer` so the failure path
      is deterministic.
- [x] **P3** `apps/macos/README.md` still says the cloud root must exist
      for gdrive; the daemon ensures it (`ensure_cloud_sync_directory`).

## 11. Agent knowledge base — `AGENTS.md` and skills

### 11.1 Accuracy fixes

- [x] **P2 CONFIRMED** Both `.agents/skills/*/SKILL.md` declare
      `license: MIT`; the repository is GPL-3.0 (`LICENSE`). Set
      `GPL-3.0-only` or drop the field.
- [x] **P2 CONFIRMED** `vapor-e2e/SKILL.md` documents scenarios S1–S8 (with
      S9/S10 asides); `scripts/e2e.sh` runs S1–S19 plus R1–R9 under
      `--full`. Replace the hand-copied table with a pointer to the script
      header as the source of truth plus the invariants the agent must
      preserve (sandbox discipline, never `--full` locally, add a scenario
      with every e2e-observable change).
- [x] **P3** `vapor-debug/SKILL.md` is accurate where checked (DB tables,
      5 consecutive tick failures) but omits: per-profile DBs at
      `state/profiles/<id>/vapor.sqlite` (only the implicit `default`
      profile uses `state/vapor.sqlite`), the `vapor.sqlite.corrupt-<ms>`
      quarantine files, `state/lifecycle.json`, the socket relocation
      rule (path > 100 bytes → OS temp dir; `vapor doctor` explains it),
      and `vapor diagnostics` / `vapor support-bundle` as first-line tools.
- [x] **P2** `AGENTS.md` statements that no longer match the code (fix in
      one pass): §2 "Framed JSON-RPC transport" (the format is explicitly
      not JSON-RPC, `ipc-contracts.md`); §6 "Keychain on macOS" (native
      store is in-memory today, §5); §9.2 `proptest` / `insta` presented
      as in use (neither is a dependency; `insta` is at least flagged as
      open) and "config reload mid-work" (no such feature); §9.7 "runs a
      parameterized contract suite … in `core/platform`" (only
      `core/providers/tests/provider_contract.rs` exists; CT-5 is open);
      §12 status line is correct. Each should read as either current
      reality or an explicitly tracked target with its task id in
      `docs/tasks/`.

### 11.2 Structure proposal (break AGENTS.md into policy + skills)

`AGENTS.md` is ~560 lines and is loaded into every agent session. Most of
its length is *procedure* (how to validate, how to release, how to add a
config key, how to write commits/docs) rather than *invariants*. Proposal:

- [x] Keep in `AGENTS.md` only the durable rules: §1 intent and
      non-negotiables, §1.1 compatibility policy, §2/§2.1/§2.2 boundaries
      and naming, §3 throttle invariants, §4 bidirectional safety, §5
      durability, §6 security, §7.1 trust-chain principles (one paragraph
      each), §8 engineering standards (Rust/Swift rules, comments policy
      §8.8, knowledge-file policy §8.9), §9.1–9.4 testing contract, §11
      definition of done. Add a 10-line "Start here / reading order" block
      at the top and a "Skills index" section that names each skill and
      its trigger.
- [x] New skill `vapor-validate` — the script-first validation order
      (§8.4), which tier to run for which change type (§9.5, §9.8), flaky
      policy (§9.6), time budgets, how to read a red run, and the
      "add an e2e scenario with the feature" rule. Trigger: before
      committing or when a change touches `core/*`, `apps/*`, `scripts/*`.
- [x] New skill `vapor-release` — `scripts/version.sh` flow, clean-`main`
      precondition, tag = `v$(cat VERSION)`, environment protection +
      action-allowlist checklist (§7.1 bullets), the one permitted push,
      post-run verification (`docs/operations/release-process.md`).
      Trigger: the owner asks to cut/prepare a release or touch
      `release.yml`.
- [x] New skill `vapor-config` — the §8.6 four-step sync for any new
      `vapor.json` key / `VAPOR_*` env var / default / launch label:
      `constants.rs` → `VaporConstants.swift` → call sites → README
      **Configuration** + `.env.example` + docs/tests, plus the CLI
      `config set` typed-parser table and the e2e S1 round-trip. Trigger:
      any change under `core/shared/src/constants.rs` or to config keys.
- [x] New skill `vapor-provider` — provider trait + capability honesty
      rules, `RemotePath` scope safety, op-id tags, changes feed / cursor
      expiry contract, retry classification, the contract suite fixture
      list, docs (`provider-onboarding.md`, `provider-auth-ops.md`),
      README **Cloud Providers**. Trigger: touching `core/providers` or
      adding a provider kind.
- [x] New skill `vapor-docs` — the §10 documentation duties: CHANGELOG
      `Unreleased` line before commit, group `README.md` per docs dir,
      root README Features/Configuration rules, plans/tasks update, "no
      task ids in code comments", and the AGENTS.md-update trigger.
      Trigger: any non-trivial change; any file added/removed under
      `docs/`.
- [x] New skill `vapor-commit` — Conventional Commit types ↔ labels ↔
      release categories, one commit per cohesive change, why-focused
      subjects, the no-push rule, the PR description template (§10
      questions). Trigger: creating commits or PRs.
- [x] Mirror each new skill with a `.claude/skills/<name>` symlink (§8.9)
      and list them in the `AGENTS.md` skills index; `description` fields
      written in third person with explicit trigger conditions.
- [x] Add `.agents/README.md` (what lives here, how skills are mirrored,
      how to add one) — the docs-group README rule stops at `docs/`, but
      the same entrypoint discipline helps agents here.

## 12. Crate READMEs, `docs/tasks`, scripts (remaining items)

- [x] **P2 CONFIRMED** `docs/tasks/cli.md`: L1-5 (`syncMode` validation)
      is implemented (`config set syncMode bogus` → "expected one of
      [two-way, pull-only, push-only]") — mark `[x]`; L2-1 note is stale;
      MT-2 is done; the "vapor support bundle" spelling.
- [x] **P2 CONFIRMED** `docs/tasks/core.md` marks C3-4/C8-48 (secret
      store), C3-5 (metrics sampler), C3-6 (idle notifier) as done while
      the native impls are placeholders (§5). Reopen them (or add explicit
      follow-up ids) so the gap is tracked, and add tasks for: doctor
      resolver unification, provider I/O still on the tick thread, config
      reload decision, offline same-size edit detection, worker panic
      containment, keep-alive on all-profiles-suspended.
- [x] **P2 CONFIRMED** `docs/tasks/README.md` Waves 12/13 still say "Add
      `windows-latest` / `ubuntu-latest` as a real (not lint-only) CI job"
      although `test.yml` already runs the full matrix (C1-6 `[x]`);
      `docs/architecture/{linux,windows}/ipc-transport.md` repeat the
      stale "lint-only" claim.
- [x] **P3** Crate READMEs drifted: `core/README.md` (crate list / wave
      status), `core/cli/README.md` (command list misses `conflicts`,
      `diagnostics`, `support-bundle`, `auth status`; references
      `scripts/cli/*.sh`), `core/ipc/README.md` ("JSON-RPC"),
      `core/lifecycle/README.md`, `core/platform/README.md` (nonexistent
      `tests/` contract dir, `--secrets-backend` flag), `core/shared/README.md`.
- [x] **P3** `scripts/version.sh` verifies the synced version in only 3 of
      the 7 workspace crates; either check every `core/*/Cargo.toml` or
      rely on `[workspace.package] version` inheritance and assert that
      instead. `scripts/rust/*.sh` keep a bootstrap-era `collect_manifests`
      fallback for a missing workspace manifest — delete.

## 13. Validation runs and manual CLI probe (2026-09-06)

| Run | Result |
| --- | --- |
| `./scripts/test.sh` | green — Rust ~673 tests, Swift 85, version checks; 38.8 s |
| `./scripts/format.sh check` | green |
| `./scripts/lint.sh` | green |
| `./scripts/e2e.sh` (default, host-safe) | green — S1–S19 pass; S8 reports 6 daemon WARNING lines on a healthy run (routine throttle transitions, see §9.2) |

`--full` (R1–R9 LaunchAgent round-trip) was deliberately not run on this
host (AGENTS §9.5). `.vapor/e2e/` was left as the script leaves it.

Manual CLI probe (throwaway `VAPOR_DIR`, `VAPOR_ENV=dev`, no daemon, no
service install) — observations that became tasks:

- [x] **P2 CONFIRMED (extends the §6 item)** `vapor config set
      resourceLimits '{"cpuPercent":5}'` writes the value as a JSON
      *string* (`"resourceLimits": "{\"cpuPercent\":5}"`). The daemon then
      starts but logs `[ERROR] Preserved unreadable vapor configuration;
      continuing with defaults. issue=ignoring invalid resourceLimits …`
      and silently ignores the user's ceiling. Same for `idleBoost`,
      `safeguards`, `profiles`. Parse object/array keys as JSON in the CLI
      (reject non-JSON with the same "expected …" style used for enums and
      booleans), round-trip them in `config get`, and cover it in e2e S1.
- [x] **P3 CONFIRMED** `vapor service --help` shows empty descriptions for
      `start`, `stop`, `restart`, `status` and for `--system` (the
      `--user` text carries the `--system` explanation). Add `///` docs on
      the clap variants.
- [x] **P3 CONFIRMED** `vapor doctor` has no `--json` (already in §6; the
      probe confirms `error: unexpected argument '--json'`).
- [x] **P3 CONFIRMED** `vapor config get syncMode` on an unset key prints
      an empty line with exit 0 (already in §6); the README says defaults
      are printed.
- [x] **P3** `vapor doctor` probes host-global state (`~/Library/
      LaunchAgents/sh.arn.vapor.daemon.plist`) even when `VAPOR_DIR`
      points at a sandbox; label that row "host" so a sandboxed run does
      not read as sandbox state.
- Positive observations (no task): error messages for a missing daemon
  ("daemon not running — try `vapor service start`"), enum/boolean
  validation, unknown-key rejection, and `vapor --version` provenance
  (`0.2.0-alpha.3 (47be741)`) are clear and consistent.

## 14. Severity summary and suggested implementation order

Counts (excluding the legend): 1 × P0, 3 × P1, ~48 × P2, ~33 × P3.

Suggested order once the owner has reviewed this file:

1. **P0** — native `SecretStore` (macOS Keychain) so `vapor auth login
   gdrive` survives the CLI process; contract test; mark Google Drive
   "in flight" in README until it lands (§5).
2. **P1s** — platform metrics sampler + idle notifier bridges (or, as an
   interim, soften every "hard cap / idle boost" claim), `vapor doctor`
   bundled-`vapord` resolver, CI concurrency-group fix (§4, §5, §6).
3. **Engine P2s with user impact** (§8–§9): offline same-size edit
   detection, provider-job panic containment, tick-thread hashing,
   provider I/O off the tick thread, keep-alive when every profile is
   suspended at composition, config-reload decision + CLI restart hint,
   typed object keys in `vapor config set`, gdrive silent `/` skip.
4. **One consistency sweep PR** (§1–§3, §11.1, §12): AGENTS.md accuracy,
   "JSON-RPC" wording everywhere, stale architecture/ops/CI docs,
   tasks reopened/closed to match reality, skills license + scenario list,
   crate READMEs.
5. **AGENTS.md → skills restructuring** (§11.2) as its own change set,
   after the owner decides which skills to create.
6. **P3 polish batch** (log levels, dead code, doc comments, helpers).

## 15. Framing notes from the owner (2026-09-06)

- The repo is pre-GA work in progress with no users. No backward
  compatibility is required for config, state schema, IPC, CLI output, or
  internal contracts; changes should be made without compatibility shims.
  Consequences for the tasks above: the `state_db` v3→v4→v5 migration
  chain can be deleted in favour of "reject anything but current" (§9),
  `Provider::rename` / `supports_server_side_rename` can be removed from
  the trait outright until move detection exists (§8),
  `FilesystemStubProvider` / `default_provider()` can move under
  `#[cfg(test)]`-style gating (§8), the `vapor config set` typed-key work
  may change the CLI contract freely (§6, §13), and the four
  `vapor.json` writers can be replaced by one helper without keeping the
  old file modes (§9.2). Every "interim: soften the wording" option
  remains valid only as a stop-gap; the real fix is preferred.
- The README/AGENTS "overclaim" findings are still worth fixing because
  the docs distinguish "Available now" from "In flight"; for a WIP repo
  the correct move is to move the claim to "In flight", not to delete it.
- Agent knowledge base: the source of truth is `AGENTS.md` and
  `.agents/skills/<name>/SKILL.md`. `CLAUDE.md` and `.claude/skills/*`
  are symlinks and must never be edited directly; a new skill is added
  as a real directory under `.agents/skills/` plus a `.claude/skills/`
  symlink (AGENTS §8.9). §11.2 is to be implemented that way.

## 16. Owner additions (2026-09-06, second round)

- [x] Add the `unslop` skill verbatim at `.agents/skills/unslop/SKILL.md`
      with the `.claude/skills/unslop` symlink. Done in this session; the
      symlink resolves. Claude Code discovers skills at session start, so
      the load check is: restart, confirm `unslop` appears in the skill
      list, invoke it once. Structure matches the two existing skills
      (front matter limited to `name` / `description`).
- [x] Apply `unslop` to every piece of writing produced while implementing
      this report: docs, READMEs, CHANGELOG lines, commit messages, code
      comments, and chat replies. The current README **Features** bullets
      fail it on two counts (colon connectors like "Low-impact by design:
      …", and the em dash in the sync-direction bullet).
- [x] When §11.2 moves procedure out of `AGENTS.md`, `AGENTS.md` keeps a
      "Skills" section that names every skill, states when to invoke it,
      and links the file, so an agent reading only `AGENTS.md` knows the
      skills exist. Each moved section leaves a one-line pointer in place
      ("Validation order and tiers: see skill `vapor-validate`").
- [x] Every skill and `AGENTS.md` change is verified as loadable by Claude
      Code before commit: the `.claude/skills/<name>` symlink resolves to a
      `SKILL.md`, the front matter parses, and after a session restart the
      skill shows up and can be invoked.

### README Features review

Problems with the current list:

- Overlaps. "Low-impact by design", "Pressure-aware throttle modes", and
  the in-flight "Fast-feeling background sync … without stealing your
  machine" say the same thing three ways. "Stays out of your way while
  keeping status and controls one click away" restates the diagnostics
  bullet plus the menubar. "Conflict-safe behavior" and "Conflicts stay
  visible" are one idea split in two.
- Missing features users care about and the code already has: crash and
  reboot recovery (durable queue, lease replay), no echo between devices
  (self-write loop prevention), atomic downloads (temp file plus rename),
  chunked uploads that survive a throttle pause, retry with backoff that
  honors provider rate limits, automatic creation of missing sync
  folders and self-healing when the cloud folder disappears, the full
  command line for scripts and servers, secrets in the system keychain
  and secret-free logs, no telemetry, and the menu-bar-only presence.
- The in-flight list is thin. It omits the macOS diagnostics window,
  conflict notifications, Windows and Linux apps, standalone CLI
  downloads, signed and notarized releases, rename/move detection, and
  live settings reload (all tracked in `docs/tasks`).
- Honesty gate. Four bullets describe behaviour that only exists once
  §5 lands: hard caps on CPU and memory, idle boost, "defers heavy work
  under pressure" (the throttle reads static inputs today), and keychain
  storage. They stay under "Available now" only if the P0/P1 fixes ship
  in the same change set; otherwise they move to "In flight".

Proposed replacement (one emoji each, no colons as connectors, no em
dashes, provider-agnostic, "device" not "laptop"):

Available now:

- 🔁 Two-way sync between a local folder and a cloud folder. Changes are picked up within seconds while you work.
- 🧠 Nothing is lost. Every change is written to disk before it moves, so a crash, a reboot, or a dropped connection resumes where it stopped.
- 🛡 Edits never silently overwrite each other. When two devices change the same file, Vapor keeps both copies.
- 🧭 Conflicts stay visible until you settle them, and one command resolves each one from any device.
- 🔂 Your own uploads never bounce back as new changes, so two devices cannot ping-pong a file forever.
- 🔀 Pick a direction per folder. Full two-way, or a one-way mirror for read-only backups and copies.
- 🧩 Run several sync profiles at once. One folder can flow to two clouds, or separate setups stay isolated.
- 📦 Downloads land whole or not at all, and big uploads are chunked so a pause does not restart them.
- 🔁 Retries back off on their own and respect provider rate limits, so a bad hour of connectivity fixes itself.
- 🪶 Heavy work waits while your device is busy, protecting battery and thermals.
- ⚙️ Hard caps on the share of CPU, memory, and network Vapor may use, so streaming and browsing always have room.
- 🌙 When the device sits idle, Vapor speeds up. It backs off the moment you return.
- 🌩 A burst of thousands of file changes is absorbed instead of turned into thousands of uploads.
- 🛟 A sudden mass deletion pauses sync before the wipe can reach the cloud.
- 🧹 Ignore rules, including your existing gitignore files, keep build output and junk out of the sync.
- 🚀 Starts at login, restarts itself after a crash, and stops retrying when something is really broken instead of looping.
- ⏯️ Pause and resume on demand. Changes made while paused sync when you resume.
- 📈 Status with a reason, queue depth, a live activity timeline, per-file "why is this stuck", and a one-command support bundle.
- ⌨️ A full command line for scripts and servers. Everything the app does, the terminal does too.
- 🔐 Sign-in tokens live in the system keychain, logs never contain secrets, and nothing leaves your device except the files you chose to sync.
- 🔕 Lives in the menu bar. No windows unless you ask for one.

In flight and coming next:

- 🪟 A diagnostics window in the Mac app with throttle reason, queue, conflicts, and timeline, plus live pause and flush controls.
- 🔔 A notification when a conflict needs you.
- ✂️ Renames and moves without re-uploading the file.
- ♻️ Settings that apply live, no daemon restart.
- 🖥️ Windows and Linux apps on the same runtime as the Mac app.
- 📥 Standalone command-line downloads for every OS, with Docker and systemd recipes.
- 🧾 Signed and notarized releases, verified on a clean machine every cycle.

Notes for the owner: two bullets share the 🔁 emoji (sync and retries);
swap the retry one for 🔃 or ⏳ if you want them distinct. "Deletes go to
the cloud's trash" was left out because only Google Drive does that today
and the list must stay provider-agnostic; it can live in the Providers
section instead. Twenty-one "now" bullets is long; the candidates to cut
first if you want it tighter are 🔂 (fold into 🛡) and 📦 (fold into 🧠).

- [x] Apply the approved list to `README.md` **Features** in the docs
      sweep (step 4 of §14), after the §5 P0/P1 decisions fix which
      bullets are "now".

## 17. Implementation notes (2026-09-06)

Every task above is checked because the branch implements it; the
notes below record where the implementation deviates from the wording
in the finding, so the review of the PR can judge each one.

- §5 P0: the keychain store uses `security-framework-sys` and
  `core-foundation` directly (no high-level crate) and attaches an
  access list naming `vapor` and `vapord`; a rebuilt unsigned binary
  still prompts once, documented in the auth-operations doc.
- §5 P1 metrics: CPU, power, thermal, Low Power Mode, memory and HID
  presence are real on macOS; `disk_pressure` and the two network
  fields keep neutral defaults (tracked as C3-11). Input within 30 s
  marks the user active, which holds `Throttled`; the e2e harness pins
  `VAPOR_THROTTLE_INPUTS=static` so a developer typing does not stall
  reconcile scenarios.
- §8 renames: removed from the provider trait rather than implemented;
  move detection is RV-11.
- §9.1 tick-thread hashing: the keep-both apply and both deletion
  guards now answer the common case from size plus mtime (the mtime
  comparison was broken at nanosecond granularity, so the fast path had
  never fired). A file that was touched since its last sync and has a
  size that could still match is hashed on the tick thread; moving that
  residual to the job pool is not done.
- §9.1 type mismatch: surfaced on the timeline once per walk pass, not
  in `vapor conflicts list` (that list is derived from conflict copies
  on disk).
- §9.2 `MassDeleteGuardSettings::resolve` warning: no assertion added;
  live reload now calls it whenever `safeguards` changes, so it is no
  longer composition-only and the warning on a disabled guard is the
  intended reminder.
- §9.1 config reload: option (a) implemented for the live subset;
  `vapor service restart` already existed, and the CLI hint plus the
  status notice cover the restart-required keys.
- §10 constants: `VaporConstants.Providers` folded into `Provider`; the
  daemon stdout/stderr log-name mirrors were kept because the Rust
  constants say they are mirrored, even though no Swift code reads them.
- §11.2: `AGENTS.md` is now 646 lines, longer than the original in line
  count because the invariants were kept verbatim and the skills index
  and pointers were added; the procedures themselves live in the nine
  skills, which is what shrinks per-task context.
- §16 Features: the proposed list was applied with the retry bullet on
  ⏳ (two bullets shared 🔁) and "settings apply live" moved to
  "Available now" since live reload shipped in this change set.
