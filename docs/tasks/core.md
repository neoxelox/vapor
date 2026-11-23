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
- [ ] E-13 Take the copy of a discard off the tick thread. A file that
      Vapor removes on a volume that refuses a trash folder is copied
      to the trash of the runtime directory by the tick thread, for as
      long as the copy takes. It is the one read of file content that
      `AGENTS.md` §3 still has on that thread.
- [ ] E-14 Decide what to do with a local folder that a rename in the
      cloud emptied of synced files and that still holds ignored
      files. An empty folder is removed. A folder with a `.DS_Store`
      in it stays for good, and so does one with a `node_modules`
      folder, which must stay. The rule has to tell the two apart.
- [ ] E-15 Try again a profile whose composition failed for a reason
      that can pass. A profile whose state database cannot be opened
      at the start (a full disk, a lock) is suspended until the
      configuration changes or the daemon restarts. Only a profile
      that ran and then failed is composed again after 1, 5, and 30
      minutes.
- [ ] E-16 Make the check of the cloud root that guards a local
      removal cheaper and faster. Each cloud deletion reads the
      identity of the cloud root by itself: on Google Drive that is one
      request for each part of `cloudRoot`, for each file of a burst.
      A removal that finds the root in doubt asks the runtime for a
      root check at once, but only the local check runs at once; the
      cloud check keeps its cadence, so the hold comes up to 45
      seconds later (no file is removed in that time).

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

- [ ] E-35 Work inside a folder in the seconds after the folder was
      renamed by capitals only goes wrong in three ways, with one
      device and no change in the cloud. `Folder` to `FOLDER`, then
      1.5 s later: an edit of a file in it became a conflict with the
      original bytes under the name; a rename of a child folder
      brought the old child back with 3 of its 5 files next to the new
      one; a deleted file came back. Each happened on the first try,
      and none happened when the daemon was quiet between the steps.
      The moving workload hit all three 40 s apart (75 files that came
      back and one conflict copy in 150 s), because E-19 keeps the
      window open for minutes. While the moves of the rename wait, an
      intent for a path under the new spelling finds no index row and
      takes the "keep the data" path: a conflict, or a download of the
      object under the old spelling. Look the row up by the spelling
      the index holds, or hold the intents of a folder until its
      rename has landed.
- [ ] E-17 An event in the cloud that changes no bytes turns a waiting
      local edit into a conflict, and the old bytes get the name. A
      file that another tool put in the cloud folder, a local edit not
      uploaded yet (a pause, a backlog, a failed upload), and a
      `chmod`, a `chflags`, or a rewrite with the same bytes of the
      cloud copy: Vapor moves the edit to a `~conflict-` copy and
      downloads the old content under the name, on both sides.
      Reproduced 6 times in 6 on the filesystem provider. On `80c842a`
      a `touch` of the cloud copy does it too (2 of 2 runs); on
      `d18a288` it did not. The feed
      drops an event as "no change" when its hash equals the index, or
      when its op-id equals the op-id the index recorded and the size
      and mtime agree (`remote_sync.rs`, `is_durable_self_write_echo`).
      An object that no Vapor wrote has no op-id: the index holds an
      empty one, the event holds none, the two do not compare equal,
      and the size and mtime are never looked at. The filesystem feed
      sends no hash, so nothing else catches it. Fix both places: an
      event whose size and remote mtime equal the index row is no
      change, with or without an op-id, and a download that finds the
      local file changed while the remote still equals the last synced
      state is an upload, not a conflict. The Google Drive feed sends
      a hash, which should drop the same event there; that was not
      run.
- [ ] E-18 A file edited while an ignore rule hid it becomes a conflict
      when the rule goes, and the old bytes get the name. `a.secret`
      synced, `*.secret` was added to `postIgnoreRules`, the file
      changed here, the rule was removed: the cloud's old content took
      the name on both sides and the edit went to a `~conflict-` copy.
      The cloud copy never changed. The pair seems to lose its index
      row while the rule hides it, so nothing says which side moved.
      Keep the row of a hidden path.
