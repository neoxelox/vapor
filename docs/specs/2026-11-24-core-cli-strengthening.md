# A faster and more robust core and CLI before the Mac app

Status: implemented 2026-11-24. The design round and the open points
wait for the owner.

## Goal

The owner asked for the core, daemon, and runtime tasks that were still
open, so that the core and the CLI are "very fast, efficient and
robust" before the Mac app work starts, because the app is mostly a
front end for the `vapor` CLI (owner, 2026-11-24). The work is one
branch, `feat/strengthen-core-cli`.

The owner gave the goal and no rulings on the way. Each place where
the work had more than one reasonable answer is under Open points,
with the choice that was built. Until the owner answers, the built
choice stands. The tasks that need a design were not built: their
questions are under Design round 1, and none of them is a decision.

## Facts

- The task files held about 60 open tasks (`docs/tasks/core.md`,
  `docs/tasks/cli.md`). Windows and Linux tasks stay closed until the
  owner opens those surfaces (`docs/tasks/README.md`).
- The rules that could not move: a deletion propagates on the evidence
  of the sync index only, keep-both in `two-way`, no provider I/O and
  no file content on the tick thread, every expensive piece of work
  gated by the throttle state, no migration of the state database
  before GA, and no network in a test (`AGENTS.md` §3 to §5, §9.1).
- A measurement on the contributor Mac (12 cores, release build,
  `Idle`, static throttle inputs, two folders on one drive): 6 000
  files of 100 bytes reached the cloud folder in 19.3 s, 310 a second.
  A profile of the daemon in that burst showed the tick thread busy
  all the time: 41% in the write to the log of the state database at
  each commit, 31% in status snapshots, and the transfers bound by the
  6 transfer slots, which decide how many files share one flush of the
  drive.
- A second process that writes to the state database while the daemon
  is in such a burst waited up to 3.1 s for the lock with SQLite's own
  wait, once every write of a tick was one transaction.

## What was built

By what a user or the app notices. The detail of each item is in the
document that owns it.

1. **`vapor profiles enable|disable|rename`, and no restart for a
   change of the profile set.** A profile that is added, enabled,
   disabled, removed from the file, or renamed applies to a running
   daemon within seconds, and the other profiles keep their runtimes
   and start no scan. `disable` keeps the sync state, the sign-in, and
   the trash. The output of `vapor profiles add` lost its `applies`
   field: the answer is always "now" (`docs/product/configuration.md`,
   `data-flow.md` §multi-profile).
2. **A sign-in that survives a slow consent.** `vapor auth login`
   waits 30 minutes, and a wait that timed out leaves a small process
   on the redirect address for a day that tells a late sign-in to
   start again (`operations/provider-auth-operations.md`).
3. **A locked keychain is a wait, not a sign-out.** The secret store
   has a third answer for a read, "could not ask", next to "not
   there" and "refused". The Google Drive provider keeps the tokens it
   holds, also through a 401, and a daemon that starts before the
   keychain opens reads the sign-in when it does
   (`platform-abstractions.md` §`SecretStore`).
4. **`vapor init` looks for the cloud folder with the sign-in of that
   run from memory**, so the step works on a build with no Developer
   ID, where the keychain gives the stored item to `vapord` only.
5. **Small files move at about 530 a second, not 310.** Small
   transfers have a lane of their own next to the transfer slots,
   every write of a tick is one transaction, and a status snapshot is
   built four times a second under load (`data-flow.md` §Resource
   ceilings; `docs/tasks/core.md` E-31 has what is left).
6. **A folder removal that is planned again costs no second scan of
   the index** for another spelling of the folder.
7. **A name in the cloud folder that cannot be synced is a question**,
   as such a name on this device is. The provider's listing reports
   it (`provider-onboarding.md`).
8. **A removal that the feed of the `filesystem` provider reads late
   is not dropped** until the next scan of everything.
9. **Vapor works behind a proxy and on a network that inspects TLS.**
   The HTTP transport trusts the system's root certificates next to
   the shipped ones, and reads the proxy from the environment or from
   the macOS network settings (`platform-abstractions.md`
   §`ProxySettings`).
10. **The CLI has tests through the built binary for every command
    that talks to the daemon**, against a stand-in for the daemon, and
    its three exit codes are documented and asserted
    (`core/cli/README.md`).
