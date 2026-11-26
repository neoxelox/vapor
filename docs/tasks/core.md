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
- [ ] E-46 A wait of the planner counts as a try, and two waits have
      no upper limit. A download waits for the files of a folder that
      the cloud replaced with a file, and looks again each 3 s. The
      removal of a cloud folder waits for its entries, and looks again
      each 2 s to 30 s. Each look adds one to `attempt_count`
      (`defer_leased`), and a row above `MAX_ATTEMPT_COUNT` (10 000)
      cannot be read: the lease of the queue then fails with
      `InvalidAttemptCount`. At 3 s that is 8 hours of a folder whose
      files cannot leave, for example one that refuses a change. Read
      from the code and checked at the state database with 10 001 waits
      of one row (2026-11-24). What the runtime does after the failed
      lease was not checked. The rename waits stop after
      `MOVE_SETTLE_MAX_DEFERRALS`.
### Left open by the rename in one step (2026-11-25)

A file or a folder that gets another name or place in the cloud is one
rename on the device now, where the provider has an id for it
(`docs/specs/2026-11-25-cloud-folder-rename.md`). What that change
found and did not build:

- [ ] E-48 Pair a name that both sides changed when this device also
      moved the entry into another folder. The engine looks for the
      entry that this device renamed in the folder of the old path
      only. An entry that this device moved elsewhere, while the cloud
      gave it another name, is not paired: the device works file by
      file, and the folder can end under both names, each with every
      file, as before the change. A look by content at the upload gate
      (the hash of a file under the new path is the hash of an index
      row whose path is gone) finds the pair in any folder. The same
      end when a scan has no feed entry for the move and compares the
      folder of the old path before the folder that holds the entry in
      the cloud: the old name is on neither side there, so its index
      rows are forgotten before the id could pair the two names.
- [ ] E-49 Carry a folder that is renamed on this device to the cloud
      as one move. It goes file by file today: each file is one
      `Provider::move_object`, the provider makes a new folder, and the
      old one is removed. Another device then gets one rename for each
      file, and not one for the folder. The index holds the id of each
      synced folder since 2026-11-25, and `name_carries` moves a folder
      with one call, so the upload gate can carry the folder when the
      first file under the new name is found to be a moved one.
- [ ] E-50 A leftover folder stays when its volume takes a copy for
      the trash. A folder that the cloud no longer has, and that holds
      only files an OS wrote by itself (`.DS_Store` and the like), is
      removed with them. On a volume that refuses a trash folder the
      discard of such a file needs a copy, which only a worker makes.
      The file then stays, and the folder with it. Read from the code;
      no run showed it.
- [ ] E-51 A row of `name_carries` stays when its move can no longer
      be made. The row is written in `two-way` and `push-only`. When
      the profile changes to `pull-only` before the move ran, nothing
      removes the row, and the scan leaves its three paths out of the
      comparison for as long as it is there. A change of the sync mode
      has to drop the rows of the profile.
- [ ] E-52 The daemon remembers a rename that it made for 30 seconds
      (the life of a self-write record). A watcher that reports the
      rename later than that is not recognised: each file under the
      new name then gets one remote lookup and one hash, and nothing
      is transferred. A burst that the watcher folds into a scan can
      take longer than 30 seconds to arrive. Inside the 30 seconds the
      record hides too much: a file that the user removes under the
      old path in the moment of the rename, and a folder that the user
      moves in at that moment, read as the echo of the rename and wait
      for the next scan. In `pull-only` a file that the user changed
      around the rename gets no restore until that scan.
- [ ] E-53 Measure the start of a profile with a large index. Before
      the first tick the runtime reads the id of every synced file and
      folder and hands the list to the provider. That is one read of
      two tables on the thread that starts the profile; it was not
      measured on a tree of a hundred thousand files.
