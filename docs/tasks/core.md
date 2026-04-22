# Vapor core task list

Plan reference: `docs/plans/core.md`
Original source: `docs/plans/original.md`
Related task lists: `docs/tasks/macos.md`, `docs/tasks/cli.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

## Phase C0 - Documentation and naming hygiene (portability foundation)

- [x] C0-1 Save portability plan to `docs/plans/core.md` and task list to
      `docs/tasks/core.md`.
- [x] C0-2 Reorganize `docs/` so common docs stay flat under each group and
      platform-specific docs move to `docs/<group>/<platform>/` (e.g.,
      `docs/architecture/macos/app-lifecycle.md`).
- [x] C0-3 Rename `docs/architecture/xpc-contracts.md` →
      `docs/architecture/ipc-contracts.md` and reframe content as
      transport-agnostic; transport specifics move to
      `docs/architecture/<platform>/ipc-transport.md`.
- [x] C0-4 Add `docs/architecture/platform-abstractions.md` — authoritative
      reference for every trait in `core/platform` and its per-OS
      implementations.
- [x] C0-5 Move `docs/architecture/macos-app-lifecycle.md` →
      `docs/architecture/macos/app-lifecycle.md`.
- [x] C0-6 Move `docs/operations/launchagent-policy.md` →
      `docs/operations/macos/launchagent-policy.md` and
      `docs/operations/distribution-trust-chain.md` →
      `docs/operations/macos/distribution-trust-chain.md`; add a generic
      `docs/operations/distribution-trust-chain.md` that delegates to the
      per-OS docs.
- [x] C0-7 Update `AGENTS.md` §1 (product intent), §2 (system boundaries),
      §2.2 (naming conventions — add `vapor` / `vapord`), §7 (distribution —
      per-platform), §8.2 (toolchain — per-platform), §9 (test matrix — add
      platform matrix), §11 (definition of done — per-OS CI).
- [x] C0-8 Update root `README.md` to describe the portable-runtime model,
      add `vapor` (CLI) to the Structure section, and keep the Features
      section platform-agnostic.
- [x] C0-9 Update per-directory `README.md` files (`core/README.md`,
      `core/daemon/README.md`, `core/providers/README.md`,
      `core/shared/README.md`, `apps/macos/README.md`) to match the new
      architecture vocabulary.
- [x] C0-10 Rename references to "FSEvents callback" that describe generic
      behavior to "fs-watch callback" in `docs/architecture/data-flow.md`,
      `docs/architecture/system-overview.md`, `AGENTS.md`, and
      `core/daemon/README.md`. Keep "FSEvents" where it specifically means the
      macOS API.

Exit gate:

- Docs and naming reflect the portable-runtime + platform-layer model.
- `AGENTS.md` no longer states Vapor is macOS-only; it calls out macOS as
  the first shipping surface.

## Phase C1 - Engine portability fixes (compile on all three OSes)

- [ ] C1-1 `core/shared/src/runtime_paths.rs`: gate Unix-only
      `DirBuilderExt`/`OpenOptionsExt`/`PermissionsExt` calls behind
      `#[cfg(unix)]`; add no-op fallback on Windows or translate to DACL set
      when capabilities warrant. Preserve `0o700`/`0o600` semantics on Unix
      (`PRIVATE_DIRECTORY_MODE` / `PRIVATE_FILE_MODE`).
- [ ] C1-2 `core/shared/src/runtime_paths.rs` + `core/daemon/src/sync_directories.rs`:
      replace `HOME`-only lookup with a cross-platform resolution that honors
      `USERPROFILE` on Windows (and, if adopted, `$XDG_*` on Linux when
      present). Recommended crate: `directories` or `etcetera`.
- [ ] C1-3 `core/daemon/src/state_db.rs`: replace Unix `OsStrExt`/`OsStringExt`
      path-as-bytes encoding with UTF-8 storage (`Path::to_str()` /
      `PathBuf::from(&str)`) or `os_str_bytes`-based portable encoding.
      Pre-GA schema bump is allowed per `AGENTS.md §1.1`; add a migration
      that rewrites stored path bytes to the new encoding on first open.