11. **The build fails when `vapor` or `vapord` is over 10 MiB**, and
    the lint says when the local Rust is older than the one CI uses.
12. **Every platform trait but `ServiceInstaller` has one contract
    body that its fake and its native implementation both run**, and
    the order of the durable queue and the names of conflict copies
    have property tests. The release gate runs every property 4 000
    times (`docs/architecture/testing-strategy.md`).
13. **A scan of a cloud folder asks for several listings at one
    time.** A walk asks for the listings of the next folders ahead, 8
    while the device is idle, and a walk that waits for a listing goes
    on the moment the answer arrives. On Google Drive a tree of 77
    folders took 90 s to scan and takes about 10 s (`data-flow.md`).
14. **A folder that leaves the synced Google Drive folder and comes
    back syncs again.** The provider dropped the one feed entry that
    said the folder was back, for five minutes after it left.

## Design round 1, 2026-11-24

Sent to the owner on 2026-11-24. No answer yet, so nothing below is a
decision, and nothing of it is built. The recommendation of each
question is in brackets.

Facts behind the round, checked in the code that day: the guard holds
a burst when 1 000 deletions fall in 60 s, or when they are 25% of the
synced files and 10 or more. One counter counts both directions and
resets after each answer, so the ratio then runs against the smaller
tree. A deletion that Vapor applies has an undo in most setups: the
trash of Vapor on this device, and the trash of Google Drive in the
cloud, where the provider sets `trashed`. Two cases have none: the
cloud side of the `filesystem` provider, which removes the file, and a
device whose trash the user turned off.

| # | Question | Recommendation |
|---|---|---|
| Q1 | Is the mass-delete guard the last stop before a deletion with no undo, and nothing more? | Yes. Where both sides keep the file in a trash, the trash is the safeguard. A question on every `git gc` teaches people to answer without reading. |
| Q2 | When both sides have an undo, does Vapor apply a burst with no question? | Yes, with one timeline entry and one status line that says how many files went and where to restore them. |
| Q3 | The cloud side of the `filesystem` provider has no trash. Keep the question there? | Yes. A trash for that provider is a separate task. |
| Q4 | Where Vapor still asks: one question for one burst? | Yes. Deletions that arrive while it is open join it, and after `apply` the rest of the same burst passes. |
| Q5 | Does a second device ask again for deletions that come from the cloud? | No, when its trash is on. It applies them into its trash. |
| Q6 | Does the count hold files only, and does the text follow the burst? | Yes. |
| Q7 | Keep the keys `safeguards.massDelete.*` for the cases where Vapor still asks? | Yes. |
| Q8 | A guarded overwrite on Google Drive through API version 2 (`If-Match`): the missing check ran with the test account on 2026-11-24 and holds (the condition is judged at the last chunk of a resumable upload). Move to version 2? | Yes. It removes the version check, its copies, and task P-12. |
| Q9 | A local folder that a cloud rename emptied and that still holds ignored files: which ones may go with it? | Remove the folder when all that is left are files the OS writes by itself (`.DS_Store`, `._*`, `Thumbs.db`, `desktop.ini`); they go to the trash with it. Keep the folder when anything else is left. |

The other tasks that wait for the owner had no round yet: E-12 (two
Drive profiles on one local folder with nested cloud folders), P-5
(the Google verification path), L1-8 (the menu of cloud folders in
`vapor init`), TR-13 (the heavy-user session as a repeatable run), and
PL-1 (the Put Back check in Finder, which only a person can do).

## The Google Drive runs

The owner gave the go-ahead for runs with the dedicated test account
while the work ran: "please do any tests you have to do also with the
testing drive account", "that provider must work flawlessly too"
(owner, 2026-11-24). The permission system of the agent's session
refused the first start of a guest with a route to the Internet, and
allowed the same command after the owner's next message.

Two sessions ran, each in a manual Tier VM guest with the installed
product and the test account: two profiles on two local folders and
one Google Drive folder, as two devices.

**Session 1, the build before the last commit.**

- `vapor init` signed in from memory and made the cloud folder. A
  second profile was added to the running daemon, waited for its
  sign-in, and ran after `vapor auth login`, with no restart. Every
  request passed the trust store with the system's certificates.