- [ ] E-55 Revert content that the cloud changed under a name that a
      `push-only` profile moves back. When both sides gave a file
      another name and the cloud also changed its content, the cloud
      object is moved to the name on this device. In `two-way` the file
      is compared at once and the new content is fetched. The upload of
      a mirror compares nothing and always sends, so `push-only` sends
      only a file that this device changed: content that the cloud
      changed stays there until the next scan of everything reverts
      it. A comparison that sends only when the cloud object differs
      would close it with no transfer for an unchanged file.

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

- [ ] E-31 Small files move at about 530 a second to a cloud folder on
      this device, and the tick thread is the limit. Measured on
      2026-11-24 with a release build in `Idle` on 12 cores: 6 000
      files of 100 bytes reach the cloud folder in 11 s, the first
      after 2.5 s (310 a second and 19.3 s before, on the same host).
      Three changes gave that: small transfers run 30 at a time next
      to the 6 transfer slots and share each flush of the drive
      (`F_FULLFSYNC`), every write of a tick is one write to the log of
      the state database, and a status snapshot is built four times a
      second under load and not at each tick. With every stage at 32
      at a time the same burst ran at 610 a second, so the stages
      before the upload (the probe of the planner and the hash, 6 at a
      time each) hold a little, and the rest is the tick thread: one
      commit and one harvest for each wake-up, and the checks of a
      name's spelling on the volume (`stored_spelling`). Left to try:
      fewer wake-ups under load, so a tick harvests more than a few
      outcomes, and the planner and hash stages wider for small files.
      The removal of a folder of 6 000 files is one deletion in the
      cloud and takes 3 s.
- [ ] E-44 The first look for another spelling of a folder still
      reads every index row under the folder when all of them are
      spelled as the folder is (`sync_index_row_spelled_otherwise`): 8
      ms for a folder of 100 000 files, in a release build, on the
      tick thread. The answer is now remembered until the index gets a
      new path, so a folder removal that is planned again while it
      waits costs no second read. A seek that needs no read at all is
      still to find: no index answers "in this range of folded keys
      and outside that range of paths". A table of the folder
      spellings the index holds would, at the cost of a write for each
      new folder. Found by the review of the pull request, 2026-11-23.
- [ ] E-45 A scan that the user asks for, or that a profile starts
      with, holds every other piece of work of the profile until the
      scan ends, for 60 seconds at most
      (`STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS`). The wait is
      there for a start after a crash, where the scan rebuilds what the
      daemon held in memory. `vapor reconcile` and Sync now queue the
      same scan with the same wait, so on a large Google Drive folder a
      file that changed on another device arrives up to a minute later
      because the user asked for a sync. With the test account on
      2026-11-24 a profile that was enabled again got a waiting file
      after 64 s, on a tree of 77 folders whose scan took 90 s then and
      takes about 10 s now. Decide which scans need the wait, and
      whether a scan asked for by the user needs it at all.
- [ ] E-47 A disk that is mounted inside the sync folder and then
      unmounted reads as deletions. The watcher reports nothing at the
      unmount. The next scan of everything (at start, once a day, or
      on request) finds the empty folder where the disk was, with an
      index row for each file, and removes the files from the cloud:
      "Removed from the cloud ... it was deleted on this device". The
      identity check covers the two roots only. Seen in the e2e manual
      sandbox on 2026-11-24, with a disk image of 6 files mounted at a
      folder of the local root: the scan removed all 6 from the cloud
      folder. On the `filesystem` provider that removal has no undo
      (P-19). A folder on another device than its root could be held
      as a root is: wait for the disk, never mirror its absence. The
      mass-delete guard caught this only for 10 files or more and a
      quarter of the profile; it is gone since 2026-11-24.

## Providers

- [ ] P-1 Give the second of two Google Drive files with one name in
      one folder a local name of its own (an alias, as a case collision
      gets). Today one of the two syncs, the choice is stable, a
      warning names the other, and since 2026-11-24 an
      `unsyncable-name` question says that a second file of that name
      stays in the cloud. Checked with the test account on 2026-11-24:
      both devices ask, and the question goes when one of the two
      files gets another name in Google Drive.