- [ ] E-19 After one burst, a whole top-level folder stops syncing
      until it is quiet, for up to 10 minutes. A storm (200 paths in
      2 s: a branch switch, a formatter, an install) is compacted into
      one deferred reconcile, and each new event in the compacted
      folder pushes the reconcile back
      (`DEFERRED_RECONCILE_MAX_DELAY_MILLIS` is 600 000). The detector
      takes the deepest folder over the threshold (`storm.rs`), but
      the run compacted the parent of both projects: the log says
      `subtree=<root>/code` with the burst in `code/demo`. A likely
      path: the windows of the ancestors keep the paths of the burst
      for 2 s, the events of the compacted folder no longer reach the
      detector, and one event next to it then trips the common parent.
      Measured with one busy project in `code/demo`: a save in another
      project, `code/other-project`, took 1.4 s before the burst and
      54 s to 307 s after it, until the busy project stopped; a save
      outside `code/` took 1.5 s all the time. The second run gave the
      same numbers. Keep a compaction from
      growing to a parent because of the burst it already took, keep
      single saves flowing next to a compacted folder, and bound the
      wait by seconds while the device is idle.
- [ ] E-37 A deletion that waits in its move-settle delay when the
      mass-delete guard trips is not held with the burst, and
      `discard` then restores the others and not that one. Seen in CI
      on `ubuntu-latest`, e2e scenario S40: 12 cloud deletions, one of
      them "Intent deferred during planning ... a queued download may
      be this file renamed in the cloud delay_ms=3000", the guard
      tripped 3.7 s later and held 11, `discard` said "11 deletion(s)
      dropped, 11 restore(s) enqueued", and the oracle found
      "victim-01.txt: present locally, missing in the cloud root". The
      file that the user chose to keep stays deleted in the cloud. S40
      fails when the timing falls that way, so it is a flaky test
      under `AGENTS.md` §9.6 until this is fixed. Hold a deferred
      deletion of the same direction with the burst.
- [ ] E-36 While a compacted folder waits for its deferred reconcile,
      `vapor status` says that almost nothing is left to do: "Queue: 0
      queued" on `d18a288` and "Queue: 1 queued" on `80c842a`, with
      `reconcile.state: idle`, for a burst of 1 500 new files. The
      files show as queued work only when the wait is over; in the
      coding runs the status was empty for 4 minutes while hundreds of
      changes waited (E-19). The app then shows a synced state that is
      not true, and a person cannot tell a wait from a stop. Report a
      folder that waits for its scan as waiting work, with the number
      of changes it holds and the time of the scan.
- [ ] E-38 A burst waits 30 seconds before its first file moves, on a
      device that is idle and plugged in. 1 500 new files: no queued
      work for 30 s, then 1 384 intents, and the files were in the
      cloud about 45 s after they were written. A project of 603
      small files (6 MB) took 36 s and a library of 209 files (151 MB)
      took 34 s, while 1 GiB in one file takes 11 s. The wait is
      `DEFERRED_RECONCILE_DELAY_MILLIS` (30 000), with a release after
      5 s of quiet that these runs did not get. The delay protects a
      device under load; on an idle one it only costs time. The second
      run: the first file after 33 s, all 1 500 after 39 s. Start the
      scan when the burst has been quiet for a second or two in
      `Idle`, and keep the long delay for the throttled states.
- [ ] E-39 While a mass-delete question is open, the same deletions
      are held again and again, and the daemon grows and works with
      nothing to do. `rm -rf node_modules` on `80c842a`, no answer for
      25 minutes: 43 879 held rows for 10 513 files, resident memory
      from 41 MB to 360 MB, the state database from 8 MB to 22 MB, and
      25% to 45% of one core the whole time. Each scan finds the
      deleted files again, plans their deletions again, and the guard
      holds them next to the ones it has. `apply` then removed the
      cloud files in 41 s and the memory fell to 47 MB. A question
      that waits overnight costs the device a core. A deletion that is
      held for a path must stand for every later one of that path, and
      a scan must not plan work that a held intent already covers.
- [ ] E-40 A deletion takes 5 s to 7 s to reach the other side, a
      rename 1.5 s to 3 s. One change at a time on an idle device,
      `80c842a`: a deletion made here was gone from the cloud folder
      after 5.3 s, and one made in the cloud folder was gone here
      after 6.7 s; with a user at the device 4.8 s and 24 s (61 s at
      most, the poll of P-11). Through Google Drive a deletion took
      16 s from device to device. The first planning of a deletion waits
      3 s to see whether a create of the same file follows, so a move
      is one move. Shorten that wait when nothing of the same size is
      queued or arriving, and measure what each cloud deletion pays
      for the check of the cloud root (E-16).
