# Completing the core runtime

Status: implemented 2026-11-23. The open points wait for the owner.

## Goal

The owner asked for every open task of the daemon, the runtime, and the
CLI to be done before the Mac app work starts, so that the app is a
front end for a runtime that is robust and fast (owner, 2026-11-23).
The work is one pull request.

This spec records no design round. The owner gave the goal and no
rulings on the way. Each place where the work had more than one
reasonable answer is listed under Open points, with the choice that was
built and what else was possible. Until the owner answers, the built
choice stands.

## Facts

- The task files held the findings of the heavy-user session of
  2026-11-22 (`docs/tasks/core.md`, `docs/tasks/cli.md`), the
  follow-ups of the review of 2026-11-14, and older work.
- The rules that could not move: a deletion propagates on the evidence
  of the sync index only, keep-both in `two-way`, no file content on
  the tick thread, a throttle state slows sync and only `Suspended`
  stops it, and no migration of the state database before GA
  (`AGENTS.md` §3 to §5).
- No test may use the network or a real cloud account. What was built
  for Google Drive was checked against the scripted HTTP transport
  only.

## What was built

By what a user notices. The detail of each item is in the document that
owns it.

1. **A local edit is not turned into a conflict copy by a cloud event
   that changed no bytes.** A `chmod`, a `touch`, or a rewrite with the
   same content in the cloud is no change. A copy made in the cloud
   that keeps the time of its original is a second file, not a rename
   (`data-flow.md` §Local to remote).
2. **A change in the spelling of a folder name is one move in the
   cloud.** The index has a folded key for each path, and work inside
   the folder waits for the move (`data-flow.md` §Directory and symlink
   semantics; state schema 8).
3. **A mass-delete question holds every later deletion of its batch,
   costs no CPU while it is open, and one answer applies to all of
   them** (`data-flow.md` §Decisions).
4. **A burst, a deletion, and a cloud change sync in seconds.** The
   scan of a burst starts when the burst is over, a settled file waits
   one second and not four, a deletion waits for its create only while
   one can still come, and a watched feed is read each second
   (`data-flow.md`).
5. **`vapor status` says what cannot sync.** An intent that failed
   five times is counted and listed as blocked, a new event for its
   path tries it again at once, and times are local times
   (`ipc-contracts.md`).
6. **A file where the other side has a folder settles on its own when
   one side made the swap**, and is a question only when both sides
   changed (`data-flow.md` §A file where the other side has a folder).
7. **A daemon that hangs is ended, and a crashed one is back in a
   second.** `vapor service stop` ends with `SIGKILL` after its wait,
   the supervisor ends a daemon that gives no answer for 30 seconds,
   and it notices an exit within a second
   (`operations/macos/launchagent-policy.md`).
8. **The daemon reads the sign-in that a terminal stored.** The daemon
   binary writes the keychain item, and a background read never opens
   a dialog (`operations/provider-auth-operations.md`).
9. **Two uploads of one file that land in the same moment keep both
   versions.** The replaced version stays as a copy named
   `~conflict-replaced-<time>` (`data-flow.md` §Conflict handling).
10. **Two profiles of one daemon on one cloud folder no longer write
    under one operation id**, which mixed two uploads in one file.
11. **Small files move at 340 a second and not 200**, because files
    written together share one flush of the drive.
12. **The macOS trash goes through `NSFileManager`**, the copy of a
    discard runs on a worker, and a profile whose state database
    cannot be opened is tried again.

## Verification

- Tier 1: `./scripts/test.sh`, green on the contributor Mac.
- Tier E2E: `./scripts/e2e.sh`, 80 scenarios pass and 2 are skipped on
  the contributor Mac. S76 to S81 are new, and
  S70 has one more assertion. Each new assertion failed on the base
  build and passes on this one.
- The tests of Google Drive use the scripted transport. No run used a
  real account. The checks that need one are in `docs/tasks/core.md`
  (P-4).
- Tier VM: `./scripts/vm.sh`, 14 scenarios pass, with the launchd
  round trip of the service in a fresh guest. In a manual guest with no
  route to the Internet, a made-up token given to `vapor auth login
  --token -` through the link on the `PATH` was read by the daemon with
  no dialog: the keychain item names `vapord` in its access list and in
  its partition list.