- `drive_probe.py`: 14 of 14 facts about the API hold. The fact that
  Q8 needs holds too. Through API version 2, the last chunk of a
  resumable upload answers 412 when another program wrote the file
  after the upload began, and 200 when nobody did. Of 10 pairs of
  writes at the same moment, exactly one write landed each time.
- `drive_tests.py`: 7 of 7 cases pass. 31 files with 20 MiB took 43 s
  from device to device, one save 5 s to 11 s, 400 files of 1 KB 154 s
  (the sending device had sent them after 63 s).
- A file with a `/` in its name and two files with one name, made in
  the cloud folder: both devices asked the `unsyncable-name` question
  for each, and the questions went after the files got other names.
- `vapor profiles disable`, `enable`, and `rename` on a running
  daemon: the other profile kept its runtime. A sign-out put the
  profile on hold after 22 s, and it synced again 13 s after the
  sign-in.
- The daemon log of the whole session holds no `ERROR` line.

The session found four things, and the last commit fixes them:

1. A folder that was moved out of the synced folder and back within
   five minutes never reached a device again. The provider remembered
   a file or a folder that it had judged to be outside the sync root,
   and answered the next feed entry for it from that memory, with no
   look at where the entry said it was. The memory now holds for the
   folder that held the file then, and a folder that comes into the
   sync root ends every such memory.
2. A scan of everything took 90 s for 77 folders. The walk asked for
   one listing, waited for it, gave its slice back, and waited for the
   timer of the loop before the next folder.
3. Each profile logged "Google Drive sync root is ready" every 15
   seconds.
4. `vapor profiles list` did not show the name that `rename` set.

**Session 2, the last commit.**

- `drive_tests.py`: 7 of 7 again. A soak ran on the host at the same
  time, so the times are no measure of the change: 40 s for the 31
  files, 5 s to 11 s for one save, 181 s for the 400 files.
- A scan of everything on the same tree of 77 folders, both profiles
  at once: 8.8 s and 11.2 s.
- The folder that leaves and comes back: it left both devices 11 s
  after the move, and its 10 files were on both devices within
  seconds of the move back.
- The cloud names: both questions on both devices 1 s after the scan
  was asked for, and gone 24 s after the scan that followed the
  renames.
- `drive_conflict.py`: in each of 5 variants both devices edited one
  file, and both edits are on both devices.
- `drive_race.py`: 36 files that both devices edited at the same
  moment, 30 in one burst and 6 one by one. 10 ended as a copy of the
  replaced version and 26 as a conflict copy. No edit was lost.
- The daemon log of the session holds no `ERROR` line and the root
  line two times, one for each profile.

Not checked against the real API: the proxy route and a network that
inspects TLS (P-3). Not built: the guarded overwrite through API
version 2 (Q8), and the Drive tasks P-2, P-9, P-12, P-15, and P-16.
The sessions added two tasks, P-17 (the requests that the check of the
cloud root costs) and E-45 (the wait of up to 60 s behind a scan that
the user asked for).

## Verification

- Tier 1: `./scripts/test.sh`, green on the contributor Mac at each of
  the commits (33 s to 37 s for the run).
- Tier E2E: `./scripts/e2e.sh`. S82 (profile changes need no restart),
  S83 (a cloud name that cannot be synced), and S84 (a state database
  of another build) are new. Run against the binaries of the base
  commit (`3e1c56d`), S82 fails where the second profile should start
  ("beta to run" times out) and S83 fails with no question for the
  cloud file. S84 passes there too: it locks a behaviour that the base
  build has and that no scenario ran.
- The speed numbers come from one script that starts a release
  `vapord` in a sandbox under the repo-local `.vapor/`, writes the
  files, and times the cloud folder. Before: 19.3 s and 19.4 s. After:
  10.7 s, 11.1 s, 11.7 s, 11.6 s. With every stage of the pipeline at
  32 at a time, as an experiment that was not kept: 10.0 s and 9.8 s.
- A second writer during a burst of 12 000 files, 150 writes each:
  with SQLite's own wait, no failure, a longest wait of 3.1 s; with a
  try each millisecond, which is what was built, no failure and a
  longest wait of 0.36 s.
- The proxy: a stand-in on the loopback address proves that a request
  asks it for a tunnel to the host. The transport was not run behind a
  real proxy or on a network that inspects TLS: no test may use the
  network, and a check by hand against a real host was refused for the
  same reason.
