# Vapor core task list

Plan reference: `docs/plans/core.md`
Related task lists: `docs/tasks/macos.md`, `docs/tasks/cli.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

This file holds open work only. A task that is done leaves the file:
`CHANGELOG.md` and the git history record what landed, and
`docs/specs/` records the decisions behind it. Task ids are stable and
are never reused.

## Sync engine

- [ ] E-1 Build the first set of ignore rules on a worker. A rule
      change already rebuilds on a worker, but the first build, which
      walks the tree for ignore files, runs while a profile is
      composed. Move it to a worker and hold the release of intents
      until the rules are in.
- [ ] E-2 Close the cloud-side window of a swapped root. The local root
      is read again before any lease and before each cloud deletion.
      The cloud root is checked each 15 seconds only, so a cloud root
      that is swapped can make local files look deleted for that long.
      Those files go to the trash, so nothing is lost, but the check
      belongs before each local removal too.
- [ ] E-3 Decide what two profiles with overlapping local roots mean,
      and refuse the pair if the answer is "nothing safe". One profile
      whose cloud folder sits inside its local folder is refused today;
      two profiles whose local folders overlap are not. This waits for
      the owner (`docs/specs/2026-11-14-full-review.md`, open points).
- [ ] E-4 Remove a local folder that a cloud-side rename or removal
      emptied. On a volume that keeps spellings apart, or for any
      rename of a folder in the cloud that the reconcile walk finds,
      the files move and the old local folder stays behind, empty.
- [ ] E-5 `Pause`, `Resume`, and `Reconcile` for one profile: a profile
      argument on the three IPC controls and `--profile` on the three
      commands. Today each one acts on every profile.
- [ ] C8-71 Per-path detail on timeline `conflict` events (today the
      event carries only the per-tick count), so app notifications can
      name the conflicted file without an immediate list scan. Needed
      by `macos.md` M3-8.

## Providers

- [ ] P-1 Give the second of two Google Drive files with one name in
      one folder a local name of its own (an alias, as a case collision
      gets). Today one of the two syncs, the choice is stable, and a
      warning names the other.
- [ ] P-2 Keep the Drive file id in the sync index. The Drive feed
      reports a deletion by id, and the provider's id cache is empty
      after a restart, so a deletion made in the cloud while the daemon
      was down is found by the startup reconcile and not by the feed.
- [ ] P-3 Make the HTTP transport read the proxy settings and the
      system's root certificates. Today it uses neither, so a network
      that inspects TLS cannot sync.
- [ ] P-4 Check against a real Drive account what was built from the
      API documents: a resumable upload answers with the checksum, and
      a folder rename that changes only the case keeps its children
      reachable. Runs with the Drive leg of the release gate (TR-8).
- [ ] P-5 Decide the Google verification path. The `drive` scope is
      restricted: Google requires app verification and a yearly
      security assessment before more than 100 users can sign in, and a
      client in Testing status signs every user out each week
      (`docs/operations/provider-auth-operations.md`). The choice is
      verification, or a narrower scope with what it costs. It gates
      general availability of the Drive provider and waits for the
      owner.

## Platform layer

- [ ] C3-11 Remaining throttle inputs. macOS: a disk-pressure source
      (none is public; evaluate free space on the sync-root volume as a
      proxy). Linux: the speed of the network interface, the
      display-server idle query, and PSI. Windows: the full sampler and
      idle notifier listed in
      `docs/architecture/platform-abstractions.md`, when that surface
      ships.
- [ ] PL-1 Move a file to the macOS system trash through
      `NSFileManager.trashItem`, so Finder offers Put Back and a file
      on another volume goes to that volume's trash. The rename into
      `~/.Trash` that ships today gives neither. Needs a manual check
      in Finder.
- [ ] PL-2 Read the state of a launchd job without matching the text
      of `launchctl print`, which Apple does not keep stable. The
      installer matches that text today to tell a loaded job from a
      missing one.
- [ ] CT-5 Every `core/platform` trait ships with (a) an in-memory
      fake, (b) a parameterized contract-test suite, and (c) native
      implementations wired into that suite on every shipping OS.
      `SecretStore` and `FsWatcher` have theirs; `ServiceInstaller`,
      `PlatformMetricsSampler`, `IdleNotifier`, `FilesystemCapabilities`,
      `ProcessSupervisor`, and `TrashBin` are open.

### Windows (deferred, optional)

Gated on the project owner opting into a Windows surface. Nothing in
the primary path is blocked by it. See `docs/tasks/README.md`.

- [ ] C6-1 `core/platform/fs_watch/windows.rs`: `ReadDirectoryChangesW` with
      IOCP. Prefer `notify` as MVP; switch to direct `windows` crate when
      buffer sizing or rename-pair semantics need tuning. Rename pair
      handling (`FILE_ACTION_RENAMED_OLD_NAME` + `_NEW_NAME`) verified.
- [ ] C6-2 `core/platform/service/windows.rs`: Task Scheduler impl
      (per-user, `AtLogOn` trigger, `RestartOnFailure`) via `ITaskService`
      through the `windows` crate.
- [ ] C6-3 `core/platform/secrets/windows.rs`: Credential Manager via
      `keyring` or `windows` crate (`CredWriteW` / `CredReadW`).
- [ ] C6-4 `core/platform/metrics/windows.rs`: `GetSystemTimes` +
      `GetProcessTimes`, `GetSystemPowerStatus`,
      `CallNtPowerInformation(SystemPowerInformation)`, the interface
      speed, and metered connection awareness via
      `NotifyNetworkConnectivityHintChange`.
- [ ] C6-5 `core/platform/idle/windows.rs`: `GetLastInputInfo` polled on
      the 1s throttle cadence.
- [ ] C6-6 `core/platform/fs_caps/windows.rs`: NTFS Alternate Data Streams
      via `CreateFileW` with `filename:streamname`; side-file fallback on
      ReFS/FAT; case sensitivity read for the directory.
- [ ] C6-7 `core/platform/process/windows.rs`: `SetConsoleCtrlHandler` for
      interactive; `SERVICE_STOP` via `windows-service` for service runs;
      `WM_ENDSESSION` for logout.
- [ ] C6-8 Windows distribution trust chain doc:
      `docs/operations/windows/distribution-trust-chain.md` + task-scheduler
      policy in `docs/operations/windows/scheduled-task-policy.md`.
- [ ] C6-9 The named-pipe IPC transport, with the user and the hash of
      the runtime directory in the pipe name
      (`docs/architecture/windows/ipc-transport.md`).

### Linux (native traits in; not a shipping surface yet)

Every `core/platform` trait has a Linux implementation that the daemon
and the CLI consume, verified by Tier 1 and the e2e harness on Linux
runners. What remains before Linux ships a surface:

- [ ] C7-7 Linux distribution trust chain doc:
      `docs/operations/linux/distribution-trust-chain.md` + systemd unit
      policy in `docs/operations/linux/systemd-unit-policy.md`.
- The `vapor service` round-trip against a real `systemctl --user`
  session (`cli.md` L2-6) and the rest of the Linux soak (TR-10).

## Tests and tools

- [ ] CT-3 Audit the current `core/*` test corpus for trivial-test
      smell per `AGENTS.md §9.3` (defaults that mirror constants,
      Debug/Display string equality, serde round-trips of trivial
      structs). Remove or replace with behavior-level assertions.
      Document any kept legacy trivial test with a one-line rationale.
- [ ] CT-4 Adopt `insta` for the `--json` shape locks in `core/cli`;
      today every command has an explicit field-by-field test in its
      module and `core/cli/tests/binary.rs` locks the shell contract
      of the built binary on every CI OS. Document the `cargo insta
      review` flow in `docs/development/runbook.md` when it lands.
- [ ] CT-9 Add a `cargo-fuzz` harness for parsers: ignore-rule parser,
      IPC frame parser, JSON config loader, path normalization. Short
      corpus committed in-tree; long runs ride the Tier-2 release
      gate. Not a PR gate.
- [ ] CT-10 Add `loom`-backed tests for `ThrottleWorkgate` permit
      allocation under contention and `BoundedFsEventRecorder`
      drop-count semantics. Run under the Tier-2 release gate; not a PR
      gate. Keep the test set small: `loom` is slow.
- [ ] CT-11 Rerun Tier-1 property tests with a higher case count
      (1 000 to 4 000 per property) under the Tier-2 release gate to
      catch rare counterexamples the PR budget does not reach. Add the
      properties that explicit cases cover today: throttle
      monotonicity, conflict-suffix determinism, durable-queue FIFO,
      ignore-rule precedence, the IPC handshake skew matrix.
- [ ] T-16 Resource-ceiling integration tests over the cross-product in
      `docs/performance/acceptance-budgets-and-benchmark-harness.md`
      (`{default, cpuPercent=5, memoryPercent=5}` x `{idle, active,
      storm}` x `{boost on, boost off}` x `{global, profile-lowered}`).
- [ ] TR-8 Google Drive mode of the harness: a `CloudSide` the harness
      performs through the provider crate, a per-run `VaporE2E-<run-id>`
      folder created and deleted by the harness, the test account's
      token put into each sandbox (a sandbox has its own secret scope),
      longer wait budgets, `gdrive-provider` scenarios for token
      refresh and rate limits, and the Drive leg of the soak. Runs only
      as a leg of `./scripts/release.sh` on the maintainer's machine,
      never in CI. Waits on the dedicated test account.
- [ ] TR-10 The rest of the Linux soak: a `tmpfs`-backed cloud volume
      for the disk-full fault (the only cell still macOS-only in
      `soak.yml`), cgroup CPU and memory limits, Linux budgets for
      `perf.yml`, and the Google Drive soak mode once TR-8 lands.
- [ ] TR-11 Scenarios through the real binaries that nothing runs yet:
      a backward wall-clock step while the daemon runs, the CLI against
      a daemon of another IPC version through two real binaries (the
      skew matrix is in-process), and an upgrade (a state DB and a
      config written by an older binary, opened by the new one, which
      sets the database aside and rebuilds it). Each is one scenario.
- [ ] TR-12 The Google Drive leg of the release gate has not run yet:
      the first release after the dedicated test account exists runs
      `./scripts/release.sh` with the Drive leg for real, and TR-8's
      Drive-side scenarios land before or with it. Until then the leg
      is empty (every scenario declares the filesystem provider) and
      the gate refuses without credentials.
- [ ] PT-1 Tune the soak cell `scripts/perf.sh` gates on (duration,
      seed, load) using release baseline history once per-OS baselines
      exist; gate SLO-1 on the `quiet` phases. Give the thresholds that
      the budgets document marks "not measured" a measurement.
- [ ] PT-2 Tier-2 perf fixtures: a 10k-file provider-backed fixture
      within engine budgets, a remote-apply linear-scaling measurement,
      and provider-adapter overhead microbenches. Belongs to the
      `scripts/perf.sh` release-gate suite; Tier-1 guard-rails (storm
      bounds, burst admission) already cover the regression-catching
      role on every PR.

## Operations

- [ ] R-1 Write the incident runbooks `AGENTS.md §12` mandates under
      `docs/operations/`: daemon crash loops, auth/token refresh
      failures, provider rate-limit storms, a state database that was
      set aside, reconcile backlog non-convergence. Only the release
      incident playbook exists today. Each runbook needs detection,
      mitigation, user-visible state, and recovery verification.

## Documents

- [ ] D-1 One owner for each fact that several documents repeat: the
      crash-loop schedule (four documents and one skill), the bundle
      layout (six documents), and the test tiers (five places). The
      other places link to the owner. The toolchain baseline and the
      script list have one owner each already.
