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

- [ ] E-7 Ask when a case-sensitive local root holds `a.txt` and
      `A.txt` and the cloud folder folds case. Today the second upload
      waits for the name to go and is tried again without end; it needs a
      decision, like the name collision in the other direction. It takes
      a local volume that keeps the two spellings apart and a cloud
      folder that folds them: the filesystem provider on such a volume.
      A local root on the default macOS volume cannot hold the two
      names, and Google Drive keeps them apart.
      The owner put this off (2026-11-22), because no shipping setup
      reaches it; when it is built, the question is the one a name that
      cannot be synced gets today.
- [ ] E-9 Follow a sync root whose own spelling changes while the
      daemon runs (`vapor` renamed to `Vapor` on a volume that folds
      case). Names below the root compare right; not checked is whether
      the watcher still delivers events under the path the daemon holds.
      The filesystem provider refuses every path under a cloud root that
      was respelled, until a restart.
- [ ] E-12 Refuse two Google Drive profiles that share a local folder
      when their cloud folders are one folder, or one inside the other
      (`/Vapor` and `/Vapor/sub` of one account). Each file is copied
      one level deeper without end. The `filesystem` provider refuses
      such a pair by its paths. For Drive a path does not say whether
      two folders are one, because two accounts may both use `/Vapor`:
      the check needs the identity of each cloud folder and the chain
      of its parents, which the provider trait does not give today.
- [ ] E-14 Decide what to do with a local folder that a rename in the
      cloud emptied of synced files and that still holds ignored
      files. An empty folder is removed. A folder with a `.DS_Store`
      in it stays for good, and so does one with a `node_modules`
      folder, which must stay. The rule has to tell the two apart.
### Found by the heavy-user session in a Mac guest (2026-11-22)

The release installer of `0.2.0-alpha.3` (`d18a288`), installed in a
Tier VM guest and used through `vapor` between two folders (the
filesystem provider, two-way): about 200 edge cases in both
directions, conflict and race cases, lifecycle faults, a coding
workload with a real git repository, real `npm ci` and
`rm -rf node_modules` cycles, and a workload that moves folders. What
passed is in the description of the pull request that added these
tasks. Each task below was seen in the installed product, and E-37 in
a CI run of that pull request. A second run on 2026-11-23 used the
build of `80c842a`, after the engine change of that day; where a task
says nothing else, the second run showed the same. E-24, E-29, and
E-37 were not run again. `docs/tasks/README.md` has the order by what
a task costs a user.

- [ ] E-20 The mass-delete guard asks during ordinary developer work,
      and then holds every later deletion. Git's own maintenance
      removed about 1 000 loose objects: "895 of the 3231 files it
      syncs (27%) would be removed on the cloud", 1 038 deletions held,
      the deletes and renames of source files after it among them. A
      second question came 5 minutes later, because E-19 delivers the
      deletions of minutes as one burst. `rm -rf node_modules` asks
      each time. A small tree asks at once: on Google Drive, with 70
      files synced, the rename of a folder of 10 files opened the
      question ("19 of the 70 files it syncs (27%)"), because a folder
      rename counts as deletions. Eight renames and removals of a
      folder of 10 files, on a Drive tree of 37 to 50 files, asked 8
      times. The count of the question holds the folders and its total
      does not: `rm -rf` of 1 440 files in 360 folders asked about
      "1800 of the 1501 files it syncs (100%)". One removal can ask
      many times: 360 files removed one at a time in 32 s asked 8
      questions in 67 s ("95 of the 374", "78 of the 309", down to
      "23 of the 85"), because each `apply` leaves a smaller tree and
      the next deletions are a quarter of it again. In the second run the count of the
      question was right in one run and old in another, and the
      daemon needed 80 s to 148 s to hold the 10 493 deletions (E-31).
      And each device asks: one `rm -rf`
      of 400 files asked on the device that ran it and again on the
      second device, which kept 251 of the files until its own
      question was answered. The text of the question keeps the count of the
      moment it opened ("2916 of the 13070 files (22%)" for 10 493
      held). A question on every `git gc` teaches the user to answer
      `apply` without reading. Needs a design with the owner: what a
      burst is in a tree where tools delete thousands of files as
      routine (content-addressed stores such as `.git/objects`,
      dependency folders, build output), and a count in the question
      that follows the burst.
- [ ] E-31 Small files move at about 340 a second to a cloud folder on
      this device, and the drive is the limit. Measured on 2026-11-23
      with a release build in `Idle` on 12 cores: 6 000 files of 100
      bytes reach the cloud folder in 19 s, the first after 2 s. Before
      the flushes of the drive were shared and the scan of a burst
      started on time, the figures were 200 a second and 31 s. A file
      now costs its share of one flush of the drive (`F_FULLFSYNC`) and
      three commits of the state database, which wait behind the
      flushes of the other files. Three changes were measured and gave
      nothing: one transaction for the outcomes of one harvest, a short
      wait for the job that follows a job, and a least time between two
      ticks. Left to try: more small uploads at a time than the
      transfer cap of the throttle state, because the files in flight
      share a flush and the cap is 6 on that host; and one commit for a
      whole tick. The removal of a folder of 6 000 files is one
      deletion in the cloud and takes 3 s after the answer to the
      mass-delete question.