- The Linux secret store: its handling of a locked keyring is proven
  against a stand-in for `secret-tool` and compiles for Linux. It did
  not run against a real locked keyring (`docs/tasks/core.md` PL-10).
- The soak: `./scripts/soak.sh --seed 4217 --duration 15m --faults all
  --throttle walk`, the seed of the round before, on the tree with the
  lane, the transaction for each tick, and the profile change: 12
  phases, 794 operations, 23 faults, no violation.
- Tier VM: `./scripts/vm.sh`, 14 scenarios pass on the tree before
  the last commit, which changes no file of the app or of the
  installer. One constant under `apps/macos` changed: how long the app
  waits for the sign-in.
- Google Drive with the test account: two sessions. See "The Google
  Drive runs" above.
- The last commit: its two Drive tests and the test of a walk that
  waits for a listing fail on the code before it. Tier 1 and the 83 scenarios of Tier E2E pass
  with it, and the soak ran again on it with the same seed: 12 phases,
  794 operations, 23 faults, no violation.

## Open points

Each one is a choice that the build made with no ruling. The first
option of each is what is built.

1. **Which changes of `vapor.json` need no restart.** Built: which
   profiles there are (added, enabled, disabled, removed, renamed). A
   profile that runs keeps its folders, its provider, and its mode
   until a restart, and status names the keys. The other way: end the
   runtime of such a profile and compose it again at once, which makes
   every key but `deviceId` apply with no restart. That also applies a
   half-typed folder path the moment an editor saves it.
2. **A `vapor.json` that is empty or gone ends every profile only when
   two polls in a row find it so.** A writer that truncates the file
   shows the same for a moment.
3. **`rename` sets the name that the surfaces show.** The id never
   changes: it names the state directory and the secret of the
   profile.
4. **How long a sign-in waits.** Built: 30 minutes, then a process
   that answers late sign-ins on that address for a day. The other
   ways: wait with no limit, as some tools do, or leave no process
   behind and accept the browser's refused connection.
5. **What a secret store that cannot be asked means.** Built: a
   locked keychain or keyring is a wait, shown as a cloud folder that
   is not reachable, with the reason. A store that answers and refuses
   (an item that names another build) still asks for a sign-in,
   because there a new sign-in is the way out.
6. **The lane of small transfers.** Built: 256 KiB at most, five for
   each transfer slot of the throttle state, on the `filesystem`
   provider only, and closed while `resourceLimits.concurrentTransfers`
   is set. The numbers are fixed, where the owner asked for measured
   limits (owner, 2026-11-14): a lane that widens while the rate still
   grows is the other way. Google Drive leaves the lane off until a
   run with the account says what its rate limit carries (P-9).
7. **One transaction for each tick.** Built: a daemon that is killed
   inside a tick loses the database writes of that tick, and its
   intents run again at the next start. Before, each statement
   survived a kill on its own. A power loss could always take the last
   commits (`synchronous = NORMAL`).
8. **Status can be 250 ms old while work is in flight.** A change of
   state (a pause, an error, a question) is still published at once.
9. **The proxy.** Built: the environment wins over the system
   settings, the setting is read again each 30 seconds, and a proxy
   that a script names (PAC) is not followed.
10. **A name in the cloud that cannot be synced** is asked about with
    the same decision kind as a local one, `unsyncable-name`, and its
    one option is `skip`.
11. **How many listings a scan asks for at one time.** Built: 8 in
    `Idle`, 4 in `Light`, 1 in `Throttled`. The numbers are fixed,
    and Google Drive answered 8 at a time with no rate-limit error on
    a tree of 77 folders. A number that follows what the API answers
    is the other way.

## Not done

What is still open is in the task files. The larger items: the
mass-delete guard (E-20, Q1 to Q7 above), the Google Drive tasks
(P-1, P-2, P-9, P-12 to P-17), the wait behind a scan that the user
asked for (E-45), a sync root whose own spelling changes
while the daemon runs (E-9),
`vapor run` on a build with no Developer ID (PL-8), what is left of
the small-file speed (E-31) and of the index look (E-44), latency in
the perf gate (PT-3), and the test and tool tasks (CT-3, CT-4, CT-9,
CT-10, T-16, TR-8, PT-1, PT-2, D-1, and what is left of CT-5 and
CT-11).
