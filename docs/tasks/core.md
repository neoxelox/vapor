# Vapor core task list

Plan reference: `docs/plans/core.md`
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

- [x] C1-1 `core/shared/src/runtime_paths.rs`: gate Unix-only
      `DirBuilderExt`/`OpenOptionsExt`/`PermissionsExt` calls behind
      `#[cfg(unix)]`; add no-op fallback on Windows or translate to DACL set
      when capabilities warrant. Preserve `0o700`/`0o600` semantics on Unix
      (`PRIVATE_DIRECTORY_MODE` / `PRIVATE_FILE_MODE`).
- [x] C1-2 `core/shared/src/runtime_paths.rs` + `core/daemon/src/sync_directories.rs`:
      replace `HOME`-only lookup with a cross-platform resolution that honors
      `USERPROFILE` on Windows (and, if adopted, `$XDG_*` on Linux when
      present). Recommended crate: `directories` or `etcetera`.
- [x] C1-3 `core/daemon/src/state_db.rs`: replace Unix `OsStrExt`/`OsStringExt`
      path-as-bytes encoding with UTF-8 storage (`Path::to_str()` /
      `PathBuf::from(&str)`) or `os_str_bytes`-based portable encoding.
      Pre-GA schema bump is allowed per `AGENTS.md §1.1`; add a migration
      that rewrites stored path bytes to the new encoding on first open.
