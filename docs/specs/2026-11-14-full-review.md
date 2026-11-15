# Full review of the repository, and what it changed

Status: implemented 2026-11-15

## Goal

The project owner asked for a full review of Vapor before the work on
the Mac app starts: every bug, inefficiency, contradiction between a
document and the code, and improvement that a complete read of the
repository finds. The owner then decided each finding that had more
than one reasonable answer, and asked for all of it to be built in one
pull request. In the owner's words: "we must always keep syncing in the
device, we cannot stop syncing, we are a background cloud sync".

What a user gets: a sync that does not lose a file in the cases the
review found, that keeps going on battery and while the device is in
use, that asks before a one-way mode runs, and that creates nothing
before the user chose what to sync.

## Facts

What was true when the design started (`main` at `64001ad`, version
`0.2.0-alpha.3`):

- The review read every crate, script, workflow, and document. It
  listed 95 findings: 2 that lose data in normal use, 8 that break a
  guarantee of `AGENTS.md` or show wrong behaviour in a normal case, 24
  wrong in an edge case or wasteful, 53 inconsistencies, and 8 ideas.
  Two more were found while the fixes were built.
- Most findings came from reading. Some were run with temporary probe
  tests, and four with the real binaries in the e2e manual sandbox.
  Nothing ran against `~/.vapor`, a LaunchAgent, a packaged app, the
  network, or Google Drive.
- `./scripts/lint.sh` and `./scripts/test.sh` were green on the base
  commit. No finding came from a red check.
- Three root causes were behind the data-loss findings:
  1. The planner of a local deletion did not check that the file was
     still absent, and deleted with no comparison when the sync index
     had no row.
  2. On macOS the daemon did not recognise its own renames and trash
     moves as echoes.
  3. A modification time was used as evidence of order.
- Two gaps let those pass every tier: no test delivered the events
  that the daemon's own writes cause, and no whole-scope reconcile ran
  between two starts to repair a missed change.
- Invariants touched: `AGENTS.md` §3 (throttle), §4 (bidirectional
  safety), §5 (durability and migrations), §6 (secrets), §7 (release),
  §9 (tests). Where a decision below changes a rule, the same change
  set changed the section.

The findings that the decisions name:

| Id | Priority | Finding |
|---|---|---|
| APP-01 | P3 | App findings |
| CI-12 | Idea | No dependency check in any gate |
| CI-13 | Idea | The release ships one CPU architecture and no document says so |
| CI-14 | P1 | The next release fails at the publish step |
| DOC-07 | P3 | Em dashes in every document class except the agent files |
| DOC-12 | Idea | Record the decisions that already exist as specs |
| ENG-01 | P1 | On battery or while the user types, a folder that had a burst stops syncing |
| ENG-02 | P1 | Low Power Mode stops all sync, with no way out |
| ENG-03 | P2 | Every start on battery or with an active user syncs nothing for 60 s |
| ENG-05 | P2 | Release builds have no panic containment |
| ENG-06 | P2 | A profile suspended at runtime never comes back |
| ENG-07 | P2 | A broken `vapor.json` makes the daemon run the default profile |
| ENG-08 | P2 | The CPU ceiling is not a measured limit |
| ENG-09 | P2 | The bandwidth ceiling is a share of an assumed 100 Mbps |
| ENG-15 | P1 | A file put back with an old modification time is deleted again |
| ENG-16 | P2 | A local deletion with no index row deletes the cloud object unseen |
| ENG-19 | P3 | The state database has no integrity check and keeps three migrations |
| ENG-21 | P2 | Five changed text files in a minute count as a user at the keyboard |
| ENG-22 | P3 | The auto-tuner changes a value that no longer sets the pace |
| ENG-23 | P3 | An ignore-rule change does not sync the files it frees |
| ENG-24 | P2 | No whole-scope reconcile runs between two starts |
| ENG-28 | P1 | A transfer that waits for bandwidth spins the tick loop on a full core |
| ENG-29 | P0 | A case-only rename removes the file from the cloud, and can remove it from both sides |
| PLT-01 | P1 | No surface warns before a one-way mode is enabled |
| PLT-07 | P3 | The support report is called redacted and holds every file path |
| PRV-01 | P1 | After a Drive upload over 5 MiB, the next edit of that file cannot upload |
| PRV-07 | P3 | A name that the path model refuses gets no decision |
| SHR-03 | P3 | Two devices with the same host name get the same device id |
| SHR-06 | Idea | The default ignore list hides user data without notice |