- [ ] E-43 Ask about a name in the cloud folder that cannot be synced,
      as a local one is asked about. A file with a backslash in its
      name in the cloud folder of the filesystem provider stays there
      with one warning in the log, and `vapor decisions` and
      `vapor ignored` do not show it. The provider trait carries a
      remote entry only as a valid path, so the walk never sees such a
      name: the trait needs a way to report it.

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
      The same run checks the copy of a version that an upload
      replaced unseen: two profiles of one daemon on one Drive folder
      change one file in the same second (the workload of the
      heavy-user session), and the folder must end with the file and a
      `~conflict-replaced` copy that holds the other edit. The code
      reads `revisions.list` and the content of one revision
      (`revisions.get` with `alt=media`); only the scripted transport
      has answered it so far. See also whether the `version` field of
      a file tells a second writer without the list, which costs one
      request for each overwrite.
- [ ] P-12 Make the Drive check for a replaced version survive a stop
      of the daemon. An upload that lands leaves the check in memory
      for the next changes poll. A daemon that stops in between makes
      no copy, and the replaced version is then only in the version
      history of the file in Google Drive. Both uploads must land in
      the same second for a version to be replaced at all, so the
      window is small; a durable record of the check (the file, the
      hash it replaced, the hash it wrote) closes it.
- [ ] P-5 Decide the Google verification path. The `drive` scope is
      restricted: Google requires app verification and a yearly
      security assessment before more than 100 users can sign in, and a
      client in Testing status signs every user out each week
      (`docs/operations/provider-auth-operations.md`). The choice is
      verification, or a narrower scope with what it costs. It gates
      general availability of the Drive provider and waits for the
      owner.
- [ ] P-7 Give a removal that the `filesystem` feed derives from an
      older event the time of the poll. An object that is gone when
      the feed reads its create or write event becomes a removal with
      the time of the newest event of the batch. When the removal
      event of the watcher is in a later batch, that time is older
      than the last sync, and the engine drops the removal until the
      next whole-scope scan. The feed needs the clock of the daemon
      for this, which the provider does not get today.
- [ ] P-9 Small files move through Google Drive at about 2 a second.
      400 files of 1 KB took 214 s from one device to the other (the
      upload, the changes poll, and the download together), and 31
      files with one of 20 MiB took 110 s. On `80c842a` the 400 files
      took 301 s (1.3 a second), their removal 286 s after the answer
      to the mass-delete question, and the 31 files 64 s. One save
      takes 9 s to 12 s from device to device (P-11). Measure the
      upload and
      the download apart, then raise what runs in parallel within the
      rate limits of the API. Found with the dedicated test account in
      a Mac guest, 2026-11-22.
## Platform layer

- [ ] C3-11 Remaining throttle inputs. macOS: a disk-pressure source
      (none is public; evaluate free space on the sync-root volume as a
      proxy). Linux: the display-server idle query and PSI (the
      interface speed is PL-3). Windows: the full sampler and idle
      notifier listed in
      `docs/architecture/platform-abstractions.md`, when that surface
      ships (C6-4, C6-5).
- [ ] PL-3 Read the interface speed on Linux, into
      `ThrottleInputs::link_speed_kbps`
      (`core/platform/src/metrics/linux.rs`). It is the starting figure
      of the bandwidth ceiling: macOS has it, so the ceiling holds from
      the first byte there. Linux reads none today, so the first
      transfer step of each direction runs with no limit, and the
      ceiling holds from the first measurement on.
      - Wired: `/sys/class/net/<interface>/speed`, in Mbit/s, for the
        interface that carries the default route (`/proc/net/route`).
        A value of `-1` or a read error means "not known", as today.
      - Wi-Fi: `/sys` has no figure. The rate of the current link
        comes from an nl80211 station query over netlink. Until that
        is built, Wi-Fi stays "not known".
      - Done when: the sampler's test reads a plausible figure or none
        on the Linux CI job, as the macOS test does, and
        `docs/architecture/platform-abstractions.md` no longer says
        "with no interface speed".
- [ ] PL-1 Check in Finder that a file which Vapor moved to the macOS
      trash offers Put Back. The move goes through
      `NSFileManager.trashItem` now, and a contract test proves the
      file is in the trash of its volume; only a person at Finder sees
      the menu item. Needs the owner, in a guest window.
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
      `NotifyNetworkConnectivityHintChange`. The interface speed is
      the transmit and receive link speed of the interface that
      carries the default route (`GetIfTable2`), into
      `ThrottleInputs::link_speed_kbps`: the starting figure of the
      bandwidth ceiling, as in PL-3. Without it the first transfer
      step of each direction runs with no limit.
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
- [ ] C6-10 `vapor report` redaction for Windows paths: drive letters
      and backslash separators in the configuration, the logs, and the
      live state. The redactor splits a path at `/` today, and its
      tests with path fixtures run on Unix only.