- [ ] P-3 Check the HTTP transport on a network that inspects TLS, and
      follow a proxy that an auto-configuration script names. The
      transport now trusts the system's root certificates next to the
      shipped ones, and reads the proxy from `HTTPS_PROXY`,
      `ALL_PROXY`, and `NO_PROXY`, or from the network settings of
      macOS. A stand-in for a proxy proves in Tier 1 that a request
      asks it for a tunnel. Nobody ran it on a network that inspects
      TLS or behind a real proxy, and no test may use the network. A
      proxy that needs a sign-in other than a user and a password in
      its address, and a script that names the proxy (PAC), are not
      followed.
- [ ] P-14 A young folder that is moved into the synced folder of
      Google Drive can still wait for the next scan of everything.
      Drive has one entry for a folder that moved and none for what it
      holds. The provider takes a folder that it never saw and that
      was made in the last two minutes for a new one, whose files come
      with entries of their own, unless the feed showed that folder
      or a file in it outside the synced folder a moment before. Two
      cases are left: a folder that was made and filled while no
      daemon polled, and a young folder that holds only folders at its
      top. An older folder gets its walk. Checked with the test
      account on 2026-11-24: a folder that left and came back, and a
      young folder that got a file outside and was moved back, each
      held all its files on both devices within seconds.
- [ ] P-15 Where a device still works a cloud rename file by file, it
      can put the files in the trash and download them again. A folder
      or a file that gets another name in Google Drive is one rename
      on the device since 2026-11-25, with no transfer. The
      file-by-file path stays for a rename that is not paired: work is
      queued under the old path, the new name is taken on the device,
      or the index has no id for the entry. There the removal of the
      old path waits for the download of the new one only when that
      download is queued before the removal is first planned, and only
      for 9 seconds. Both devices end right, and the trash keeps the
      files. Seen with the test account on 2026-11-24, before the
      rename in one step. A second way into the same result: an upload
      of the old path that is planned between the rename in the cloud
      and the report of the feed (a watcher event that the daemon
      still held back, a scan). It finds the old name gone and the
      local file as it was last synced, so it removes the local file,
      and the rename is then a download. Seen on 2026-11-26 in the
      end-to-end scenario of a cloud rename, on the `filesystem`
      provider, about one run in six until the scenario waited longer.
- [ ] P-16 A folder signal costs a walk of everything under the folder
      that holds the path. A folder that gets another name in Google
      Drive is one rename since 2026-11-25 and costs no walk. The
      signal (`FolderMoved`) stays for a folder that left the synced
      folder, for one that moved in from outside, for one whose new
      name another folder has, and for a rename that the device could
      not make in one step. Its walk enters every folder under the one
      that holds the path: one request for each folder of the tree. A
      move between two folders asks for two walks, also when one of
      them holds the other. A walk that pairs one level of names, and
      then enters only the folder that moved, would cost a few
      requests. Found by the review of the change, 2026-11-24.
- [ ] P-20 Pair a renamed folder on the `filesystem` provider. It has
      no id for a folder or a file, so a folder that gets another name
      in such a cloud folder is one folder that is gone and one that
      is new. The device moves the files one by one, with no transfer,
      and removes the old folder when nothing of the user is left in
      it. A name that both sides changed ends as two folders there,
      each with every file. An id that the volume keeps for a file and
      a folder (the inode on a local volume) would let the provider
      report one move, where the volume keeps it across a remount.
- [ ] P-21 Give a folder that this device makes in the cloud its id in
      the index at once. The index gets the id of a file when its
      transfer ends, and the id of a folder when a scan compares the
      folder on both sides: at the next start at the latest. Until
      then the provider holds the id in memory only. A stop of the
      daemon in that time loses it, and a rename of that folder made
      while the daemon is stopped is applied file by file, by the ids
      of its files, with no transfer.