- [ ] E-41 A file whose type has no class of its own waits 4 s before
      it moves (`DEFAULT_DEBOUNCE_WINDOW_MILLIS`). One 5 MB `.bin`
      file took 4.3 s to reach the cloud folder, where a `.ts` file
      took 1.5 s and a 200 KB `.jpg` 1.6 s; 1 GiB then moves at
      96 MiB/s, so the wait is most of the time of a small file. A
      fixed window by file name guesses when a file is complete. Use
      what the file does: start when its size and time stood still for
      a short moment, and keep a longer window only for a file that
      still grows.
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
      rename counts as deletions. In the second run the count of the
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
- [ ] E-21 A file replaced by a folder of the same name, or a folder by
      a file, on one side never syncs on its own. The intents retry
      with "Not a directory", "Is a directory", and "File exists", and
      a `type-mismatch` question opens only when a reconcile walk
      passes the folder, which can be the daily one. The index says
      that the other side still holds what was last synced, so a
      change on one side can apply with no question. The same holds
      when both sides make the name at once: the question must open
      when the intent fails, not at the next walk.
- [ ] E-22 A download that cannot be applied leaves one staging file
      for each attempt in the user's folder. The staging name carries
      the op-id, and the op-id carries the attempt
      (`.vapor-tmp-dl-<device>-op527-a0…a9-t<ms>`). A target that is
      locked in Finder, or a name that is a folder, left 7 and 10
      files; they stayed after the download succeeded and after a
      restart, and the sweep takes them after 24 hours. Each is a full
      copy: a 1 GB file fills the disk one attempt at a time. Use one
      staging name for an intent and remove it when an attempt fails.
- [ ] E-23 A file that cannot be read or replaced is retried without
      end, and the user is not told. A local file with mode 000, a
      cloud file with mode 000, a target with the Finder lock: the
      intent backs off to about 15 minutes and stays in the queue, so
      `vapor status` says "N queued" for good and only `vapor
      diagnostics` has the reason. Count such files apart from queued
      work, name them with their reason in `vapor status` and in the
      app, and stop calling the profile busy for them.
- [ ] E-24 A retry does not wake when its cause is gone. A file made
      readable again, a lock removed, a `type-mismatch` answered, and
      `vapor reconcile` all left the intent in its backoff (attempt 11:
      about 15 minutes). After `prefer-local` the cloud folder was
      empty for that time, with the files only on this device. A new
      event for the path, an answer that releases it, and a reconcile
      on request must make the intent due at once.
- [ ] E-25 A reconcile marker of a folder that is gone keeps the queue
      at 2 for 13 minutes. `rm -rf node_modules` while the walk was
      inside it: "Reconcile walk cannot stat a local entry; deferring
      the directory", and the marker waits out the full deferral.
      `vapor status` said "Queue: 2 queued" for 13 minutes at 0% CPU.
      Not seen again in one run on `80c842a`. Drop the marker of a
      folder that neither side holds.
- [ ] E-26 Empty folders pile up in the cloud. After a local rename or
      removal of a folder the cloud keeps the old folder, empty: 268
      after ten minutes of coding (`.git/objects/xx` among them), and
      770 after `rm -rf node_modules`, which leaves `node_modules/` in
      the cloud as a tree of empty folders (768 in the second run).
      The same on this device was fixed on 2026-11-22, and E-14 has
      what is left of it. Remove a cloud folder that a deletion or a
      move emptied.
- [ ] E-27 A folder renamed in the cloud by capitals only keeps its old
      spelling here (`CaseDir` to `casedir`). A file renamed that way
      follows in both directions, and a folder follows from this
      device to the cloud.
- [ ] E-28 A read-only file that comes from the cloud gets a
      `<name>.vapor-meta.json` next to it in the local folder. The
      download sets the mode before the op-id tag, the tag then fails
      with a permission error, and the fallback writes the side file
      (`executor.rs`, "Tag after the rename"). The upload direction
      tags first. Git writes its objects and packs read-only: five
      commits made on the cloud side left 17 side files in the local
      `.git`, `git count-objects -v` says "garbage: 17", and `git
      fsck` prints "bad sha1 file" for each.