- [ ] C1-4 `core/daemon/src/fs_events.rs::normalize_absolute_path`: extend
      to preserve `Component::Prefix` so Windows drive-letter paths are
      accepted; optionally strip `\\?\` UNC via `dunce` for user-facing
      paths.
- [ ] C1-5 `core/daemon/Cargo.toml`: move `libc` under
      `[target.'cfg(unix)'.dependencies]` once C3-7 (signal abstraction)
      lands.
- [ ] C1-6 Add `windows-latest` and `ubuntu-latest` Rust jobs to
      `.github/workflows/lint.yml` and `test.yml` for `cargo build --workspace`,
      `cargo clippy`, and `cargo test` on `core/*` crates only (Swift stays
      macOS-only). Gate branch protection on the new jobs.
- [ ] C1-7 Update `docs/ci/overview.md` and `docs/ci/required-checks.md`
      with the new per-OS matrix.

Exit gate:

- `cargo build --workspace` + `cargo test` succeed on macOS, Linux, Windows
  CI jobs with no `cfg` hacks in engine code.

## Phase C2 - Close remaining runtime gaps (from prior Phase 2.5 work)

Moved from the pre-portability macOS task list; they are platform-agnostic
runtime work.

- [ ] C2-1 Replace default-`ThrottleInputs` placeholder in the composed
      runtime tick path with a real input source. Until
      `PlatformMetricsSampler` (Phase C4) is implemented, inject a
      `StaticMetricsSampler` driven by config so the tick path already
      exercises real plumbing.
- [ ] C2-2 D-1 (from legacy tasklist) — Harden `ThrottleWorkgate` permit-ID
      allocation against `u64::MAX` saturation: switch to wrapping
      allocation with a free-list of released ids (or wrap when the
      active-permits map shows the slot is free); add a stress test that
      walks past the boundary in-process.
- [ ] C2-3 D-2 (from legacy tasklist) — Migrate local elapsed-time
      measurements in the daemon runtime from `SystemTime` to `Instant` so
      wall-clock rewinds cannot affect tick cadence, slice budgets, throttle
      sampling, or staged-executor timing. Introduce a test-injectable clock
      abstraction on `DebounceLoop`, `ReconcileController`, `DaemonRuntime`,
      and `StagedExecutor`; keep `SystemTime` for durable/crossing-process
      fields.
- [ ] C2-4 D-3 (from legacy tasklist) — Add hysteresis and min-dwell to
      `ThrottleController::evaluate` itself so state does not flap when
      input metrics oscillate. Suggest 5s for `Light`/`Throttled`, 1s for
      `Suspended`. Regression test: oscillating CPU samples must not flip
      state more than once per `MIN_DWELL_*_SECONDS`.

Exit gate:

- The composed runtime loop is fed by a real input source (config or
  sampler) instead of defaults.
- Throttle controller is stable under oscillating inputs.
- Durable time handling is correct across wall-clock rewind.

## Phase C3 - Platform abstraction layer (`core/platform` crate)

Introduce the crate + trait skeletons + macOS native implementations ported
from existing Swift/docs. Windows/Linux impls land later (Phase C6/C7).

- [ ] C3-1 Add `core/platform` crate to the Cargo workspace. Module
      structure per `docs/plans/core.md §2`: `fs_watch/`, `service/`,
      `secrets/`, `metrics/`, `idle/`, `fs_caps/`, `process/`.
- [ ] C3-2 Define trait `FsWatcher` (start/stop a recursive watch on a
      canonical root; emit normalized `Created`/`Modified`/`Removed`/
      `Renamed` events). Initial macOS implementation wraps the existing
      `notify::RecommendedWatcher` code from `core/daemon/src/fs_events.rs`;
      do not regress callback discipline. Add an in-memory fake for unit
      tests.
- [ ] C3-3 Define trait `ServiceInstaller` (`install_and_enable`,
      `disable_and_uninstall`, `start_daemon`, `stop_daemon`, `is_installed`,
      `is_running`, `status`). Port the macOS LaunchAgent logic from
      `apps/macos/Sources/VaporCore/LaunchAgentController.swift` to
      `core/platform/service/macos.rs` using `launchctl` via
      `std::process::Command`. Keep the exact plist schema from
      `docs/operations/macos/launchagent-policy.md`.
- [ ] C3-4 Define trait `SecretStore` (`get`, `set`, `delete`, `list`).
      macOS implementation via `security-framework` (Keychain) or `keyring`
      crate with macOS backend. Add in-memory fake for tests.
- [ ] C3-5 Define trait `PlatformMetricsSampler` that returns
      `ThrottleInputs`. Add `StaticMetricsSampler` (config-driven) for the
      CLI / headless / test case. macOS implementation via `mach2` +
      `IOKit` / FFI-bridged `NSProcessInfo` signals (thermal, low-power
      mode); battery via `IOPSCopyPowerSourcesInfo`; CPU via
      `host_statistics64` / `task_info`.
- [ ] C3-6 Define trait `IdleNotifier`. macOS implementation via
      `CGEventSourceSecondsSinceLastEventType`. Add `AlwaysIdleNotifier` for
      headless/test case.
- [ ] C3-7 Define trait `ProcessSupervisor` (`register_shutdown_handler`).
      Port the existing `SIGTERM`/`SIGINT` handlers from
      `core/daemon/src/main.rs` to `signal-hook`-based handlers on Unix.
      Leave Windows impl stubbed to `unimplemented!()` until Phase C6.
- [ ] C3-8 Define trait `FilesystemCapabilities` (`supports_xattr`,
      `case_sensitive_by_default`, `metadata_store_api`). macOS implementation
      via `xattr` crate + APFS/HFS+ case-sensitivity detection.
- [ ] C3-9 Wire the daemon main loop (`core/daemon/src/main.rs` +
      `runtime.rs`) to accept trait implementations via dependency injection
      instead of referring to platform APIs directly. macOS builds stay
      behaviorally identical.
- [ ] C3-10 Author `docs/architecture/platform-abstractions.md`: trait list,
      contract, expected per-OS native API, test fake, and the parity matrix
      from `docs/plans/core.md §9`.

Exit gate:

- `core/platform` compiles on all three OSes (macOS-native impls work;
  Windows/Linux impls stubbed to `unimplemented!()` but compile).
- macOS daemon behavior is identical before and after the refactor.

## Phase C4 - Daemon lifecycle moves into Rust (`core/lifecycle`)

- [ ] C4-1 Add `core/lifecycle` crate to the workspace.
- [ ] C4-2 Port `CrashLoopGuard` from
      `apps/macos/Sources/VaporCore/DaemonLifecycle.swift` to Rust verbatim
      (same policy: `baseDelay=2s`, `maxDelay=120s`,
      `maxConsecutiveFailuresBeforePause=5`, `failureWindow=600s`). Keep the
      `CrashLoopPaused` semantics.
- [ ] C4-3 Port `DaemonLifecycleManager` to Rust. Consumes
      `core/platform/service::ServiceInstaller`. Keeps `autoLaunch` policy
      semantics from the existing Swift store.
- [ ] C4-4 Introduce `AutoLaunchSettingStore` in Rust reading/writing the
      `autoLaunch` field in `vapor.json`. Swift + Rust share the same file
      atomically (cross-process safe write).
- [ ] C4-5 Expose a stable C-ABI (`extern "C"`) or JSON-RPC-over-stdio
      surface so the macOS Swift app can invoke the Rust lifecycle layer.
      Recommended: Swift app invokes `vapor service …` as a subprocess
      (avoids FFI lifecycle complexity). Swift keeps its
      `LaunchAgentControlling` protocol but the default implementation now
      calls the CLI.
- [ ] C4-6 Parity tests: `DaemonLifecycleManagerTests` that previously ran
      in Swift must pass against the Rust implementation (or an equivalent
      Rust-side test matrix).
- [ ] C4-7 Remove the duplicate Swift lifecycle logic once parity is proven;
      leave the Swift protocol as a thin shim over the Rust layer.

Exit gate:

- `core/lifecycle` owns `CrashLoopGuard` and `DaemonLifecycleManager`.
- Swift app delegates lifecycle orchestration to `core/lifecycle` via the
  `vapor` CLI (or FFI).
- macOS behavior unchanged end-to-end.

## Phase C5 - IPC channel between apps and daemon

- [ ] C5-1 Pick and document the IPC transport: Unix domain socket at
      `<vapor_dir>/vapord.sock` on Unix; named pipe
      `\\.\pipe\vapord-<user-sid>` on Windows. Protocol: JSON-RPC 2.0,
      length-prefixed frames. Finalize `docs/architecture/ipc-contracts.md`
      and the per-OS transport docs.
- [ ] C5-2 Implement the daemon-side IPC server in `core/daemon` with the
      versioning/handshake/skew-matrix discipline already specified (`Hello`
      / `HelloAck` / `IncompatibleVersion`; `schema_version`; unknown-field
      tolerance; `payload_bytes` bound at `XPC_MAX_PAYLOAD_BYTES` — rename
      to `IPC_MAX_PAYLOAD_BYTES`).
- [ ] C5-3 Implement the client library in `core/shared` (or a new
      `core/ipc` crate) that both the `vapor` CLI and future apps consume.
- [ ] C5-4 Wire the status/control endpoints: `Status`, `Pause`, `Resume`,
      `FlushNow`, `Reconcile`, `Timeline`, `AutoLaunchToggle`,
      `ConfigUpdate` (subset — full surface aligns with
      `docs/architecture/ipc-contracts.md`).
- [ ] C5-5 Add IPC integration tests: app ↔ daemon skew matrix
      (`N ↔ N`, `N ↔ N-1`, `N-1 ↔ N`, `|N-M|=2` rejected), field-omission
      defaults, payload-bounds rejection.

Exit gate:

- macOS Swift app, `vapor` CLI, and future Windows/Linux apps all speak the
  same IPC protocol.

## Phase C6 - Windows platform implementations

- [ ] C6-1 `core/platform/fs_watch/windows.rs`: `ReadDirectoryChangesW` with
      IOCP. Prefer `notify` as MVP; switch to direct `windows` crate when
      buffer sizing or rename-pair semantics need tuning. Rename pair
      handling (`FILE_ACTION_RENAMED_OLD_NAME` + `_NEW_NAME`) verified.
- [ ] C6-2 `core/platform/service/windows.rs`: Task Scheduler impl
      (per-user, `AtLogOn` trigger, `RestartOnFailure`) via `ITaskService`
      through the `windows` crate. System-wide variant via
      `windows-service` + SCM behind `--system`.
- [ ] C6-3 `core/platform/secrets/windows.rs`: Credential Manager via
      `keyring` or `windows` crate (`CredWriteW` / `CredReadW`).
- [ ] C6-4 `core/platform/metrics/windows.rs`: `GetSystemTimes` +
      `GetProcessTimes`, `GetSystemPowerStatus`,
      `CallNtPowerInformation(SystemPowerInformation)`, and metered
      connection awareness via `NotifyNetworkConnectivityHintChange`.
- [ ] C6-5 `core/platform/idle/windows.rs`: `GetLastInputInfo` polled on
      the 1s throttle cadence.
- [ ] C6-6 `core/platform/fs_caps/windows.rs`: NTFS Alternate Data Streams
      via `CreateFileW` with `filename:streamname`; side-file fallback on
      ReFS/FAT; case-insensitive-by-default detection.
- [ ] C6-7 `core/platform/process/windows.rs`: `SetConsoleCtrlHandler` for
      interactive; `SERVICE_STOP` via `windows-service` for service runs;
      `WM_ENDSESSION` for logout.
- [ ] C6-8 Windows distribution trust chain doc:
      `docs/operations/windows/distribution-trust-chain.md` + task-scheduler
      policy in `docs/operations/windows/scheduled-task-policy.md`.

Exit gate:

- `vapor run`, `vapor service install` on Windows work end-to-end.
- Windows CI job runs the full `core/*` test suite including platform
  impls.

## Phase C7 - Linux platform implementations

- [ ] C7-1 `core/platform/fs_watch/linux.rs`: `inotify` (user) MVP; optional
      `fanotify` variant behind `CAP_SYS_ADMIN` for system-wide scenarios.
      `vapor doctor` hooks for watch-limit detection
      (`/proc/sys/fs/inotify/max_user_watches`).
- [ ] C7-2 `core/platform/service/linux.rs`: systemd user unit
      (`~/.config/systemd/user/vapord.service`) + `systemctl --user …`;
      system unit (`/etc/systemd/system/vapord.service`) under `--system`.
      Optional `loginctl enable-linger` prompt when the user wants sync
      while logged out.
- [ ] C7-3 `core/platform/secrets/linux.rs`: `secret-service` / libsecret
      D-Bus as default; `age`-encrypted file at `<vapor_dir>/secrets.age`
      fallback for headless hosts; `--secrets-backend=command` shim for
      external tools.
- [ ] C7-4 `core/platform/metrics/linux.rs`: `/proc/stat`,
      `/proc/self/stat`, `/sys/class/power_supply/*`,
      `/proc/pressure/{cpu,io,memory}` (PSI), `/proc/net/dev`. Optional
      NetworkManager D-Bus `NM-metered` integration when present.
- [ ] C7-5 `core/platform/idle/linux.rs`: X11 `XScreenSaverQueryInfo`;
      Wayland `org.freedesktop.ScreenSaver` or `ext-idle-notify-v1`;
      headless hosts report always-idle.
- [ ] C7-6 `core/platform/fs_caps/linux.rs`: native xattr on ext4/xfs/btrfs;
      side-file fallback on filesystems without xattr support; case-
      sensitivity probe.
- [ ] C7-7 Linux distribution trust chain doc:
      `docs/operations/linux/distribution-trust-chain.md` + systemd unit
      policy in `docs/operations/linux/systemd-unit-policy.md`.

Exit gate:

- `vapor run`, `vapor service install` on Linux work end-to-end.
- Linux CI job runs the full `core/*` test suite including platform impls.

## Phase C8 - Port / finish runtime capabilities on the portable stack

Continues the pre-portability macOS task list's Phase 3–Phase 10 work —
still part of the macOS MVP, but implemented in portable Rust so every app
inherits it.

### Filesystem reference provider and bidirectional runtime shell

- [ ] C8-1 Provider trait surface + provider-neutral error taxonomy in
      `core/shared`. Full trait: `enumerate`, `stat`, `upload`, `download`,
      `delete`, `rename`, and a changes-feed producer gated by
      `ProviderCapabilities::supports_remote_changes_feed`. Error taxonomy:
      `Transient`, `RateLimited`, `Authentication`, `PreconditionFailed`,
      `NotFound`, `Permanent`.
- [ ] C8-2 `provider` config field (`filesystem` default, `google_drive`
      accepted but inert). Document `cloudSyncDirectory` reinterpretation
      when `provider = "filesystem"`. Mirror constant in
      `apps/macos/Sources/VaporCore/VaporConstants.swift` and
      `core/shared/src/constants.rs`.
- [ ] C8-3 `core/providers/src/filesystem/`: full `Provider` implementation
      backed by local filesystem; atomic writes via temp-file-plus-rename;
      op-id tagging via `FilesystemCapabilities` metadata API (xattr on
      macOS/Linux, ADS on Windows, side-file fallback); strict scope
      enforcement (refuses symlink escape / relative traversal / device
      crossing).
- [ ] C8-4 Filesystem-backed remote changes feed using
      `core/platform/fs_watch` on the remote root, with the same
      callback discipline and a monotonic cursor persisted in the durable
      state DB.
- [ ] C8-5 Replace the Phase 2.5 timed staged-executor simulator with real
      planner/hash/upload/download workers driven by the provider; add
      `WorkClass::Download`; preserve slice-budget interruptibility.
- [ ] C8-6 Remote-to-local apply pipeline (`IntentSource::Remote`);
      cursor advance persisted only on durable intent completion.
- [ ] C8-7 `self_write_cache` runtime implementation consuming the existing
      constants in `core/shared::constants::self_write_cache`. Op-id primary
      + content-hash fallback; xattr/ADS primary + side-file fallback; LRU-
      on-insert; TTL expiry on 1s tick; memory-pressure floors; provider
      hides side-files from enumeration.
- [ ] C8-8 Ensure-remote-root semantics in the filesystem provider (mirrors
      local-root auto-create); invalid remote root surfaces actionable
      configuration error.
- [ ] C8-9 Wire provider selection in `core/daemon/src/main.rs` to read the
      resolved `provider` kind from config; default `filesystem` pre-GA;
      keep `GoogleDriveProvider` compiled but inert.
- [ ] C8-10 Integration tests for local→remote, remote→local, self-write
      loop prevention, restart recovery with in-flight work, throttle
      transitions during real work, and scope safety under escape inputs.
- [ ] C8-11 Microbench/regression coverage for provider-backed execution:
      10k-file fixture within engine budgets; no admission serialization
      under burst; remote-apply cost scales linearly.
- [ ] C8-12 Happy-path bidirectional race smoke test: concurrent
      local+remote writes converge within 30s across 5 runs with either
      canonical-path landing or `~conflict-pending-{intent_id}` staging.
- [ ] C8-13 Phase 2.5 simulator removal validation (gate for Phase C8.2):
      no `stage_duration` placeholders; every `WorkClass` does real work;
      no simulator-shaped functions in production paths.

### Bidirectional safety, conflicts, deletion semantics

- [ ] C8-14 Bidirectional conflict policy (`keep both` suffix template
      `{stem}~conflict-{device_id}-{timestamp_ms}{ext}` with `-{seq}`
      collision-avoidance; "data preservation wins over deletion"). Replace
      the Phase C8-12 `~conflict-pending-{intent_id}` scaffolding.
- [ ] C8-15 `deviceId` field in `VaporConfiguration` (Swift) and durable
      state (Rust): derive from `gethostname()` normalized to `[a-z0-9-]`
      (length-capped at 32, UUIDv4-truncated-to-12 fallback); persist
      immediately; never silently regenerate.
- [ ] C8-16 Tombstone/delete reconciliation with restart-safe replay.
- [ ] C8-17 Deterministic race resolution for simultaneous edits,
      rename+modify, delete/restore.
- [ ] C8-18 Bidirectional race integration tests + corruption-recovery
      validation.

### Profile model, multi-provider accounts, settings overrides

- [ ] C8-19 Durable profile model (`profile_id`, display name, provider
      kind, account identity, enabled state); classify settings into
      app-global vs profile-override-capable.
- [ ] C8-20 Namespace `SecretStore` entries, auth refresh state, and
      provider connection metadata by profile/account.
- [ ] C8-21 Profile-scoped override resolution for sync-affecting settings;
      keep global-only settings singular. The mechanism built here is reused
      by auto-tuning (C8-32) to layer `resourceLimits` and `idleBoost`.
- [ ] C8-22 Multiple enabled profiles concurrently (same local root fan-out,
      different local roots, per-profile debounce/scheduler/durable-queue).
- [ ] C8-23 Deduplicate shared local-root watches: one `FsWatcher` per
      canonical realpath with per-callback fan-out into per-profile ingest
      queues tagged with `profile_id`.
- [ ] C8-24 Blast-radius containment: profile runtimes spawned under panic
      catchers; a failed profile suspends only its queue.
- [ ] C8-25 Safe profile disconnect/delete flows.
- [ ] C8-26 Integration/perf tests for override resolution, multi-provider
      fan-out, parallel sync, restart recovery, multi-profile budgets.

### IPC / diagnostics UX

- [ ] C8-27 Finalize IPC schema (continues C5) with every control endpoint
      (`Pause/Resume`, `FlushNow`, `AutoLaunchToggle`, excludes updates).
- [ ] C8-28 Full menubar state model and reasoned status messages (consumed
      by every app surface; provided via IPC).
- [ ] C8-29 Per-intent "why stuck" diagnostics exposing `intent_id`,
      `profile_id`, `path`, `action`, `stage`, elapsed-in-stage,
      attempt count, last-error class, `blocker_reason`. Also surface
      `BoundedFsEventRecorder::dropped_incoming_event_count`.
- [ ] C8-30 Daemon activity event stream for timeline (bounded in-memory,
      configurable max length, default `1000`).
- [ ] C8-31 Tests for timeline ordering/truncation/skew/field-omission.

### Auto-tuning and user resource-budget enforcement

- [ ] C8-32 Add `resourceLimits` config group (`cpuPercent` default `15`,
      `memoryPercent` default `10`, `bandwidthPercent` default `25`; all
      ranges `1..100`). Clamp invalid values with classified warning.
- [ ] C8-33 Add `idleBoost` config group (defaults per plan); require
      `boost*Percent >= resourceLimits.*Percent` at config-load.
- [ ] C8-34 Extend C8-21 override resolution to cover `resourceLimits` and
      `idleBoost` via MIN-lowering semantics; any enabled profile with
      `idleBoost.enabled = false` disables boost daemon-wide.
- [ ] C8-35 Document both groups in root `README.md` **Configuration**
      table and in `docs/architecture/data-flow.md` under "User resource
      budgets".
- [ ] C8-36 `ResourceBudget` runtime component: resolve effective ceilings
      each tick; consume `PlatformMetricsSampler` + `IdleNotifier`; run
      idle-boost state machine; publish caps + reason codes.
- [ ] C8-37 Extend workgate to consume `ResourceBudget` effective ceilings
      (CPU-ceiling-derived cap interacts with throttle caps via MIN;
      in-flight work yields at next slice checkpoint when cap drops).
- [ ] C8-38 Provider-neutral bandwidth shaper in `core/providers` (bytes/s
      token bucket). Rate driven by effective `bandwidthPercent` ceiling
      against measured link capacity.
- [ ] C8-39 Memory-ceiling enforcement: bounded caches + intent maps react
      to RSS; `self_write_cache` TTL shortens, timeline buffers trim, storm
      compaction thresholds lower; knobs bounded with hysteresis.
- [ ] C8-40 Expose effective ceilings + current utilization + idle-boost
      state + human reason through IPC diagnostics; render in every app's
      diagnostics surface.
- [ ] C8-41 Integration tests per `docs/architecture/data-flow.md` §"Ceiling
      transitions" and `docs/performance/acceptance-budgets-and-benchmark-
      harness.md` §"SLO applicability under user resource ceilings".
- [ ] C8-42 Auto-tuning loop (60-120s cadence; one small change per cycle;
      tune priority: impact → rate-limit avoidance → latency; hysteresis +
      rollback-on-regression; bounded by effective ceilings).

### Provider-system extensibility hardening

- [ ] C8-43 Finalize provider capability model and trait boundaries.
- [ ] C8-44 Provider contract tests with reference/mock provider across
      semantics (iCloud/S3/R2/Proton Drive-style constraints).
- [ ] C8-45 Compatibility validation for bidirectional flows, conflicts,
      tombstones, retries, throttle behavior through abstractions.
- [ ] C8-46 Performance checks for provider-adapter overhead.
- [ ] C8-47 Provider-onboarding checklist + acceptance criteria docs.

### Google Drive provider (first external cloud target)

- [ ] C8-48 `provider_gdrive` OAuth (PKCE), token storage via
      `core/platform/secrets`, refresh handling per
      `docs/operations/provider-auth-operations.md`.
- [ ] C8-49 Authenticated Google Drive folder lookup/create for
      `cloudSyncDirectory` before sync starts.
- [ ] C8-50 Block normal sync until the cloud root exists or the provider
      returns an actionable initialization error.
- [ ] C8-51 Upload paths (multipart small, resumable large) behind the
      provider trait; chunked retry; rate-limit-aware.
- [ ] C8-52 Remote changes polling via the Google Drive changes endpoint;
      adaptive cadence + request budgeting tied to throttle state.
- [ ] C8-53 Provider metadata caching + resumable upload chunk auto-sizing.
- [ ] C8-54 Flip `GoogleDriveProvider` from inert to selectable via
      `provider` config, gated on C8-44 contract tests.

### Optional advanced safeguards

- [ ] C8-55 Active-coding detection (permissioned) with heuristic fallback.
- [ ] C8-56 Folder priority classes + temporary flush boost.
- [ ] C8-57 Mass-change / ransomware guard with pause + alert workflow.
- [ ] C8-58 Diagnostics history + support export bundle.

## Phase C9 - `vapor` CLI delivery

Tracked separately in `docs/tasks/cli.md`; this phase is informational here
and the acceptance gate for cross-platform parity.

## Cross-phase mandatory validation

- [ ] T-1 Crash/restart during active sync resumes without lost intent on
      every supported OS.
- [ ] T-2 Throttle transitions follow battery/thermal/load/network pressure
      correctly (macOS/Windows/Linux samplers each verified).
- [ ] T-3 Fs-watch callback remains lightweight on every OS
      (no DB/hash/network; p99 under budget).
- [ ] T-4 Self-write echo suppression works in bidirectional paths
      cross-OS (xattr + ADS + side-file fallback).
- [ ] T-5 Conflict policy verified under simultaneous local/remote edits
      cross-OS.
- [ ] T-6 Security validation: native secret store only, redacted logs,
      private-mode dirs where OS supports them.
- [ ] T-7 Upgrade compatibility validation across app/daemon/schema versions
      on every OS.
- [ ] T-8 CI parity validation: PR lint/test/build match local script entry
      points on every matrix OS.
- [ ] T-9 Scope safety validation: watcher only watches configured local
      sync root; no escalation to full-device scan on any OS.
- [ ] T-10 Ignore-rule precedence validation.
- [ ] T-11 Performance SLO validation per-OS (per-OS absolute numbers;
      ratios consistent).
- [ ] T-12 Memory/backpressure validation under storm-scale workloads.
- [ ] T-13 Auto-tuning stability validation (no oscillation;
      rollback-on-regression).
- [ ] T-14 Multi-profile isolation validation cross-OS.
- [ ] T-15 Autolaunch validation per OS: `vapor service install` +
      `vapor run` + `vapor status` full round-trip passes on macOS
      (LaunchAgent), Windows (Task Scheduler / SCM), Linux (systemd user /
      system).

## Deferred tasks

- [ ] PT-1 Tune `./scripts/perf.sh` smoke thresholds using real CI/release
      baseline history once per-OS baselines exist.
- [ ] O-1 Design and implement the production onboarding flow
      (information architecture, step sequence, copy, UX states). When this
      starts, run a clarification pass with the project owner to define the
      onboarding structure before implementation.