- The soak: `./scripts/soak.sh --seed 4217 --duration 15m --faults all
  --throttle walk`. The first run stopped with 4 violations, all one
  defect: a rename by spelling that was tried while the cloud folder
  could not be read was undone on this device. After the fix the same
  seed finished 12 phases, 794 operations, and 23 faults with no
  violation.

## Open points

Each one is a choice that the build made with no ruling. The first
option of each is what is built.

1. **The name of a version that a second upload replaced.** Built: a
   copy named `{stem}~conflict-replaced-{ms}{ext}`, and the later
   upload keeps the name of the file. The other way: put the replaced
   version back under the name and move the later upload to a copy
   named after its device, as an ordinary conflict does. That needs a
   second write to the cloud that can itself replace a third version,
   or the deletion of a stored version on Google Drive. The built way
   only adds files.
2. **The Google Drive check for a replaced version is in memory.** A
   daemon that stops between the upload and the next changes poll
   makes no copy; the version is then only in the version history of
   the file in Drive (`docs/tasks/core.md` P-12). Both uploads must
   land in the same second for this to matter. A durable record closes
   it at the cost of one more table.
3. **Each overwrite on Google Drive costs one more request**, the list
   of the versions of the file, at the next poll and not inside the
   upload. The `version` field of a file may tell a second writer
   without the list; that needs a real account to check (P-4).
4. **The durability of a transferred file on macOS.** Built: the same
   guarantee as before (`F_FULLFSYNC` before the file gets its name),
   shared between the files written at the same moment. `fsync` alone
   is several times faster and is what SQLite uses for the state
   database by default, but a loss of power could then leave an index
   row for a file whose bytes are gone. That trade is the owner's.
5. **When an intent counts as blocked.** Built: after 5 failed tries.
   A new event for the path tries it again at once, unless the last
   try failed less than 10 seconds ago.
6. **The times of a burst.** Built: in `Idle` the scan of a burst
   starts 1.5 seconds after the last event of the burst and no later
   than 30 seconds after the first; a folder counts as busy from 50
   absorbed events inside one window. In the other states the scan
   waits its 30 seconds.
7. **A settled file waits 1 second.** Built: a file whose size did
   not change inside one second is taken after 1 second, and a file
   that still grows after 4. A program that writes a file in two
   steps more than a second apart is uploaded twice.
8. **How often the cloud is asked.** Built: a feed that the provider
   watches itself (a local folder) is read each second in every state
   but `Suspended`; a feed that costs a request is read at the active
   cadence for 60 seconds after the last change.
9. **A file and a folder under one name.** Built: when the index
   shows that one side made the swap, the other side follows with no
   question; a folder that is empty makes way for a file. A question
   opens only when both sides changed.
10. **A change in the spelling of a name when the move fails.**
    Built: when the adopted cloud root says that nothing is under the
    old name, the rows of the old spelling are forgotten and one scan
    of the folder settles the names; the cloud's spelling then stays.
    No file is deleted on either side. While the cloud root is away or
    cannot be read, the rename waits. The soak found the case where it
    did not wait: the rename was undone on this device.
11. **A sign-in after an update of a build with no Developer ID.**
    Built: the daemon binary writes the keychain item, so a sign-in
    works at once on such a build. After an update of such a build the
    user signs in one more time, because the item names the old
    binary. A release signed with a Developer ID keeps its access.
12. **The supervisor ends a daemon that gives no answer for 30
    seconds**, and the shutdown of the daemon gives its workers 5
    seconds. A daemon whose tick takes longer than 30 seconds for an
    honest reason would be ended and counted as one crash.
13. **The state of the launchd job** comes from the exit status of
    `launchctl print` and from the process table, because the text of
    `launchctl print` is not stable.
14. **The sign-in message** no longer says "again": the daemon cannot
    tell a profile that never signed in from one whose sign-in
    expired.

## Not done

What is still open is in the task files. The larger items: Google
Drive speed for small files (P-9), the Drive file id in the sync index
(P-2), two Drive files with one name (P-1), proxy settings and system
root certificates (P-3), a sync root whose own spelling changes while
the daemon runs (E-9), the contract tests of the remaining platform
traits (CT-5), `vapor profiles enable|disable|rename` (L1-7), and the
test and tool tasks. The items that wait for the owner are marked as
such there (E-12, E-14, E-20, P-5, L1-8).