- [ ] E-29 The `unsyncable-name` question is withdrawn and asked again
      at every walk, and `skip` does not stick: 33 decisions for one
      file with a backslash in two hours, each with the warning
      "asking once". The same name in the cloud folder is dropped with
      no question, no warning, and no line in `vapor ignored`.
- [ ] E-30 A `touch` in the cloud downloads the file again and rewrites
      the local copy, and a `chmod` in the cloud reaches this device
      while a `chmod` here never reaches the cloud. `data-flow.md`
      says that a change of mode alone does not sync; the cloud side
      does it by accident, through that download. Compare the content
      before a download, and make both directions do what the document
      says.
- [ ] E-31 Small files move at about 160 a second with nothing near a
      ceiling. `npm ci` wrote 10 493 files (126 MB) in 2.8 s; the
      upload took 65 s at 23% of one core in `Idle`. Holding 10 493
      deletions took 100 s, and applying them 60 s with stops of 8 to
      20 s at 0% CPU while thousands waited. Find what a file costs
      (one transaction, one `fsync`, one tick each) and batch it.
- [ ] E-32 A file whose name looks like a conflict copy, with no
      conflict behind it (`report~conflict-otherdevice-1700000000000.txt`),
      opens a `conflict` question: "report.txt is gone, but its
      conflict copy is still here". Ask only for a copy that this
      profile made or that has a file under the original name.
- [ ] E-33 Log noise in a healthy run. "Updated throttle state ...
      reason=system CPU load is 69%" comes once a second while the
      percentage moves; "Filesystem provider ensured cloud root" comes
      at INFO every 30 s; a symlink that points outside the root logs
      the WARNING "Dropped stabilized event that resolves outside
      watch root" for each event, though the contract skips symlinks.
- [ ] E-34 Say in the product documents what a synced project loses on
      the other side, with the reason for each: symlinks (so
      `node_modules/.bin` is missing and `npm run` fails there until
      an install), empty folders (so a repository made with `git init`
      is "not a git repository" on the other device until its first
      commit, because `.git/objects` and `.git/refs` are empty), and
      the holes of a sparse file (a 64 MiB sparse file took 16 KiB
      here and 64 MiB there).

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
- [ ] P-11 A change from the cloud side reaches a device that is in
      use a minute late. The daemon reads the cloud side each 5 s in
      `Idle`, each 15 s in `Light`, and each 60 s in `Throttled`
      (`REMOTE_POLL_*_SECONDS`), and a person at the device puts it in
      `Throttled`. Measured between two local folders on `80c842a`,
      one change at a time: a new or an edited file went from this
      device to the cloud folder in 1.4 s to 1.5 s, idle or in use.
      From the cloud folder to this device it took 3.6 s on an idle
      device (9.1 s at most) and 58 s with a user at it (34 s at
      least). The watcher of the `filesystem` provider sees the change
      at once and costs no request; the daemon reads its events at the
      poll cadence all the same. On Google Drive, from one device to
      another, a new file took 9 s to 12 s on idle devices and 58 s
      with a user at them. Read the events of a watcher when they
      come, in every state but `Suspended`. For a
      real cloud, poll sooner when something says a change is near
      (this device just uploaded, the app is open, a poll just brought
      changes), and measure what a poll costs before the 60 s stays.