- [ ] C6-11 The Windows installers, `Vapor-windows-x64` and
      `Vapor-windows-arm64`, as a `package (windows)` entry of
      `release.yml`. One installer holds the app, `vapord.exe`, and
      `vapor.exe`; the CLI gets a directory of its own, because
      `Vapor.exe` cannot share one with it on a filesystem that ignores
      case, and the installer puts that directory on the `PATH`. Open
      with it: `.msi` or `.msix`, which follows the UI toolkit of the
      app, and how the supervisor hands over to a new build after an
      update (`docs/specs/2026-11-21-platform-release-assets.md`).

### Linux (native traits in; not a shipping surface yet)

Every `core/platform` trait has a Linux implementation that the daemon
and the CLI consume, verified by Tier 1 and the e2e harness on Linux
runners. What remains before Linux ships a surface:

- [ ] C7-7 Linux distribution trust chain doc:
      `docs/operations/linux/distribution-trust-chain.md` + systemd unit
      policy in `docs/operations/linux/systemd-unit-policy.md`.
- [ ] C7-10 The Linux packages: a `.deb` and an `.rpm` for x64 and for
      ARM64 (`Vapor-linux-<arch>.<ext>`), as a `package (linux)` entry
      of `release.yml`, each with its GPG signature inside. A package
      puts `vapor` in `/usr/bin` and installs on a host with no
      desktop: the desktop libraries are optional dependencies, so a
      server gets the CLI and the daemon and `vapor init` sets it up.
      The first packages may hold `vapor` and `vapord` only; the app
      window joins the same packages later, under the same names. The
      package check installs it on the runner and finds `vapor` on the
      `PATH`, as the macOS one does. Open with it: where `vapord` goes
      (`/usr/bin` or a private directory), and a distribution with
      neither format, which builds from source.
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
      never in CI. The dedicated test account exists since 2026-11-22.
      Its sign-in is in the maintainer's gitignored `.env` as
      `VAPOR_E2E_GDRIVE_TOKEN`: the JSON document that `vapor auth
      login --token -` reads. The harness has to read that variable,
      and `.env.example` has to name it.
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
- [ ] TR-13 Make the heavy-user session of 2026-11-22 a run that
      anyone can repeat. Its workloads are in `tools/vm/workloads/`,
      as they ran: scripts for a manual guest, started by hand, with a
      README. They are edge-case batteries (names, file types,
      operations, conflicts, lifecycle faults), a coding workload on a
      real git repository that checks each file before it touches it
      again, a probe that times single saves next to a busy project,
      real `npm ci` and `rm -rf` cycles from an offline cache, a
      workload that moves folders, and the Google Drive cases. Needs a
      short design: whether they stay scripts or join the harness, how
      a run reports, and which of them gate a release. Each fix of a
      finding of the session adds its case to Tier 1 or Tier E2E
      first; the soak model does not produce any of them today.
- [ ] PT-3 Measure in the perf gate what a user waits for, so a slower
      build fails the gate. `scripts/perf.sh` checks CPU and memory of
      a soak cell today and no latency. `tools/vm/workloads/latency.py`
      times one change at a time in both directions; the gate needs
      the same on the CI runner, with budgets set there and not from a
      guest. The cases, with what a Mac guest of 4 CPUs measured on
      `80c842a` between two local folders (this device to the cloud
      folder, then the other way): a new file 1.5 s and 3.6 s, an
      edit 1.4 s and 3.6 s, a 5 MB file 4.3 s and 3.3 s (E-41), a
      rename 1.5 s and 2.8 s, a deletion 5.3 s and 6.7 s (E-40); with
      a user at the device a new file 1.4 s and 58 s (P-11); one save
      next to a busy project 54 s to 248 s (E-19); the first file of a
      burst of 1 500 after 33 s and the last after 39 s (E-38); small
      files a second up, 160 (E-31); one large file 96 MiB/s up and
      124 down; the catch-up after a pause 33 s and after a stop 22 s
      (17 s and 16 s on `d18a288`, one run each); the restart after a
      kill 21 s (PL-6); and the memory and CPU of a daemon with an
      open question (E-39). The same cases for Google Drive join with
      its leg (TR-8); measured on `80c842a` from device to device: a
      new file 9 s to 12 s, an edit 9 s, a 5 MB file 23 s to 27 s, a
      rename 11 s, a deletion 16 s, and 31 files with one of 20 MiB
      64 s.
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

## Documents

- [ ] D-1 One owner for each fact that several documents repeat: the
      crash-loop schedule (four documents and one skill), the bundle
      layout (six documents), and the test tiers (five places). The
      other places link to the owner. The toolchain baseline and the
      script list have one owner each already.
- [ ] D-2 Bring the Rust toolchain of the contributor Mac in step with
      the one that CI installs, and say in
      `docs/development/toolchain-baseline.md` how to check it. CI
      takes the latest stable at each run. A local toolchain that is
      some months older passes `./scripts/lint.sh` on code that CI
      rejects for a new deprecation, which cost one CI cycle on
      2026-11-22.