- [x] C1-4 `core/daemon/src/fs_events.rs::normalize_absolute_path`: extend
      to preserve `Component::Prefix` so Windows drive-letter paths are
      accepted; optionally strip `\\?\` UNC via `dunce` for user-facing
      paths.
- [x] C1-5 `core/daemon/Cargo.toml`: move `libc` under
      `[target.'cfg(unix)'.dependencies]` once C3-7 (signal abstraction)
      lands. (Closed alongside C3-7 — `core/daemon` no longer depends on
      `libc` directly; `core/platform/process` owns the Unix
      `signal-hook` integration and the cfg-gated `libc` dep.)
- [x] C1-6 Add `windows-latest` and `ubuntu-latest` Rust jobs to
      `.github/workflows/lint.yml` and `test.yml` for `cargo build --workspace`,
      `cargo clippy`, and `cargo test` on `core/*` crates only (Swift stays
      macOS-only). Gate branch protection on the new jobs.
- [x] C1-7 Update `docs/ci/overview.md` and `docs/ci/required-checks.md`
      with the new per-OS matrix.

Exit gate:

- `cargo build --workspace` + `cargo test` succeed on macOS, Linux, Windows
  CI jobs with no `cfg` hacks in engine code.

## Phase C2 - Close remaining runtime gaps (from prior Phase 2.5 work)

Moved from the pre-portability macOS task list; they are platform-agnostic
runtime work.

- [x] C2-1 Replace default-`ThrottleInputs` placeholder in the composed
      runtime tick path with a real input source. Until
      `PlatformMetricsSampler` (Phase C4) is implemented, inject a
      `StaticMetricsSampler` driven by config so the tick path already
      exercises real plumbing.
- [x] C2-2 D-1 (from legacy tasklist) — Harden `ThrottleWorkgate` permit-ID
      allocation against `u64::MAX` saturation: switch to wrapping
      allocation with a free-list of released ids (or wrap when the
      active-permits map shows the slot is free); add a stress test that
      walks past the boundary in-process.
- [x] C2-3 D-2 (from legacy tasklist) — Migrate local elapsed-time
      measurements in the daemon runtime from `SystemTime` to `Instant` so
      wall-clock rewinds cannot affect tick cadence, slice budgets, throttle
      sampling, or staged-executor timing. Introduce a test-injectable clock
      abstraction on `DebounceLoop`, `ReconcileController`, `DaemonRuntime`,
      and `StagedExecutor`; keep `SystemTime` for durable/crossing-process
      fields.
- [x] C2-4 D-3 (from legacy tasklist) — Add hysteresis and min-dwell to
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

- [x] C3-1 Add `core/platform` crate to the Cargo workspace. Module
      structure per `docs/plans/core.md §2`: `fs_watch/`, `service/`,
      `secrets/`, `metrics/`, `idle/`, `fs_caps/`, `process/`.
- [x] C3-2 Define trait `FsWatcher` (start/stop a recursive watch on a
      canonical root; emit normalized `Created`/`Modified`/`Removed`/
      `Renamed` events). Initial macOS implementation wraps the existing
      `notify::RecommendedWatcher` code from `core/daemon/src/fs_events.rs`;
      do not regress callback discipline. Add an in-memory fake for unit
      tests.
- [x] C3-3 Define trait `ServiceInstaller` (`install_and_enable`,
      `disable_and_uninstall`, `start_daemon`, `stop_daemon`, `is_installed`,
      `is_running`, `status`). Port the macOS LaunchAgent logic from
      `apps/macos/Sources/VaporCore/LaunchAgentController.swift` to
      `core/platform/service/macos.rs` using `launchctl` via
      `std::process::Command`. Keep the exact plist schema from
      `docs/operations/macos/launchagent-policy.md`.
- [x] C3-4 Define trait `SecretStore` (`get`, `set`, `delete`, `list`).
      macOS implementation via `security-framework` (Keychain) or `keyring`
      crate with macOS backend. Add in-memory fake for tests. *(Wave 4
      ships the trait + in-memory store; the Keychain bridge lands with
      Wave 5 / C4-5.)*
- [x] C3-5 Define trait `PlatformMetricsSampler` that returns
      `ThrottleInputs`. Add `StaticMetricsSampler` (config-driven) for the
      CLI / headless / test case. macOS implementation via `mach2` +
      `IOKit` / FFI-bridged `NSProcessInfo` signals. *(Wave 4 ships the
      trait + `StaticPlatformMetricsSampler`; the mach2 / IOKit bridge
      lands incrementally as Wave 4 follow-ups.)*
- [x] C3-6 Define trait `IdleNotifier`. macOS implementation via
      `CGEventSourceSecondsSinceLastEventType`. Add `AlwaysIdleNotifier` for
      headless/test case. *(Wave 4 ships the trait + `AlwaysIdleNotifier`;
      the CGEvent bridge lands with the C8 active-coding-detection work.)*
- [x] C3-7 Define trait `ProcessSupervisor` (`register_shutdown_handler`).
      Port the existing `SIGTERM`/`SIGINT` handlers from
      `core/daemon/src/main.rs` to `signal-hook`-based handlers on Unix.
      Leave Windows impl stubbed to `unimplemented!()` until Phase C6.
- [x] C3-8 Define trait `FilesystemCapabilities` (`supports_xattr`,
      `case_sensitive_by_default`, `metadata_store_api`). macOS implementation
      via `xattr` crate + APFS/HFS+ case-sensitivity detection. *(Wave 4
      ships the trait + per-OS compile-time defaults from the parity
      matrix; the runtime xattr / ADS probes land alongside C8.)*
- [x] C3-9 Wire the daemon main loop (`core/daemon/src/main.rs` +
      `runtime.rs`) to accept trait implementations via dependency injection
      instead of referring to platform APIs directly. macOS builds stay
      behaviorally identical. *(Wave 4 wires `NativeProcessSupervisor`
      into `main.rs`; remaining traits get consumed alongside the C8
      runtime expansion.)*
- [x] C3-10 Author `docs/architecture/platform-abstractions.md`: trait list,
      contract, expected per-OS native API, test fake, and the parity matrix
      from `docs/plans/core.md §9`.

Exit gate:

- `core/platform` compiles on all three OSes (macOS-native impls work;
  Windows/Linux impls stubbed to `unimplemented!()` but compile).
- macOS daemon behavior is identical before and after the refactor.

## Phase C4 - Daemon lifecycle moves into Rust (`core/lifecycle`)

- [x] C4-1 Add `core/lifecycle` crate to the workspace.
- [x] C4-2 Port `CrashLoopGuard` from
      `apps/macos/Sources/VaporCore/DaemonLifecycle.swift` to Rust verbatim
      (same policy: `baseDelay=2s`, `maxDelay=120s`,
      `maxConsecutiveFailuresBeforePause=5`, `failureWindow=600s`). Keep the
      `CrashLoopPaused` semantics.
- [x] C4-3 Port `DaemonLifecycleManager` to Rust. Consumes
      `core/platform/service::ServiceInstaller`. Keeps `autoLaunch` policy
      semantics from the existing Swift store.
- [x] C4-4 Introduce `AutoLaunchSettingStore` in Rust reading/writing the
      `autoLaunch` field in `vapor.json`. Swift + Rust share the same file
      atomically (cross-process safe write).
- [x] C4-5 Expose a stable C-ABI (`extern "C"`) or JSON-RPC-over-stdio
      surface so the macOS Swift app can invoke the Rust lifecycle layer.
      Recommended: Swift app invokes `vapor service …` as a subprocess
      (avoids FFI lifecycle complexity). Swift keeps its
      `LaunchAgentControlling` protocol but the default implementation now
      calls the CLI. *(Shipped as the subprocess route: every `vapor
      service` subcommand takes `--json` and renders the stable contract
      locked by the `json_contract_*` tests in
      `core/cli/src/commands/service.rs`; the Swift
      `VaporCLIServiceController` decodes it.)*
- [x] C4-6 Parity tests: `DaemonLifecycleManagerTests` that previously ran
      in Swift must pass against the Rust implementation (or an equivalent
      Rust-side test matrix).
- [x] C4-7 Remove the duplicate Swift lifecycle logic once parity is proven;
      leave the Swift protocol as a thin shim over the Rust layer.
      *(Swift `CrashLoopGuard` / `CrashLoopPolicy` / launchctl plumbing /
      `AutoLaunchSettingStore` deleted; `DaemonLifecycleManager` is a thin
      facade over the CLI-backed `LaunchAgentControlling` seam.)*

Exit gate:

- `core/lifecycle` owns `CrashLoopGuard` and `DaemonLifecycleManager`.
- Swift app delegates lifecycle orchestration to `core/lifecycle` via the
  `vapor` CLI (or FFI).
- macOS behavior unchanged end-to-end.

## Phase C5 - IPC channel between apps and daemon

- [x] C5-1 Pick and document the IPC transport: Unix domain socket at
      `<vapor_dir>/vapord.sock` on Unix; named pipe
      `\\.\pipe\vapord-<user-sid>` on Windows. Protocol: JSON-RPC 2.0,
      length-prefixed frames. Finalize `docs/architecture/ipc-contracts.md`
      and the per-OS transport docs.
- [x] C5-2 Implement the daemon-side IPC server in `core/daemon` with the
      versioning/handshake/skew-matrix discipline already specified (`Hello`
      / `HelloAck` / `IncompatibleVersion`; `schema_version`; unknown-field
      tolerance; `payload_bytes` bound at `IPC_MAX_PAYLOAD_BYTES`).
- [x] C5-3 Implement the client library in `core/shared` (or a new
      `core/ipc` crate) that both the `vapor` CLI and future apps consume.
      *(Lands in a new `core/ipc` crate.)*
- [x] C5-4 Wire the status/control endpoints: `Status`, `Pause`, `Resume`,
      `FlushNow`, `Reconcile`, `Timeline`, `AutoLaunchToggle`,
      `ConfigUpdate` (subset — full surface aligns with
      `docs/architecture/ipc-contracts.md`). *(Wave 6 phase 2 ships
      `Status` only; the control endpoints land alongside the L3
      consumers in Wave 7.)*
- [x] C5-5 Add IPC integration tests: app ↔ daemon skew matrix
      (`N ↔ N`, `N ↔ N-1`, `N-1 ↔ N`, `|N-M|=2` rejected), field-omission
      defaults, payload-bounds rejection.

Exit gate:

- macOS Swift app, `vapor` CLI, and future Windows/Linux apps all speak the
  same IPC protocol.

## Phase C6 - Windows platform implementations

**Status: deferred / optional.** Gated on the project owner explicitly
opting into a Windows surface. Nothing in the primary path (core +
macOS app + CLI-on-macOS) is blocked by this phase. See
`docs/tasks/README.md` wave 12.

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

**Status: deferred / optional.** Gated on the project owner explicitly
opting into a Linux surface. Nothing in the primary path (core +
macOS app + CLI-on-macOS) is blocked by this phase. See
`docs/tasks/README.md` wave 13.

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

- [x] C8-1 Provider trait surface + provider-neutral error taxonomy in
      `core/shared`. Full trait: `enumerate`, `stat`, `upload`, `download`,
      `delete`, `rename`, and a changes-feed producer gated by
      `ProviderCapabilities::supports_remote_changes_feed`. Error taxonomy:
      `Transient`, `RateLimited`, `Authentication`, `PreconditionFailed`,
      `NotFound`, `Permanent`.
- [x] C8-2 `provider` config field (`filesystem` default, `gdrive`
      accepted but inert). Document `cloudSyncDirectory` reinterpretation
      when `provider = "filesystem"`. Mirror constant in
      `apps/macos/Sources/VaporCore/VaporConstants.swift` and
      `core/shared/src/constants.rs`.
- [x] C8-3 `core/providers/src/filesystem/`: full `Provider` implementation
      backed by local filesystem; atomic writes via temp-file-plus-rename;
      op-id tagging via `FilesystemCapabilities` metadata API (xattr on
      macOS/Linux, ADS on Windows, side-file fallback); strict scope
      enforcement (refuses symlink escape / relative traversal / device
      crossing).
- [x] C8-4 Filesystem-backed remote changes feed using
      `core/platform/fs_watch` on the remote root, with the same
      callback discipline and a monotonic cursor persisted in the durable
      state DB.
      *Implementation note:* shipped as an in-provider bounded ring feed
      (`ChangesFeed`, cursor = monotonic sequence number, overflow →
      `CursorExpired` → reconcile + re-baseline) fed by the provider's
      own writes plus a manual test handle; a live fs-watch bridge on
      the remote root can layer on later without changing the cursor
      contract. The cursor is persisted in the durable state DB.
- [x] C8-5 Replace the Phase 2.5 timed staged-executor simulator with real
      planner/hash/upload/download workers driven by the provider; add
      `WorkClass::Download`; preserve slice-budget interruptibility.
- [x] C8-6 Remote-to-local apply pipeline (`IntentSource::Remote`);
      cursor advance persisted only on durable intent completion.
- [x] C8-7 `self_write_cache` runtime implementation consuming the existing
      constants in `core/shared::constants::self_write_cache`. Op-id primary
      + content-hash fallback; xattr/ADS primary + side-file fallback; LRU-
      on-insert; TTL expiry on 1s tick; memory-pressure floors; provider
      hides side-files from enumeration.
- [x] C8-8 Ensure-remote-root semantics in the filesystem provider (mirrors
      local-root auto-create); invalid remote root surfaces actionable
      configuration error.
- [x] C8-9 Wire provider selection in `core/daemon/src/main.rs` to read the
      resolved `provider` kind from config; default `filesystem` pre-GA;
      keep `GoogleDriveProvider` compiled but inert.
- [x] C8-10 Integration tests for local→remote, remote→local, self-write
      loop prevention, restart recovery with in-flight work, throttle
      transitions during real work, and scope safety under escape inputs.
- [x] C8-11 Microbench/regression coverage for provider-backed execution:
      10k-file fixture within engine budgets; no admission serialization
      under burst; remote-apply cost scales linearly.
      *Scope note:* Tier-1 guard-rails landed (150-event storm stays
      bounded; upload bursts drain without admission serialization). The
      10k-file fixture and linear-scaling measurements belong to the
      Tier-2 perf suite (release pipeline) — tracked under "Deferred
      tasks" below.
- [x] C8-12 Happy-path bidirectional race smoke test: concurrent
      local+remote writes converge within 30s across 5 runs with either
      canonical-path landing or `~conflict-pending-{intent_id}` staging.
- [x] C8-13 Phase 2.5 simulator removal validation (gate for Phase C8.2):
      no `stage_duration` placeholders; every `WorkClass` does real work;
      no simulator-shaped functions in production paths.

### Bidirectional safety, conflicts, deletion semantics

- [x] C8-14 Bidirectional conflict policy (`keep both` suffix template
      `{stem}~conflict-{device_id}-{timestamp_ms}{ext}` with `-{seq}`
      collision-avoidance; "data preservation wins over deletion"). Replace
      the Phase C8-12 `~conflict-pending-{intent_id}` scaffolding.
- [x] C8-15 `deviceId` field in `VaporConfiguration` (Swift) and durable
      state (Rust): derive from `gethostname()` normalized to `[a-z0-9-]`
      (length-capped at 32, UUIDv4-truncated-to-12 fallback); persist
      immediately; never silently regenerate.
- [x] C8-16 Tombstone/delete reconciliation with restart-safe replay.
- [x] C8-17 Deterministic race resolution for simultaneous edits,
      rename+modify, delete/restore.
- [x] C8-18 Bidirectional race integration tests + corruption-recovery
      validation.

### Profile model, multi-provider accounts, settings overrides

- [x] C8-19 Durable profile model (`profile_id`, display name, provider
      kind, account identity, enabled state); classify settings into
      app-global vs profile-override-capable.
- [x] C8-20 Namespace `SecretStore` entries, auth refresh state, and
      provider connection metadata by profile/account.
- [x] C8-21 Profile-scoped override resolution for sync-affecting settings;
      keep global-only settings singular. The mechanism built here is reused
      by auto-tuning (C8-32) to layer `resourceLimits` and `idleBoost`.
- [x] C8-22 Multiple enabled profiles concurrently (same local root fan-out,
      different local roots, per-profile debounce/scheduler/durable-queue).
- [x] C8-23 Deduplicate shared local-root watches: one `FsWatcher` per
      canonical realpath with per-callback fan-out into per-profile ingest
      queues tagged with `profile_id`.
- [x] C8-24 Blast-radius containment: profile runtimes spawned under panic
      catchers; a failed profile suspends only its queue.
- [x] C8-25 Safe profile disconnect/delete flows.
- [x] C8-26 Integration/perf tests for override resolution, multi-provider
      fan-out, parallel sync, restart recovery, multi-profile budgets.

### IPC / diagnostics UX

- [x] C8-27 Finalize IPC schema (continues C5) with every control endpoint
      (`Pause/Resume`, `FlushNow`, `AutoLaunchToggle`, excludes updates).
- [x] C8-28 Full menubar state model and reasoned status messages (consumed
      by every app surface; provided via IPC).
      *Scope note:* the daemon/IPC side (reasoned run/throttle state,
      per-profile summaries, counters) is complete; rendering it in the
      macOS menubar is Wave 9 app-surface work (`docs/tasks/macos.md`).
- [x] C8-29 Per-intent "why stuck" diagnostics exposing `intent_id`,
      `profile_id`, `path`, `action`, `stage`, elapsed-in-stage,
      attempt count, last-error class, `blocker_reason`. Also surface
      `BoundedFsEventRecorder::dropped_incoming_event_count`.
- [x] C8-30 Daemon activity event stream for timeline (bounded in-memory,
      configurable max length, default `1000`).
- [x] C8-31 Tests for timeline ordering/truncation/skew/field-omission.

### Auto-tuning and user resource-budget enforcement

- [x] C8-32 Add `resourceLimits` config group (`cpuPercent` default `15`,
      `memoryPercent` default `10`, `bandwidthPercent` default `25`; all
      ranges `1..100`). Clamp invalid values with classified warning.
- [x] C8-33 Add `idleBoost` config group (defaults per plan); require
      `boost*Percent >= resourceLimits.*Percent` at config-load.
- [x] C8-34 Extend C8-21 override resolution to cover `resourceLimits` and
      `idleBoost` via MIN-lowering semantics; any enabled profile with
      `idleBoost.enabled = false` disables boost daemon-wide.
- [x] C8-35 Document both groups in root `README.md` **Configuration**
      table and in `docs/architecture/data-flow.md` under "User resource
      budgets".
- [x] C8-36 `ResourceBudget` runtime component: resolve effective ceilings
      each tick; consume `PlatformMetricsSampler` + `IdleNotifier`; run
      idle-boost state machine; publish caps + reason codes.
- [x] C8-37 Extend workgate to consume `ResourceBudget` effective ceilings
      (CPU-ceiling-derived cap interacts with throttle caps via MIN;
      in-flight work yields at next slice checkpoint when cap drops).
- [x] C8-38 Provider-neutral bandwidth shaper in `core/providers` (bytes/s
      token bucket). Rate driven by effective `bandwidthPercent` ceiling
      against measured link capacity.
- [x] C8-39 Memory-ceiling enforcement: bounded caches + intent maps react
      to RSS; `self_write_cache` TTL shortens, timeline buffers trim, storm
      compaction thresholds lower; knobs bounded with hysteresis.
- [x] C8-40 Expose effective ceilings + current utilization + idle-boost
      state + human reason through IPC diagnostics; render in every app's
      diagnostics surface.
- [x] C8-41 Integration tests per `docs/architecture/data-flow.md` §"Ceiling
      transitions" and `docs/performance/acceptance-budgets-and-benchmark-
      harness.md` §"SLO applicability under user resource ceilings".
- [x] C8-42 Auto-tuning loop (60-120s cadence; one small change per cycle;
      tune priority: impact → rate-limit avoidance → latency; hysteresis +
      rollback-on-regression; bounded by effective ceilings).

### Provider-system extensibility hardening

- [x] C8-43 Finalize provider capability model and trait boundaries.
- [x] C8-44 Provider contract tests with reference/mock provider across
      semantics (iCloud/S3/R2/Proton Drive-style constraints).
- [x] C8-45 Compatibility validation for bidirectional flows, conflicts,
      tombstones, retries, throttle behavior through abstractions.
- [x] C8-46 Performance checks for provider-adapter overhead.
      *Scope note:* covered at Tier 1 by the burst/storm guard-rails over
      the real provider; dedicated adapter-overhead microbenches belong
      to the Tier-2 perf suite — tracked under "Deferred tasks" below.
- [x] C8-47 Provider-onboarding checklist + acceptance criteria docs.

### Google Drive provider (first external cloud target)

- [x] C8-48 `provider_gdrive` OAuth (PKCE), token storage via
      `core/platform/secrets`, refresh handling per
      `docs/operations/provider-auth-operations.md`.
- [x] C8-49 Authenticated Google Drive folder lookup/create for
      `cloudSyncDirectory` before sync starts.
- [x] C8-50 Block normal sync until the cloud root exists or the provider
      returns an actionable initialization error.
- [x] C8-51 Upload paths (multipart small, resumable large) behind the
      provider trait; chunked retry; rate-limit-aware.
- [x] C8-52 Remote changes polling via the Google Drive changes endpoint;
      adaptive cadence + request budgeting tied to throttle state.
- [x] C8-53 Provider metadata caching + resumable upload chunk auto-sizing.
- [x] C8-54 Flip `GoogleDriveProvider` from inert to selectable via
      `provider` config, gated on C8-44 contract tests.

### Optional advanced safeguards

- [x] C8-55 Active-coding detection (permissioned) with heuristic fallback.
      *Implementation note:* the heuristic fallback shipped (code-class
      churn ⇒ user-active throttle input). The permissioned native HID
      signal remains the `MetricsSampler`/`IdleNotifier` platform bridge
      tracked in Phase C3 follow-ups; the heuristic composes with it
      additively when it lands.
- [x] C8-56 Folder priority classes + temporary flush boost.
- [x] C8-57 Mass-change / ransomware guard with pause + alert workflow.
- [x] C8-58 Diagnostics history + support export bundle.

### Sync modes (directional / one-way sync) — prioritized

Plan reference: `docs/plans/core.md §2.5`. Full design:
`docs/architecture/sync-modes.md`. Pipeline mechanics: `docs/architecture/data-flow.md §Sync modes (directionality)`.

**Priority + ordering.** Although numbered after the C8-1…C8-58 span (to keep
existing task IDs stable), this sub-wave is prioritized: it lands as soon as
the bidirectional runtime shell exists (C8-1…C8-13), *before* the profiles UX
and *before* any app-surface work (Wave 9). Build order within the sub-wave is
deliberately `pull-only` → `two-way` → `push-only` so the remote→local
download/apply path is validated first.

- [x] C8-59 `syncMode` config surface + `SyncMode` enum (`two-way` default,
      `pull-only`, `push-only`). Add `config::KEY_SYNC_MODE` + `ALL_KEYS` and
      the enum + default in `core/shared/src/constants.rs`; mirror in the Swift
      `VaporConstants` per AGENTS.md §8.6. Thread `sync_mode` onto `SyncScope`
      (`core/daemon/src/sync_directories.rs`) and classify it as a
      **profile-override-capable, categorical** setting (top-level value is the
      default; each profile overrides outright — not MIN-lowering; see C8-19 /
      C8-21). `vapor config get|set syncMode <value>` enum-validates (see
      `docs/tasks/cli.md` L1-5). Document in root `README.md` **Configuration**,
      `docs/architecture/sync-modes.md`, and `data-flow.md`.
- [x] C8-60 **`pull-only`** (cloud → local, strict mirror) — *first*. Requires
      C8-1…C8-13 (download stage + remote-apply pipeline). Gate off all
      local→remote propagation (no upload / remote-delete / remote-rename). In
      the remote-apply + reconcile paths make local exactly match cloud: apply
      remote creates/edits/deletes, revert divergent local edits to the cloud
      canonical, and remove local-only files. Cloud is authoritative — no
      keep-both conflict copies. `self_write_cache` still suppresses echoes.
      Validates cloud→local download/mirror end-to-end.
- [x] C8-61 **`two-way`** — *second*. Bind `syncMode = two-way` to the existing
      bidirectional keep-both pipeline (C8-14…C8-18) and assert the one-way
      gates are inert in this mode. Primarily a wiring + validation task layered
      on the conflict/tombstone work.
- [x] C8-62 **`push-only`** (local → cloud, strict mirror) — *third*. Gate off
      all remote→local propagation (no download / local-delete / local-revert).
      In the local-apply + reconcile paths make cloud exactly match local:
      upload local creates/edits, propagate local deletes to the cloud,
      overwrite divergent remote files with the local canonical, and remove
      cloud-only files. Local is authoritative — no keep-both.
- [x] C8-63 Informed opt-in + overwrite warning (owner decision: **no**
      recoverable quarantine). One-way modes never activate for a profile
      unless `syncMode` is explicitly set to `pull-only` / `push-only` — never
      inferred — and the subordinate side is overwritten/deleted **permanently**
      to match the source. The runtime records the explicit opt-in and exposes
      it (with the per-profile mode + revert/delete counts, C8-65) so every
      surface can show a clear data-loss warning before the mode is enabled.
      Vapor's never-lose-data guarantee is scoped to `two-way`; one-way modes
      trade it for a faithful mirror with an up-front warning. See
      `docs/architecture/sync-modes.md §Safety`.
- [x] C8-64 Multi-profile mixed modes. Verify different profiles on one device
      run different `syncMode`s concurrently (e.g. several `pull-only` mirror
      profiles + one `two-way`), each isolated per the multi-profile watch
      coordination rules. Requires C8-19…C8-26.
- [x] C8-65 Diagnostics / IPC. Expose per-profile `syncMode` and a count of
      mirror-driven reverts/deletes in the status + diagnostics surface
      (extends C8-27…C8-31); consumed by macOS UX (`docs/tasks/macos.md` M4-2 /
      M4-5) and `vapor status`.
- [x] C8-66 Tests. `pull-only` reverts a local edit + removes a local-only file
      + deletes on cloud-delete, never uploads; `push-only` overwrites a remote
      edit + removes a cloud-only file + deletes on local-delete, never
      downloads; `two-way` keep-both unaffected and gates inert; mode change
      mid-run converges; mixed per-profile modes stay isolated;
      `self_write_cache` still prevents echoes per mode; a one-way mode never
      activates without an explicit `syncMode` opt-in. Cross-links CT-7.

Exit gate:

- All three `syncMode` values behave per `docs/architecture/sync-modes.md` on
  the filesystem reference provider, validated in the order pull-only →
  two-way → push-only.
- One-way modes are opt-in per profile, surface the destructive-action
  warning, and are observable (per-profile mode + revert/delete counts in
  diagnostics).
- No app-surface work depends on this being incomplete — the runtime behavior
  is proven before Wave 9 exposes the toggle.

### Post-Wave-8 follow-ups — conflict surfacing + real-watcher fixes

Born from project-owner manual verification of Wave 8 (PR #5): the manual
pass surfaced runtime gaps the synthetic-event suites could not see, plus
the need to make keep-both conflicts a first-class, resolvable surface.
Design: `docs/architecture/conflict-resolution.md`.

- [x] C8-67 Symmetric ignore filtering: the reconcile walk and the
      remote-change mapping consult the scope's shared path filter (one
      instance per canonical root, shared with the watcher and across
      profiles sharing a root), so an ignored name (`.DS_Store`,
      `node_modules/`) never syncs in either direction and can never
      manufacture a conflict copy. E2E S14.
- [x] C8-68 Ground-truth deletion classification: a stabilized burst
      carrying a removal/rename for a path that is gone at stabilization
      maps to `Delete` regardless of fs-watch fragment order (FSEvents
      flag coalescing made real deletions plan as uploads that no-op'd
      "vanished before upload", leaving remote copies immortal); the
      mass-deletion guard taps the same classification decision. E2E S16.
- [x] C8-69 Conflict-copy name parser
      (`conflict::parse_conflict_copy_name`, strict inverse of the
      generator, rejects marker-lookalike user files) + `CONFLICT_MARKER`.
- [x] C8-70 `vapor conflicts list [--json]` + `vapor conflicts resolve
      <copy> --keep <canonical|copy>` (cli.md L3-8): files-as-registry
      scan pruned by the ignore rules, locked JSON contract for app
      surfaces, resolution via plain file operations that sync like user
      edits and work with the daemon stopped. E2E S15.
- [x] C8-72 Hash-verified divergence for uploads onto untagged remotes:
      an op-id mismatch alone no longer means conflict — the planner
      compares the remote content hash against the sync index first, so
      the everyday "external cloud edit → download → local edit →
      upload" round-trip overwrites safely instead of manufacturing a
      keep-both copy (or reverting a just-resolved conflict).
- [x] C8-73 Local write-echo correlation hardened: the op-id tag
      survives later writes, so tag-match alone suppressed genuine user
      edits made within the echo TTL after a download-apply. Echoes now
      require the file's current content to match the daemon's write
      (size-gated hash), with the tag never sufficient by itself.
- [ ] C8-71 Per-path detail on timeline `conflict` events (today the event
      carries only the per-tick count), so app notifications can name the
      conflicted file without an immediate list scan. Needed by macos.md
      M3-8.

### File-only engine — decided

The engine syncs **regular files only**, by explicit project-owner
decision (2026-07-07): directories stay implicit containers that
materialize through their children, and symlinks / special files stay
outside the sync contract. The full first-class-folder workstream
(provider `create_directory`, directory intents, rename-as-rename) was
evaluated and rejected — the file-only model is what keeps the conflict,
echo-suppression, and transfer machinery simple and robust, and the
rename fallback (delete + re-upload through children) is data-safe.
User-facing contract: root `README.md` **What Syncs** table. Engine
semantics: `data-flow.md §Directory and symlink semantics`.

## Phase C9 - `vapor` CLI delivery

Tracked separately in `docs/tasks/cli.md`; this phase is informational here
and the acceptance gate for cross-platform parity.

## Phase CT - Testing discipline and coverage (cross-cutting)

Standing tasks that apply across every wave. The policy lives in
`AGENTS.md §9`; the full taxonomy and discipline rules in
`docs/architecture/testing-strategy.md`.

Testing is a non-negotiable part of every change. The suite is the
coding agent's feedback loop — it must stay fast (under 5 minutes per
OS on CI, under 2 minutes locally), deterministic (no sleeps, no
network, no real `VAPOR_DIR`), and honest (cover real behavior, not
trivial restatements of code).

### One-time setup (do early)

- [ ] CT-1 Adopt `proptest` as a dev-dependency in `core/daemon` and
      `core/shared`. Add initial property tests for the high-value
      invariants listed in `docs/architecture/testing-strategy.md`:
      path normalization safety, scheduler superseding collapse,
      throttle monotonicity, retry backoff monotonicity, ignore-rule
      precedence determinism, durable-queue FIFO. Each property runs
      64–256 cases on CI (fast tier).
- [ ] CT-2 Add a Tier-1 timing guard to CI that fails if
      `./scripts/test.sh` exceeds 5 minutes on a matrix runner. Emit a
      clear message pointing at `docs/architecture/testing-strategy.md
      §Discipline rules`.
- [ ] CT-3 Audit the current `core/*` test corpus for trivial-test
      smell per `AGENTS.md §9.3` (defaults that mirror constants,
      Debug/Display string equality, serde round-trips of trivial
      structs). Remove or replace with behavior-level assertions.
      Document any kept legacy trivial test with a one-line rationale.
- [ ] CT-4 Adopt `insta` as a dev-dependency in `core/cli` when it
      lands (wave 6). Snapshot every `--json` command's output with a
      fixed input fixture. Document the `cargo insta review` flow in
      `docs/development/runbook.md`.

### Per-wave standing requirements

These do not have dedicated tickets — they ship with the wave that
introduces the code they apply to.

- [ ] CT-5 Every new `core/platform` trait ships with (a) an
      in-memory fake, (b) a parameterized contract-test suite, and (c)
      native implementations wired into that suite on every shipping
      OS. Catches fake-vs-native drift. Applies to wave 4 and any new
      trait added after.
- [ ] CT-6 IPC skew matrix tests: when wave 6 lands, the test
      matrix covers `app-N ↔ daemon-N`, `app-N ↔ daemon-(N-1)`,
      `app-(N-1) ↔ daemon-N`, and `|N - M| = 2` (negative case).
      Field-omission tolerance and payload-bound rejection each have a
      test.
- [ ] CT-7 Every bidirectional race scenario listed in `AGENTS.md
      §9.2` ships with integration coverage during the C8 waves
      (simultaneous edits, rename+modify, delete/restore, loop-
      prevention verification). `docs/tasks/core.md` C8-10 and C8-18
      already expect this; CT-7 is the reminder to not skip it.
- [ ] CT-8 Every CLI command with `--json` output ships a snapshot
      test in the same PR that adds the command. Applies to every
      `cli.md` task from L1 onward.

### Tier-2 release gate (deferred setup)

Tier 2 runs only as part of the release pipeline (`perf.yml` invoked by
`release.yml`; no standalone or scheduled triggers).

- [ ] CT-9 Add a `cargo-fuzz` harness for parsers: ignore-rule parser,
      IPC frame parser (once wave 6 lands), JSON config loader, path
      normalization. Short corpus committed in-tree; long runs ride the
      Tier-2 release gate. Not a PR gate.
- [ ] CT-10 Add `loom`-backed tests for `ThrottleWorkgate` permit
      allocation under contention and `BoundedFsEventRecorder`
      drop-count semantics. Run under the Tier-2 release gate; not a PR
      gate. Keep the test set small — `loom` is slow.
- [ ] CT-11 Rerun Tier-1 property tests with a higher case count
      (1 000–4 000 per property) under the Tier-2 release gate to catch
      rare counterexamples the PR budget does not reach.

Exit gate (ongoing): Tier 1 stays under 5 minutes per OS on CI; no
flaky tests carried across two consecutive weeks; every trait in
`core/platform` has contract-test coverage against both fake and
native on every shipping OS; every `--json` CLI command has a
snapshot.

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
- [ ] PT-2 Tier-2 perf fixtures carved out of C8-11 / C8-46: 10k-file
      provider-backed fixture within engine budgets, remote-apply
      linear-scaling measurement, and provider-adapter overhead
      microbenches. Belongs to the `scripts/perf.sh` release-gate suite;
      Tier-1 guard-rails (storm bounds, burst admission) already cover
      the regression-catching role on every PR.
- [ ] O-1 Design and implement the production onboarding flow
      (information architecture, step sequence, copy, UX states). When this
      starts, run a clarification pass with the project owner to define the
      onboarding structure before implementation.