Priorities: P0 loses user data in normal use, or leaks a secret. P1
breaks a guarantee of `AGENTS.md`, or a user sees wrong behaviour in a
normal case. P2 is wrong in an edge case, a real inefficiency, or a
check that does not check. P3 is an inconsistency. An idea is an
improvement where nothing is wrong.

## Decisions

Each decision is the owner's (owner, 2026-11-14) unless its row says
otherwise. The notes name tasks by the ids of that day; the task files
hold open work only since this change, so some of those ids are gone.

### Round 1, 2026-11-14: what happens to files

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D1 | On a volume that folds case, a change in only the spelling of a name is a rename. Vapor carries it to the other side as a move. It makes no conflict copy and asks no question. (ENG-29) |
| D2 | When both sides changed the spelling in different ways before a sync, the cloud's spelling stays and the local name follows it. (ENG-29) |
| D3 | A local deletion of a file that has no row in the sync index keeps the cloud object and downloads it again. Vapor never deletes a cloud object that it did not compare. (ENG-16) |
| D4 | A modification time never decides that a deletion wins. Both tombstone-against-mtime rules go, and the sync index is the only evidence. The tombstone table goes too when no other code reads it. Accepted cost: a device that lost its state database can bring a deleted file back. (ENG-15) |
| D5 | With no usable configuration (no `vapor.json`, a file that does not parse, or no valid profile) the daemon syncs nothing and creates no folder. It stays up, and `vapor status` says what to fix. A profile exists only when the user set its roots. (ENG-07, APP-01) |
| D6 | The daemon holds a one-way profile until the data-loss warning is acknowledged for that profile, that mode, and that pair of roots. The warning is a decision in `vapor decisions`, so the CLI and the app show the same question, and a hand-edited file cannot enable the mode silently. (PLT-01) |

Notes on round 1:

- D5 confirms a task that is already open: `docs/tasks/core.md` OB-1
  ("an install with no sync scope composes no profile, creates no
  folder"), with the owner's intent of 2026-09-10 in
  `docs/tasks/macos.md`, phase M6. The review did not link ENG-07 to
  that task.
- The owner added to D5: the apps get an onboarding flow, the CLI gets
  a command that does the same, and the apps run that command
  underneath. The task lists already plan this as
  `vapor profiles list|add|remove` (`docs/tasks/cli.md` L1-6). The name
  of the command was open at that point; D7 settled it.
- D6 fits the confirmation that `docs/tasks/macos.md` M4-5 plans for
  the app.

### Round 2, 2026-11-14: device impact

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D7 | One command sets up a profile from a terminal and runs underneath the apps: `vapor profiles add`, as `docs/tasks/cli.md` L1-6 plans. With no flags on a terminal it asks the questions. The first `add` is the setup. There is no `vapor init`. (D5) |
| D8 | A reconcile scan starts in `Light` and in `Throttled` too, for each kind of scan, at the caps of that state, and it stays interruptible. (ENG-01, ENG-03) |
| D9 | Vapor does not stop syncing for a condition that can last: a device on battery, an active user, Low Power Mode. A throttle state makes sync slower. It does not stop it. The owner's words: "we must always keep syncing in the device, we cannot stop syncing, we are a background cloud sync". The wording of this row is the agent's. Which conditions may still stop all sync was open at that point; D19 settled it. |
| D10 | Low Power Mode gives `Throttled`. (ENG-02) |
| D11 | The active-coding heuristic is removed. (ENG-21) |
| D12 | A whole-scope reconcile runs one time each 24 h, when the policy admits a scan. A trigger after a long sleep can come later. (ENG-24) |
| D13 | The CPU ceiling is a measured limit. While the daemon's own CPU is above `cpuPercent`, concurrency goes down step by step. It goes up again below the ceiling. (ENG-08) |
| D14 | The bandwidth ceiling stays a share of the link capacity, and Vapor measures that capacity. It never assumes a number. More capacity means a higher rate. The owner's words: "we must measure it", "we must be super efficient", "if we have more network capacity then we sync more". The agent recommended an absolute rate; the owner rejected it. How the capacity is measured was open at that point; D18 settled it. (ENG-09) |
| D15 | Release builds use `panic = "unwind"`, so a panic suspends one profile and the others keep syncing. (ENG-05) |
| D16 | A profile that the runtime suspended is tried again after 1, 5, and 30 minutes, then each 30 minutes. (ENG-06) |
| D17 | The auto-tuner and the shared transfer step are deleted. Each provider owns its chunk size. (ENG-22) |

Notes on round 2:

- D8, D9, and D10 change rules in `AGENTS.md` §3 ("Reconcile scans
  ... run only when policy permits", the meaning of `Suspended`). The
  change set that builds them updates that section.
- D13 and D14 make "ceiling" mean a measured limit for CPU and for
  bandwidth. ENG-28 (the busy loop) is a plain fix and comes first.

### Round 3, 2026-11-14: measurement, defaults, and surfaces

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D18 | Vapor learns the link capacity from its own transfers: the highest rate that each direction reached in the last minutes, with a short probe without the limiter when the estimate is old. Before the first measurement, the estimate starts from the interface speed that the OS reports. The measures then correct it. The start from the OS value is the owner's addition. (D14, ENG-09) |
| D19 | Only critical thermal pressure stops all sync (`Suspended`). High system CPU and high CPU of the daemon give `Throttled`. Severe disk pressure holds downloads only. (D9) |
| D20 | The default ignore list stays as it is. `vapor status` shows a count of ignored paths, and a command lists them. The owner chose this over a shorter list, which the agent recommended. (SHR-06) |
| D21 | `useGitignore` stays `true` by default. (SHR-06) |
| D22 | A negation under an ignored folder follows git's rule in the watcher and in the walk: a file under an excluded folder cannot be included again. (ENG-23) |
| D23 | `vapor report` redacts paths by default. A flag keeps them. (PLT-07) |
| D24 | Before GA the state database has no migrations. A schema mismatch quarantines the database, and a whole-scope reconcile rebuilds it. The v3 to v6 chain is deleted. `AGENTS.md` §5 says that migrations start at GA. (ENG-19) |
| D25 | The device id is the host slug plus four random characters, saved at the first run. (SHR-03) |

Notes on round 3:

- D18: the interface speed is an upper bound. On a fast LAN behind a
  slow uplink, the first transfer runs below the limiter and is the
  first measure. The value needs a new method on a `core/platform`
  trait. The macOS API is not chosen yet.
- D20: two plain fixes of ENG-23 stay: a reconcile after a rule
  change, and a folder rule that no longer hides a file of that name.
- D23, the owner's question: does redaction make debugging harder for
  an agent? The agent's answer:
  - No change on the device that has the failure. The logs and the
    state database on disk are not redacted. `vapor-debug`, the e2e
    sandbox, and the soak read those. Only the copy inside a report is
    redacted.
  - A redacted report from another user hides the names. Some bugs
    depend on the name (ENG-29 on case, PRV-07 on a backslash, the
    ignore rules). The redaction must therefore keep the shape of each
    name: one stable token for each path component, a second token for
    its case-folded form, the extension, Vapor's own markers
    (`~conflict-`, `.vapor-tmp-`), and flags for length, case pattern,
    non-ASCII characters, and special characters. This design note is
    the agent's.

### Round 4, 2026-11-14: process and order

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D26 | The release workflow cuts the release body at the GitHub limit and links to `CHANGELOG.md`. The `Unreleased` section is rewritten now as a short list of changes that a user can notice. From now on the changelog gets one line for each PR, and `AGENTS.md` §10 says so. (CI-14) |
| D27 | The em dashes in the documents and the code comments are removed in one mechanical sweep. (DOC-07) |
| D28 | A past decision gets a spec only when its area is next changed. No backfill. (DOC-12) |
| D29 | the review file is a temporary document. It is deleted when the work is done. |
| D30 | `cargo deny` runs in `scripts/lint.sh` as a cargo subcommand, with no new GitHub Action. (CI-12) |
| D31 | The Mac app ships for Apple silicon only for now. The README and the release notes say so in one short line. Other architectures and platforms come when users ask for them; the portable core keeps that door open. (CI-13) Replaced by D33. |
| D32 | The whole review is built in a single PR. The agent chooses the order of the work inside it. The owner gives more instructions about this PR before the decisions close; D36 to D38 are those instructions. |

Notes on round 4:

- D29 and `AGENTS.md` §1.2 together: the PR that builds the review
  moves each decision into this spec and then deletes the review file.
- D31: the code is already portable. The Rust core builds on Linux and
  Windows in CI, and an Intel Mac needs only a second build target in
  the release workflow. CI-13 is about the release and the documents,
  not about the code.

### Round 5, 2026-11-14: clarifications

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D33 | D33 replaces D31. "Apple silicon only" is about the Mac: the Mac app is built for Apple silicon, and Vapor gets no Intel Mac build. Windows and Linux stay planned surfaces (`AGENTS.md` §1). The README and the release notes say this in one short line. (CI-13) |
| D34 | The fix for PRV-01 is built from the Google Drive API documents. The check against a real Drive account stays with the owner and does not gate the PR. |
| D35 | The PR does not wait for a manual check of the Mac app. The agent runs each check that an agent may run. The owner tests the app later, when the work on the Mac app starts. |

Notes on round 5:

- D35 does not change `AGENTS.md` §7.1 and §9.3: an agent never
  launches a packaged app and never verifies UI. The PR description
  still carries the short manual checklist, so the owner has it for
  later.

### Round 6, 2026-11-14: the single PR

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D36 | The work is on one branch. the review file is committed first, as it is. The agent makes as many commits as it wants, and it strikes a finding through in the review file when the finding is built. |
| D37 | The PR opens when all the work is committed and pushed. the review file is still in the tree then, so the adversarial review can read it. The adversarial review is large, because the change is large. The agent watches the PR until CI is green. |
| D38 | When the PR is ready to merge, the agent deletes the review file and pushes one last time. D38 sets when D29 happens. |

Notes on round 6:

- The owner asked for the push and the PR in the same message, so
  `AGENTS.md` §10 is met for this branch.
- The owner is not available during the build. A choice that comes up
  in the build and has more than one reasonable answer is recorded in
  the spec as the agent's choice, marked as open for the owner.

### Round 7, 2026-11-15: the choices the build made

The owner read the open points before the merge and accepted each
recommendation. Each decision below is the owner's
(owner, 2026-11-15).

| # | Decision |
|---|---|
| D39 | With no terminal, `vapor profiles add` refuses unless both `--local` and `--cloud` are given. A suggestion that nobody saw is no choice (D5). On a terminal the command asks, and offers `~/Vapor` and `/Vapor`. The build took the suggestions before; D39 changes that. |
| D40 | The LaunchAgent keeps `ProcessType: Background`. Hashing is about 2.7 times slower under that policy, and device impact comes first. The work on the Mac app measures real sync speed, and the owner decides again then. |
| D41 | `AGENTS.md` §3 names two bounded exceptions to "no provider I/O and no hashing on the tick thread": a file of up to 8 MiB is hashed in line, and the re-baseline of an expired changes cursor polls in line. Removing them is the first task after the merge (`docs/tasks/core.md` E-6). |
| D42 | A daemon whose profiles are all suspended stays up. It retries them on the schedule of D16 and answers `vapor status`. It does not exit. |
| D43 | The run state for "no usable profile" is named `unconfigured`. It is part of the `--json` contract. |
| D44 | Neither root has a default. A `localRoot` with no `cloudRoot` is no profile. |
| D45 | On a terminal, `vapor profiles add` asks for each option that is left out, and for the id when a profile exists. It refuses a `vapor.json` that the daemon refuses, and so does `vapor profiles remove`. |
| D46 | `vapor profiles remove` forgets the sync state and the sign-in, and keeps the profile's trash. The daemon deletes each trashed file when its retention time ends. |
| D47 | The one-way confirmation lives in the state database, so a database that is set aside and rebuilt asks again. The confirmation also ends when the profile runs `two-way`: a return to the same mirror asks again. |
| D48 | An `apply` answer to a deletion burst lets the held deletions pass the guard. It does not stand in for the sync index: a cloud file with no index row stays (D3). Only an answer that names the path itself deletes without a row. |
| D49 | The reconcile walk removes the sync index rows of names that left both folders. |
| D50 | A redacted report can keep a name that is one plain lowercase word, when the name stands alone in a sentence. The report says so. |
| D51 | `vapor service install` and `bootstrap` restart a daemon of another build. A daemon of the same build is never restarted. |
| D52 | `./scripts/lint.sh` needs `cargo deny` and a network connection. A missing tool fails lint with the install command. |
| D53 | `version.sh` refuses instead of guessing: `bump` on a prerelease and `prerelease` on a stable version stop with the command to use. |
| D54 | The CLI gets no Intel Mac build either (D33). |
| D55 | The short `Unreleased` list of the changelog stands as the agent wrote it (D26). |
| D56 | Linux and Windows read no interface speed for now. It is not a design choice: neither is a shipping surface yet, and the figure is only the start of the bandwidth ceiling. There the first transfer step of each direction runs with no limit, and the ceiling holds from the first measurement on. The owner accepts this on the condition that the work has its tasks: `docs/tasks/core.md` PL-3 (Linux) and C6-4 (Windows). |

## Design

How the decisions were built. The system documents describe the result
in full; this section says where to look.

- **Deletions (D3, D4).** The delete planner needs a sync index row and
  an unchanged survivor. With no row it keeps the cloud object and
  downloads it. A file that is back when the deletion is planned drops
  the deletion. The tombstone table and both modification-time rules
  are gone. `docs/architecture/data-flow.md` §Local to remote.
- **Spelling (D1, D2).** Sync state is keyed by the spelling the volume
  stores. The reconcile walk settles a pair that differs in spelling
  only, and the upload and download planners carry it as a move.
  `data-flow.md` §Directory and symlink semantics.
- **The daemon's own events.** The echo check matches a delete record
  by the state of the path, and a write by size and modification time.
  The runtime fixture has a stand-in watcher that reports what a real
  watcher reports after the daemon's writes.
- **Throttle (D8 to D11, D13, D19).** Every state but `Suspended`
  admits a scan and releases a compacted folder. Only critical thermal
  pressure gives `Suspended`. The CPU ceiling takes workers away while
  the measured CPU is above it. `data-flow.md` §Local to remote and
  §User resource budgets, and `AGENTS.md` §3.
- **Bandwidth (D14, D18).** `core/daemon/src/bandwidth.rs` measures the
  capacity of each direction from the transfers and sets one token
  bucket for each. The first estimate is the interface speed
  (`ThrottleInputs::link_speed_kbps`, read on macOS).
- **Reconcile (D12).** One whole-scope scan each 24 hours.
- **No configuration, no folder (D5, D7).** The daemon composes no
  profile without two roots, stays up, and reports `unconfigured`.
  `vapor profiles list|add|remove` is the setup. `data-flow.md` §What
  stops a whole profile.
- **One-way warning (D6).** A `one-way-mode` decision holds the
  profile. `docs/architecture/sync-modes.md` §Safety.
- **Failure containment (D15, D16).** Release builds unwind, one
  profile's panic suspends that profile, and the runtime tries a
  suspended profile again after 1, 5, and 30 minutes.
- **Chunk size (D17).** The auto-tuner is deleted. A transfer session
  names its own step size.
- **Ignore rules (D20 to D22).** Git's rule for a negation under an
  excluded folder, on the watcher and on the walk. `vapor status`
  counts the ignored paths and `vapor ignored` lists them.
- **Report (D23).** `vapor report` replaces names by stable codes that
  keep the shape of a name. `--keep-paths` keeps them.
- **State database (D24).** Schema 7, no migrations. A database of
  another version is set aside and rebuilt by a reconcile.
  `docs/architecture/state-schema-migrations.md`.
- **Device id (D25).** Host slug plus four random characters.
- **Release (D26, D30, D33).** `scripts/notes.sh` builds the release
  body and cuts it at GitHub's limit. `cargo deny` runs in lint. The
  README and the release body say that the Mac app is built for Apple
  silicon.
- **Documents (D27, D28).** One mechanical sweep removed every em
  dash, and lint keeps the count at zero. No spec was written for an
  older decision.

Found while the fixes were built, and fixed in the same change:

- A file that looked deleted because the local root was swapped could
  be removed from the cloud inside the 15 seconds between two root
  checks. The executor now reads the root marker before any lease and
  before each cloud deletion.
- In `push-only`, a spelling that the cloud changed was not put back
  until the next whole-scope scan, and a later upload of that file
  waited for it. The feed mapping now asks for a reconcile of the
  folder, and the filesystem feed reports the spelling the folder
  stores. The new soak operation found it.
- The first run of `cargo deny` found advisories in rustls,
  rustls-webpki, webpki-roots, and the SQLite that rusqlite bundles.
  All four were updated.

## Verification

- Tier 1 (`./scripts/test.sh`): green on macOS, and in a Linux
  container through the wrapper scripts.
- Tier E2E (`./scripts/e2e.sh`): twelve new scenarios, S56 to S67. Each
  one failed without its change and passes with it: a case-only rename
  on each side, a removal that is undone, CPU use while a transfer
  waits for bandwidth, a conflict with a slow download, a fresh install
  with no scope, the one-way question, the profiles command, the
  ignore rules, the report, the ignored list, and the push-only
  spelling. S51 now covers sync under battery, an active user, and Low
  Power Mode at once.
- Tier S: the soak workload gained a rename that changes only the case
  and a removal that is undone. Three five-minute cells ran with them
  (`two-way`, `pull-only`, `push-only`): no violation after the
  push-only fix. The cells ran again after the fixes of the pull
  request review, and the push-only cell found one more defect: the
  filesystem provider failed an intent for good when an entry went
  away while its path was resolved (a rename that changes only the
  spelling was under way). The lookup now looks again, and an entry
  that stays away is a failure to try again. The release gate runs the
  long cells.
- Not verified, and why:
  - The Google Drive changes were built from the API documents (D34).
    No run used a real account.
  - The launchd changes (an install that changes nothing when nothing
    changed, a start that never kills, a stop that waits) run only in
    the `--full` e2e phase on a CI runner.
  - The notarization path of `package.sh` cannot run: no signing
    material exists yet.
  - The Mac app was not launched (D35). The pull request carries the
    manual checklist.
- The adversarial review of the pull request (D37): six reviewers, one
  for each area (engine data safety; runtime, profiles, and trash;
  providers and platform; CLI, IPC, and app; scripts, CI, and tools;
  documents). They reported 68 findings and no data loss in normal
  use. 63 were fixed, each code fix with a test that fails without it.
  One did not reproduce in an experiment (a state database set aside
  under an open connection keeps its write-ahead log). Two became
  tasks (`docs/tasks/core.md` E-7, E-8). Two were declined: a
  `lifecycle.json` written by an older build reads as defaults (no
  build before GA has users), and a count in `REVIEW.md`, which is
  removed with the merge. The pull request lists each finding with its
  verdict.

## Open points

Round 7 settled the choices that the build made (D39 to D56). What is
still open:

1. **Two profiles whose local folders overlap are not refused.** One
   profile whose cloud folder is inside its local folder is. What an
   overlap between profiles should mean is not decided
   (`docs/tasks/core.md` E-3).
2. **The cloud-side window of a swapped root is open for 15 seconds.**
   A local file that a cloud deletion removes in that window goes to
   the trash (`docs/tasks/core.md` E-2).
3. **The Google verification path for the Drive scope.** It gates
   general availability of the Drive provider
   (`docs/tasks/core.md` P-5).

Not built, with the task that tracks each one:

| What | Where it stands | Task |
|---|---|---|
| A local name for the second of two Drive files with one name | One of the two syncs, and a warning names the other | `docs/tasks/core.md` P-1 |
| The Drive file id in the sync index | A cloud deletion made while the daemon was down is found by the startup scan | `docs/tasks/core.md` P-2 |
| Hashing of small files and the cursor re-baseline off the tick thread | Both run in line, bounded | `docs/tasks/core.md` E-6 |
| A question for two local names that the cloud folder folds together | The second upload waits and is tried again | `docs/tasks/core.md` E-7 |
| The checks of a profile before its local root is made | An invalid profile is suspended after its folder is made | `docs/tasks/core.md` E-8 |
| A sync root whose own spelling changes while the daemon runs | Names below the root compare right; the watcher path is not checked | `docs/tasks/core.md` E-9 |
| A hold on the events of a folder while its new ignore file loads | A folder that moves in whole waits; files written one by one do not | `docs/tasks/core.md` E-10 |
| The bandwidth charge of a Drive chunk below 256 KiB | The excess is not taken from the bucket | `docs/tasks/core.md` P-6 |
| Proxy settings and system root certificates in the HTTP transport | A network that inspects TLS cannot sync | `docs/tasks/core.md` P-3 |
| A live check of the Drive changes | Built from the API documents | `docs/tasks/core.md` P-4 |
| The Google verification decision | The client can serve 100 users | `docs/tasks/core.md` P-5 |
| The first ignore-rule build on a worker | A rule change rebuilds on a worker; the first build does not | `docs/tasks/core.md` E-1 |
| A profile argument for pause, resume, and reconcile | Each one acts on every profile | `docs/tasks/core.md` E-5 |
| The macOS system trash through `NSFileManager` | A rename into `~/.Trash`, with no Put Back | `docs/tasks/core.md` PL-1 |
| A launchd job state that does not come from `launchctl print` text | The installer matches that text | `docs/tasks/core.md` PL-2 |
| The first-run flow of the Mac app, its sync mode control, and its ignored-paths view | The runtime and the CLI side are in | `docs/tasks/macos.md` O-1 to O-4, M4-2, M4-5, M3-14 |