- [ ] P-10 Two uploads of one Google Drive file that commit at the same
      moment both pass, and one edit is lost with no conflict copy.
      Two profiles of one daemon on one Drive folder each changed
      `doc.txt`: the folder ended with one version, the other device
      then downloaded it over its own edit ("Remote changed while the
      local copy stayed at the last sync; downloading instead of
      uploading"), and nothing went to the trash. 6 of 6 runs, with
      the base version quiet for 25 s or fresh, paused or not, and 5 of
      5 again on `80c842a`, where either edit can be the one that is
      lost.
      `data-flow.md` promises keep-both for exactly this pair of
      profiles. The guard reads the hash and then writes
      (`reverify_precondition_before_commit`), and Drive v3 has no
      conditional update of content, so two commits inside one round
      trip both pass. After a commit, read the revisions of the file:
      when the revision before this device's one is not the one the
      index knows, another writer came in between, and its revision
      becomes the conflict copy. Not tested with two machines: one
      daemon puts both uploads in one tick, and two machines need the
      same second. Run the same pair of profiles on the filesystem
      provider too. Found with the dedicated test account in a Mac
      guest, 2026-11-22.
- [ ] P-8 The filesystem provider reads a pipe, a socket, or a device
      in the cloud folder as a file. A named pipe there blocks a
      download worker in `open` for good ("stage downloading (1240540
      ms)"), a later change of that path waits behind it, and the
      daemon cannot stop (PL-5). A socket fails with "Operation not
      supported on socket" and is retried without end.
      `remote_entry_for` and the feed (`report_directory_files`, the
      event mapping) call everything that is not a symlink and not a
      folder a file. Skip what is not a regular file, as the local
      side does. Found in the Mac guest, 2026-11-22.

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
- [ ] PL-1 Move a file to the macOS system trash through
      `NSFileManager.trashItem`, so Finder offers Put Back and a file
      on another volume goes to that volume's trash. The rename into
      `~/.Trash` that ships today gives neither. Needs a manual check
      in Finder.
- [ ] PL-2 Read the state of a launchd job without matching the text
      of `launchctl print`, which Apple does not keep stable. The
      installer matches that text today to tell a loaded job from a
      missing one.
- [ ] PL-4 Declare the `notify` dependency of `core/platform` only for
      the targets that build its backend (macOS and Linux). On Windows
      nothing uses it, and a newer cargo warns about an unused
      dependency in the lint job there. The warning does not fail the
      job today.
- [ ] PL-7 After a sign-in from a terminal, the daemon cannot read the
      sign-in from the keychain, and sync does not start. Two causes,
      both seen in a Mac guest on 2026-11-23 with `vapor auth login
      --token -` for Google Drive. First, the access list of the
      keychain item names the daemon only when `vapor` was started by
      its real path. `companion_binaries` (`secrets/macos.rs`) looks
      next to `current_exe()`, and through the link that the installer
      puts on the `PATH` that is `/usr/local/bin`, where no `vapord`
      is: the item then trusts the CLI alone (`security dump-keychain
      -a` shows one application; two when the CLI is started as
      `Vapor.app/Contents/Helpers/vapor`). macOS then asks "vapord
      wants to use your confidential information" with a password
      field, the worker of the daemon waits in that call, and the
      profile says "starting: checking the cloud folder" for as long
      as nobody answers. Second, on a build that is signed ad hoc,
      which every prerelease is, the partition list of the item holds
      the code hash of the CLI only: with the access list right, the
      daemon is refused with no prompt, and the profile says "Vapor
      needs to sign in to gdrive again" right after a sign-in. A build
      signed with one team was not tested. Resolve the link before the
      companions are looked up; read the item with no user interface,
      so a refusal is the sign-in hold and never a blocked worker
      (`AGENTS.md` §6); and decide how a prerelease build lets the
      daemon read what the CLI stored.
- [ ] PL-6 A crashed daemon is down for up to 30 seconds. After
      `kill -9` a new daemon ran 24 s later (21 s in the second run):
      the supervisor looks each
      30 s (`HEALTH_TICK_INTERVAL_SECONDS`), and the first restart is
      "at once" only from that look on. Notice the exit when it
      happens (wait on the process, or let the service manager start
      the daemon and keep the crash-loop guard at its start), with the
      crash-loop limits as they are. Found in the Mac guest,
      2026-11-22.
- [ ] PL-5 A daemon with one blocked worker cannot be stopped, and no
      part of Vapor notices that it is dead. The shutdown joins every
      worker with no limit, so a worker in a system call that never
      returns (the pipe of P-8; a network mount that hangs would do
      the same) keeps the process alive after it closed its socket.
      `vapor service stop` and `restart` then fail with "the daemon
      did not stop within 10 s of SIGTERM" and leave it there
      (`service/macos.rs`, `stop_daemon`); launchd, `vapor service
      status`, and the supervisor read the process id and say
      "running"; `vapor status` says "daemon not running". Only
      `kill -9` ended it. The stop must end with SIGKILL after the
      wait, the shutdown must give a worker a time limit, and the
      supervisor must count a daemon that does not answer on its
      socket as down. Found in the Mac guest, 2026-11-22.
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