- [ ] P-22 Two devices that send the same new file to Google Drive in
      the same moment make it twice. Drive lets two files in one
      folder have one name, and an upload of a new file is a lookup
      and then a create: both devices find the name free and both
      create. Each device then syncs one of the two, and a scan asks
      about the other as a name that cannot sync. Seen with the test
      account on 2026-11-26: the cloud root went to the trash, both
      devices answered `recreate`, and both filled the empty folder at
      once; every file and every folder was there twice, with the
      same content. No data was lost. A check after a create, like
      the one after an overwrite, could remove the copy of this device
      when the other file has the same content and the lower id.
- [ ] P-17 The check of the cloud root costs Google Drive requests all
      day. Each profile checks its two roots every 15 seconds
      (`ROOT_CHECK_INTERVAL_SECONDS`), and on Google Drive one check
      is two requests for each folder of the cloud root's path: 11 520
      requests a day for `/Vapor`, on a device where nothing changes.
      The changes feed already reports a root folder that moved, got
      another name, or went to the trash. A check that follows the
      feed, with the timer as a fallback at a longer interval, costs
      nothing while nothing changes. Seen with the test account,
      2026-11-24.
- [ ] P-19 A trash for the cloud side of the `filesystem` provider. A
      deletion that Vapor carries to a cloud folder on a mounted volume
      removes the file there for good. Google Drive moves it to its
      trash, and a device keeps what Vapor removes in Vapor's trash.
      Since the mass-delete guard is gone (2026-11-24), a mistaken
      removal on a device has no undo in such a cloud folder once it
      synced. A trash folder in the cloud root, kept for the retention
      of `trash.retentionDays` and hidden from listings as the other
      internal names are, would give one (owner, 2026-11-24: a separate
      feature).
- [ ] P-12 Finish the Drive check for a replaced version. The check
      outlives the daemon: the provider gives the engine a record of
      each check, the engine stores it with the outcome of the
      transfer, and the next start hands it back (`data-flow.md`
      §Conflict handling; the follow-up rows of the contract in
      `provider-onboarding.md`). A stop that left no record is covered
      too. Drive committed the write and the daemon stopped before the
      record was saved: a kill before the answer came, a kill after it
      and before the tick that follows it ended, a tick whose writes
      the database did not take, or a clean stop while the commit was
      on its way. At the next start the engine knows that write by its
      tag: the cloud object carries an op-id of this device and this
      profile that the index row does not hold. The engine tells the
      provider (`note_unrecorded_write`), whatever the local file
      holds, and the provider owes the check from there. What is still
      open:
      - The object was written again before this device started. When
        another device that runs Vapor wrote it, its tag replaced the
        tag of this device: nothing marks the lost write, and no check
        is made. When a program that keeps the tag wrote it (the Drive
        website), the check takes the newer content for the write of
        this device, and finds a replaced version only in the 50
        seconds before that content. In both cases a version that the
        lost write replaced unseen is only in the version history of
        the file in Google Drive. A move of this device tags the
        object too: when the step whose outcome was lost was a move,
        and the content came from another writer, the check takes that
        content for a write of this device and can keep a copy of a
        version that nobody lost.
      - The check finds a write of the same moment by its time. A
        writer can set the time of its version (Drive for desktop sets
        the time the file has on its disk): a write that lands inside
        the round trip of an upload's commit with a time of another
        day is not found. A replaced version that its writer dated
        later than the upload's own (a clock that is ahead) is listed
        after it, and the check then finds no base and ends. The check
        also ends when the list does not hold the upload's own version
        yet; the runs with the account never saw the list lag a write,
        and nothing else says that it cannot. `files.version` grows by
        2 or 3 for one overwrite and by 5 to 7 for two, so it does not
        tell a second writer.
      - A check that runs late reads the wrong holder of the name. The
        check takes the content that holds the name when it runs for
        the write that won. After a stop of the daemon and one more
        save of the file, that content is a later one: the version
        that won is then kept as a copy (one copy too many), and the
        version of this device is not kept when the write that won
        came from a program other than Vapor.
      - A copy that is made late goes to the folder and under the name
        that the file had at the upload. A file or a folder that moved
        since (up to 30 days) leaves the copy at the old place, which
        can be outside the synced folder now. A file that is in the
        trash is not told apart either.
      - Nobody measured the commit of a write with no recorded
        outcome, so 10 seconds stand for it. A replaced version whose
        time is more than 50 seconds before the time of the write, or
        more than 11 seconds after it, is not found. A commit takes
        that long for a last chunk on a slow link.
      - The copy of a replaced version moves its bytes inside the
        changes poll, one step for each poll, outside the transfer
        caps and the bandwidth ceiling. With the record stored, the
        copy can run as a transfer.
      - The copy is made by the device whose upload holds the name,
        so the other device's edit is only in the version history
        until that device polls: a device that goes to sleep right
        after its upload keeps the copy waiting.
      - A state database that is set aside (another schema version,
        damage) takes the stored records with it, and their checks
        are not made. It takes the index rows too, so the engine has
        no base for a write with no recorded outcome and makes no
        call.
      - A profile that is composed again in a running daemon (a
        change of its settings, a retry after a failure) gets a new
        provider, and that provider takes the stored records. A poll
        of the old provider can still be at work for a moment. If
        both copy the same version in that moment, Drive holds two
        files of one name: one syncs, and the other is reported as a
        duplicate. Reasoned from the code; no run showed it.
      - After a stop the provider knows what the uploads of the
        checks it gets back wrote, and nothing of the other uploads.
        Three saves of one file by this device inside one moment,
        with the checks of the last two ended and the first still
        waiting at the stop: the check of the first then reads the
        middle save as a write of another device and keeps it as a
        copy. No data is lost.
- [ ] P-5 Decide the Google verification path. The `drive` scope is
      restricted: Google requires app verification and a yearly
      security assessment before more than 100 users can sign in, and a
      client in Testing status signs every user out each week
      (`docs/operations/provider-auth-operations.md`). The choice is
      verification, or a narrower scope with what it costs. It gates
      general availability of the Drive provider and waits for the
      owner.
- [ ] P-9 Small files move through Google Drive at about 2 a second.
      400 files of 1 KB took 214 s from one device to the other (the
      upload, the changes poll, and the download together), and 31
      files with one of 20 MiB took 110 s. On `80c842a` the 400 files
      took 301 s (1.3 a second), their removal 286 s, and the 31
      files 64 s. One save took 9 s to 12 s from device to device. On
      the build of 2026-11-24 the 400 files took 208 s (1.9 a second),
      their removal 82 s, the 31 files 32 s, and one save 5 s to
      11 s. The later runs of that day timed the upload apart:
      the 400 files took 154 s with a quiet host and 181 s with a soak
      on it, and the queue of the sending device was empty after 63 s
      and 73 s (about 6 uploads a second). So more than half of the
      way is the changes poll and the downloads of the other device.
      Measure those two apart, then
      raise what runs in parallel within the rate limits of the API.
      The engine has a lane for small transfers since 2026-11-24, five
      for each transfer slot, and Google Drive leaves it off
      (`small_transfers_share_a_commit`): each of its transfers is a
      request that counts against the rate limit, so the width for
      Drive has to come from a measurement with the account.
      Found with the dedicated test account in a Mac guest,
      2026-11-22.
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
- [ ] PL-8 On a build with no Developer ID, a daemon started as
      `vapor run` with Google Drive cannot read the sign-in that
      `vapord` wrote to the keychain. The keychain lets a process read
      an item with no dialog only when the item names the code of that
      process, and an item holds the code of the one binary that made
      it. The daemon binary makes it, so the daemon of an install
      reads it, and `vapor run` holds for a sign-in that a new sign-in
      does not give it. A release signed with a Developer ID has one
      team for both and is not affected. A second item made by `vapor`
      itself would close it, at the cost of two items that a sign-out
      and a refresh must keep in step; `vapor run` could also start
      the `vapord` next to it. The look for the cloud folder in `vapor
      init` no longer reads the keychain: it uses the sign-in of that
      run from memory. Found by the review of the pull request,
      2026-11-23.
- [ ] PL-1 Check in Finder that a file which Vapor moved to the macOS
      trash offers Put Back. The move goes through
      `NSFileManager.trashItem` now, and a contract test proves the
      file is in the trash of its volume; only a person at Finder sees
      the menu item. Needs the owner, in a guest window.
- [ ] CT-5 Run the contract of `ServiceInstaller` against a native
      implementation in Tier 1. Every other `core/platform` trait has
      an in-memory fake and one contract body that the fake and the
      native implementation both run. The native `ServiceInstaller`
      registers a real job with the service manager of the host, which
      no test may do on a contributor's device: its round trip runs in
      Tier E2E with `--full` on a disposable runner and in Tier VM. A
      service manager that a test owns (a launchd domain or a systemd
      instance of its own) would let Tier 1 run it.

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
- [ ] C6-12 `core/platform/proxy` on Windows: the WinHTTP proxy
      settings of the user, into `ProxySettings::https_proxy`. It
      reports none today, so only `HTTPS_PROXY` in the environment is
      followed there.
- [ ] C6-13 A rename of a folder that refuses a taken name, native on
      Windows (`MoveFileExW` with no replace flag). The portable
      fallback of `fs_ops::atomic_noclobber_rename` claims the name
      with an empty folder, gives the claim up, and renames: a folder
      never takes the place of a file or of a folder with content
      there, but an empty folder that appears in that instant can be
      replaced. A rename that fails removes nothing at the name. macOS
      and Linux rename in one call.
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
- [ ] PL-10 Check on a Linux desktop what `secret-tool` answers for a
      secret in a keyring that is locked. The store takes `lookup` with
      exit code 1 and no output for "could not ask" when `search` still
      lists the item, and for "not there" when it does not. That is
      what the source of libsecret says, and a stand-in for the tool
      proves the store's side of it in Tier 1. Nobody ran it against a
      real locked keyring. The same run settles whether `secret-tool
      search` prints the attributes on stdout or on stderr: the listing
      of the store reads stdout only.
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
- [ ] CT-11 Add the properties that explicit cases cover today:
      throttle monotonicity, ignore-rule precedence, and the IPC
      handshake skew matrix. Path normalization, scheduler
      superseding, retry backoff, the order of the durable queue, and
      conflict copy names have theirs, and the release gate runs all
      of them at 4 000 cases each (`scripts/perf.sh`).
- [ ] CT-12 A Tier 1 test of a profile that ends while the daemon
      runs with transfer workers on. The end of a runtime on its own
      thread, and a profile with the same id that waits for that end
      (`retiring`, `awaiting_retirement`), run in Tier E2E only: the
      Tier 1 test of the live profile set runs every provider call on
      the tick thread. Found by the review of the pull request,
      2026-11-24.
- [ ] CT-13 Run a backend with ids through the provider contract
      suite. The three backends of
      `core/providers/tests/provider_contract.rs` have no ids, so the
      suite locks only that such a backend reports none. That an id is
      unique, stays with a moved object, and is the same in a listing,
      a lookup, and a transfer is tested for Google Drive alone, on
      the scripted transport. A small backend with ids in the suite
      (or the Drive provider on that transport) would hold every later
      backend to the same contract. Found by the review of the pull
      request, 2026-11-26.
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
      a backward wall-clock step while the daemon runs, which needs a
      host whose clock a test may set, and the CLI of one build against
      the daemon of a build with another IPC version, which needs two
      builds. The skew matrix is in-process, and
      `core/cli/tests/fake_daemon.rs` runs the built CLI against a
      stand-in that answers as a daemon of another version. The
      upgrade of the state database has its scenario (S84).
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
- [ ] TR-14 Run only the jobs of one system for a change to its
      native files in the platform layer. Today any file under `core/`
      runs all nine CI jobs (`scripts/scope.sh`). A file such as
      `core/platform/src/fs_watch/macos.rs` is compiled on one system
      only: each of these modules is declared under
      `#[cfg(target_os = "...")]` (checked 2026-11-26). The gate would
      then rest on that convention, so it needs a test that fails when
      a module of that name is declared in another way.
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
