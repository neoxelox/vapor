# Vapor full review

Date: 2026-11-14. Base: `main` at `64001ad`, version `0.2.0-alpha.3`.

This file lists what a full read of the repository found: bugs,
inefficiencies, contradictions between documents and code, and
improvements. It changes no code. Each finding is a task candidate.

## How to read this file

- Each finding has an id, a priority, a location, the problem, and a fix.
- The id prefix names the area (`CI`, `DOC`, `ENG`, and so on). Ids are
  stable. Refer to a finding by its id.
- "Verified" says how the finding was checked. "Read only" means the
  code was read and the behaviour was not run.

Priorities:

| Priority | Meaning |
|---|---|
| P0 | Loses user data, or removes it from a synced folder, in normal use. Or leaks a secret. |
| P1 | Breaks a guarantee from `AGENTS.md` (data loss in an edge case included), or a user sees wrong behaviour in a normal case. |
| P2 | Wrong behaviour in an edge case, a real inefficiency, or a check that does not check. |
| P3 | Inconsistency, stale text, dead code, cleanup. |
| Idea | An improvement. Nothing is wrong today. |

## Baseline

`./scripts/lint.sh` and `./scripts/test.sh` are green on the base commit
(macOS, warm build cache). Tier 1 took 41 s. No finding below comes from
a red check.

How findings were checked:

- Most findings are from reading the code and the documents.
- Some were run with temporary probe tests (the runtime fixture, the
  real watcher, the lifecycle manager). Each probe was deleted after
  its run. The tree holds no probe.
- ENG-26, ENG-27, ENG-28, and ENG-29 were also reproduced with the real
  binaries in the e2e manual sandbox (`./scripts/e2e.sh --sandbox`,
  filesystem provider, repo-local `.vapor/e2e/`). Each sandbox was
  stopped and removed.
- Nothing ran against `~/.vapor`, a LaunchAgent, a packaged app, the
  network, or Google Drive.

## Summary

95 findings: 2 at P0, 8 at P1, 24 at P2, 53 at P3, and 8 ideas.

The findings that matter most, in the order to handle them:

| Id | Priority | Finding | Checked |
|---|---|---|---|
| ENG-29 | P0 | A case-only rename removes the file from the cloud, and can remove it from both sides. | Sandbox |
| ENG-26 | P0 | After a keep-both conflict with a slow download, the daemon deletes the winning version from the cloud. | Probe and sandbox |
| ENG-27 | P1 | A file removed and put back within seconds is deleted from the cloud. | Probe and sandbox |
| ENG-15 | P1 | A file put back with an old modification time is deleted again. | Read only |
| ENG-28 | P1 | A transfer that waits for bandwidth uses a full CPU core. Default settings, filesystem provider. | Sandbox |
| ENG-01 | P1 | On battery or with an active user, a folder that had a burst stops syncing. | Read only |
| ENG-02 | P1 | Low Power Mode stops all sync. | Read only |
| PRV-01 | P1 | After a Drive upload over 5 MiB, the next edit of that file cannot upload. | Read only, needs a live check |
| PLT-01 | P1 | No surface warns before a one-way mode is enabled. | Read only |
| CI-14 | P1 | The next release fails at the publish step. | Measured |

Three root causes are behind ENG-26, ENG-27, and ENG-15:

1. The planner of a local deletion does not check that the file is
   still absent, and deletes with no comparison when the index has no
   row (ENG-16).
2. On macOS the daemon does not recognise its own renames and trash
   moves as echoes (ENG-26, fact 2).
3. A modification time is used as evidence of order (ENG-15).

Two gaps let these pass every tier: no test delivers the events that
the daemon's own writes cause (TST-01), and no whole-scope reconcile
runs between two starts to repair a missed change (ENG-24).

`vapor reconcile` repaired the divergence in ENG-26 and ENG-27 when the
file still existed on this device.

## Decisions

Status: agreed 2026-11-14. Every question is answered (D1 to D38).
The build started on 2026-11-14, on the owner's instruction.

How the build marks progress: a finding that is built has its heading
struck through, and a line under the heading says what was built. A
finding that the build could not finish says what is left.

Some findings have more than one reasonable answer. The owner decides
those, in rounds, and each answer is recorded here as a numbered
decision (D1, D2, ...). A finding with no question below has one
obvious fix: build the fix that the finding gives.

The owner asked to keep the decisions in this file first (owner,
2026-11-14). The PR that builds the review moves them into a spec under
`docs/specs/` (`AGENTS.md` §1.2) and deletes this file (D29).

### Decided

#### Round 1, 2026-11-14: what happens to files

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
  of the command is open: see Q29.
- D6 fits the confirmation that `docs/tasks/macos.md` M4-5 plans for
  the app.

#### Round 2, 2026-11-14: device impact

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D7 | One command sets up a profile from a terminal and runs underneath the apps: `vapor profiles add`, as `docs/tasks/cli.md` L1-6 plans. With no flags on a terminal it asks the questions. The first `add` is the setup. There is no `vapor init`. (D5) |
| D8 | A reconcile scan starts in `Light` and in `Throttled` too, for each kind of scan, at the caps of that state, and it stays interruptible. (ENG-01, ENG-03) |
| D9 | Vapor does not stop syncing for a condition that can last: a device on battery, an active user, Low Power Mode. A throttle state makes sync slower. It does not stop it. The owner's words: "we must always keep syncing in the device, we cannot stop syncing, we are a background cloud sync". The wording of this row is the agent's. Which conditions may still stop all sync is open: see Q31. |
| D10 | Low Power Mode gives `Throttled`. (ENG-02) |
| D11 | The active-coding heuristic is removed. (ENG-21) |
| D12 | A whole-scope reconcile runs one time each 24 h, when the policy admits a scan. A trigger after a long sleep can come later. (ENG-24) |
| D13 | The CPU ceiling is a measured limit. While the daemon's own CPU is above `cpuPercent`, concurrency goes down step by step. It goes up again below the ceiling. (ENG-08) |
| D14 | The bandwidth ceiling stays a share of the link capacity, and Vapor measures that capacity. It never assumes a number. More capacity means a higher rate. The owner's words: "we must measure it", "we must be super efficient", "if we have more network capacity then we sync more". The agent recommended an absolute rate; the owner rejected it. How the capacity is measured is open: see Q30. (ENG-09) |
| D15 | Release builds use `panic = "unwind"`, so a panic suspends one profile and the others keep syncing. (ENG-05) |
| D16 | A profile that the runtime suspended is tried again after 1, 5, and 30 minutes, then each 30 minutes. (ENG-06) |
| D17 | The auto-tuner and the shared transfer step are deleted. Each provider owns its chunk size. (ENG-22) |

Notes on round 2:

- D8, D9, and D10 change rules in `AGENTS.md` §3 ("Reconcile scans
  ... run only when policy permits", the meaning of `Suspended`). The
  change set that builds them updates that section.
- D13 and D14 make "ceiling" mean a measured limit for CPU and for
  bandwidth. ENG-28 (the busy loop) is a plain fix and comes first.

#### Round 3, 2026-11-14: measurement, defaults, and surfaces

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

#### Round 4, 2026-11-14: process and order

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D26 | The release workflow cuts the release body at the GitHub limit and links to `CHANGELOG.md`. The `Unreleased` section is rewritten now as a short list of changes that a user can notice. From now on the changelog gets one line for each PR, and `AGENTS.md` §10 says so. (CI-14) |
| D27 | The em dashes in the documents and the code comments are removed in one mechanical sweep. (DOC-07) |
| D28 | A past decision gets a spec only when its area is next changed. No backfill. (DOC-12) |
| D29 | `REVIEW.md` is a temporary document. It is deleted when the work is done. |
| D30 | `cargo deny` runs in `scripts/lint.sh` as a cargo subcommand, with no new GitHub Action. (CI-12) |
| D31 | The Mac app ships for Apple silicon only for now. The README and the release notes say so in one short line. Other architectures and platforms come when users ask for them; the portable core keeps that door open. (CI-13) Replaced by D33. |
| D32 | The whole review is built in a single PR. The agent chooses the order of the work inside it. The owner gives more instructions about this PR before the decisions close. (Q28) |

Notes on round 4:

- D29 and `AGENTS.md` §1.2 together: the PR that builds the review
  moves each decision into a spec under `docs/specs/` and then deletes this
  file.
- D31: the code is already portable. The Rust core builds on Linux and
  Windows in CI, and an Intel Mac needs only a second build target in
  the release workflow. CI-13 is about the release and the documents,
  not about the code.

#### Round 5, 2026-11-14: clarifications

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

#### Round 6, 2026-11-14: the single PR

Each decision below is the owner's (owner, 2026-11-14).

| # | Decision |
|---|---|
| D36 | The work is on one branch. `REVIEW.md` is committed first, as it is. The agent makes as many commits as it wants, and it strikes a finding through in `REVIEW.md` when the finding is built. |
| D37 | The PR opens when all the work is committed and pushed. `REVIEW.md` is still in the tree then, so the adversarial review can read it. The adversarial review is large, because the change is large. The agent watches the PR until CI is green. |
| D38 | When the PR is ready to merge, the agent deletes `REVIEW.md` and pushes one last time. D38 sets when D29 happens. |

Notes on round 6:

- The owner asked for the push and the PR in the same message, so
  `AGENTS.md` §10 is met for this branch.
- The owner is not available during the build. A choice that comes up
  in the build and has more than one reasonable answer is recorded in
  the spec as the agent's choice, marked as open for the owner.

### Order of the work inside the single PR (the agent's plan, D32)

1. Data safety: ENG-26, ENG-27, ENG-16, ENG-15, ENG-29, ENG-30,
   ENG-20, ENG-17, with the tests of TST-01 that cover them (D1 to D4).
2. Device impact: ENG-28 first, then ENG-01 to ENG-04, ENG-21, ENG-24,
   ENG-08 to ENG-11, ENG-18, ENG-22 (D8 to D19).
3. Fail closed, and surfaces: ENG-07, ENG-12, PLT-01, ENG-05, ENG-06,
   ENG-13, ENG-14, ENG-19, ENG-23, ENG-25, PLT-02 to PLT-08, IPC, LIF,
   APP, SHR (D5 to D7, D15, D16, D20 to D25).
4. Providers: PRV-01 to PRV-08.
5. CI and release: CI-01 to CI-14, TST-02 (D26, D30, D31).
6. Documents and agent files: DOC, AGT (D27, D28), the spec, and the
   removal of this file (D29).

### Open points

None.

## Scripts, CI, and release (`CI`)

### ~~CI-01 (P2) The Swift lint cannot fail~~

**Done.** Both scripts pass `--strict`.

- Where: `scripts/swift/lint.sh:27`, `scripts/swift/format.sh:34`.
- Problem: both run `swift format lint --recursive`. Without `--strict`
  the command prints findings as warnings and exits 0. The lint step
  and the format check pass with any number of violations.
- Verified: a file with nine violations gave exit 0. With `--strict`
  the exit code was 1.
- Fix: add `--strict` to both calls. The tree is clean today, so the
  change does not turn CI red.

### ~~CI-02 (P2) The Tier 1 time budget measures the compiler~~

**Done.** `scripts/test.sh` builds the tests first and times the run
only.

- Where: `scripts/test.sh:10-31`, `.github/workflows/test.yml:81`.
- Problem: the clock starts before `cargo test` and `swift test`, so
  the 300 s budget includes compilation. `AGENTS.md` §9.1 states the
  budget for the suite. A cold cache (a `Cargo.lock` change, a new
  runner image, Windows) can fail the gate with no slow test. A warm
  cache hides a slow test.
- Fix: build first (`cargo test --no-run`, `swift build --build-tests`),
  then time the run only.

### ~~CI-03 (P3) Cache keys collide inside a release run~~

**Done.** Each workflow has its own word in the cache key. The `package`
job has no build cache.

- Where: `.github/workflows/{lint,test,build}.yml` (cache key uses
  `github.workflow`), `.github/workflows/release.yml:186`.
- Problem: in a reusable-workflow call, `github.workflow` is the
  caller's name. In a release run the lint and test jobs both use the
  key `<os>-cargo-release-<lock hash>`. The `package` job uses the same
  key. The first job to finish saves its `target/`; the others cannot
  save. The `package` job then restores a debug `target/` from lint or
  test, which it cannot use.
- Also: a cache made on a tag ref is visible to that tag only. The
  release cache never helps the next release.
- Fix: put a fixed word in each key (`cargo-lint-`, `cargo-test-`).
  Remove the `target/` cache from the `package` job. A signing job is
  better built from a clean tree.

### ~~CI-04 (P3) The release gate rewrites files and still passes~~

**Done.** The gate runs `scripts/format.sh check`.

- Where: `scripts/release.sh:21`.
- Problem: the gate runs `scripts/format.sh` with its default mode,
  `apply`. Unformatted code is reformatted in place and the gate
  continues. The lint step then checks the rewritten tree. The gate
  reports success and leaves a dirty worktree, which
  `scripts/version.sh` refuses in the next step without saying why the
  tree is dirty.
- Fix: run `scripts/format.sh check` in the gate.

### ~~CI-05 (P3) `version.sh prerelease` can create a version that sorts lower~~

**Done.** The script refuses a version that does not sort above the
current one, `prerelease` on a stable version, `bump` on a prerelease,
and a number with a leading zero.

- Where: `scripts/version.sh:407-429`, `scripts/tests/version.sh:182-196`.
- Problem: on a stable `VERSION` (for example `0.2.0`),
  `version.sh prerelease rc` prepares `0.2.0-rc.1`. In semver that
  version is lower than the released `0.2.0`. The test asserts this
  result. `version.sh set <lower version>` is also accepted.
- Other gaps in the same script:
  - `bump patch` on `0.2.0-alpha.3` gives `0.2.1` and skips `0.2.0`.
  - A prerelease number with a leading zero (`08`) reaches a bash
    arithmetic test and fails with an octal error.
- Fix: refuse a target version that is not greater than the current
  one. Refuse `prerelease` on a stable version unless the base version
  is given.

### ~~CI-06 (P3) The version script tests miss most commands~~

**Done.** The tests cover `bump`, `check-sync`, `sync`, `metadata`, an
existing tag, another branch, and a fixture with a member crate and a
lockfile.

- Where: `scripts/tests/version.sh`.
- Problem: the tests cover `set`, `prerelease`, `release`, the dirty
  worktree gate, and the changelog gate. Nothing covers `bump`,
  `check-sync` on a mismatch, `sync`, `metadata` (the Apple build
  version mapping `a`/`b`/`fc`), an existing tag, or a wrong branch.
  The fixture has no members and no `Cargo.lock`, so the lockfile check
  never runs.
- Fix: add one case for each command and one fixture with a member
  crate and a lockfile.

### ~~CI-07 (P3) The pre-commit hook does a full release build on every commit~~

**Done.** The hook runs lint and Tier 1 only, and its comment states the
rule. An installed hook changes when `scripts/hooks.sh install` runs
again.

- Where: `scripts/hooks.sh:67-76`, `scripts/rust/build.sh:28`.
- Problem: the hook runs lint, test, then `scripts/build.sh`, which is
  `cargo build --workspace --release` with fat LTO and one codegen
  unit, for all nine crates, the two dev tools included, plus a Swift
  release build. The validation ladder in `AGENTS.md` §8.4 is format,
  lint, test, e2e. It does not ask for a release build.
- Also: the hook comment narrates history ("It also no longer wipes
  runtime state"), which §8.9 forbids.
- Fix: drop the build step from the hook, or build the debug profile.
  Rewrite the comment to state the rule only.

### ~~CI-08 (P3) `clean.sh` signals a PID from a file without checking what it is~~

**Done.** `clean.sh` and the harness `sandbox-stop` signal only a
process whose command is `vapor` or `vapord`.

- Where: `scripts/clean.sh:8-20`.
- Problem: a stale `daemon.pid` from a crashed sandbox can name a PID
  that the OS gave to another process. `kill -0` succeeds and the
  script sends TERM, then KILL, to that process.
- Fix: check that the process command is `vapord` or `vapor` before
  the signal (`ps -p <pid> -o comm=`).

### ~~CI-09 (P3) Packaging does not verify the notarized result~~

**Done.** The script reads the status of the submission, prints the
notarization log on a refusal, and runs `stapler validate` and `spctl`
on the stapled app. This path did not run: no signing material exists
yet.

- Where: `apps/macos/scripts/package.sh:310-337`.
- Problem:
  - `spctl` runs before notarization, and a failure is a warning only.
    On a signed build it always fails at that point.
  - After `stapler staple` nothing checks the stapled app
    (`stapler validate`, `spctl -a -t exec`).
  - `notarytool submit --wait` exits 0 when Apple answers "Invalid".
    The script then fails in `stapler` with an unrelated message, and
    the notarization log is not fetched.
- Fix: read the submission status, print `notarytool log` on a
  rejection, and run `stapler validate` plus `spctl` after stapling.
  Make both hard failures on a stable tag.

### ~~CI-10 (P3) Dead and speculative branches in the build scripts~~

**Done.** The Xcode fallback, `VAPOR_XCODE_SCHEME`, the copy of
`apps/macos/Resources`, and the three name overrides are gone.
`.env.example` names no entitlements file.

- `scripts/swift/build.sh:52-70`: an `xcodebuild` fallback that runs
  only when `Package.swift` is missing. `VAPOR_XCODE_SCHEME` in
  `.env.example` exists for it alone.
- `apps/macos/scripts/package.sh:275-277`: copies `apps/macos/Resources`,
  which does not exist.
- `apps/macos/scripts/package.sh:5-8`: `APP_NAME`, `EXECUTABLE_NAME`,
  and `BUNDLE_ID` can be overridden from the environment. `AGENTS.md`
  §2.2 fixes all three.
- `.env.example:65`: the example `VAPOR_ENTITLEMENTS` path
  (`apps/macos/Entitlements.plist`) names a file that does not exist.
- Fix: remove the branches and the overrides (§8 "no speculative
  abstraction, no dead code").

### ~~CI-11 (P3) Small script inconsistencies~~

**Done.** Fixed: one default for the format scripts, the mode check
before any side effect, the `.exe` suffix in `soak.sh`, the arguments
after `--`, a trailing comment in `.env`, `VAPOR_DIR` in `perf.sh`, and
the dead `.gitignore` line.

- `scripts/format.sh` defaults to `apply`. `scripts/rust/format.sh` and
  `scripts/swift/format.sh` default to `check`.
- `scripts/build.sh` and `scripts/rust/build.sh` validate the mode
  argument after they create directories and run `version.sh`.
- `scripts/soak.sh` has no `.exe` suffix handling. `scripts/e2e.sh` has.
- `scripts/e2e.sh:38-48`: after `--`, the loop still rewrites
  `--sandbox`, `--list`, and `--providers` in the forwarded arguments.
- `scripts/dotenv.sh`: a value with a trailing comment
  (`KEY=value # note`) loads with the comment in it.
- `scripts/perf.sh:11-13` exports `VAPOR_DIR` and creates
  `logs/` and `state/`. `scripts/soak.sh` then unsets `VAPOR_DIR`.
- `.gitignore`: `!.cursor/environment.json` cannot take effect. Git
  does not look inside the ignored `.cursor` directory.

### ~~CI-12 (Idea) No dependency check in any gate~~

**Done.** `cargo deny check` runs in `scripts/rust/lint.sh` with
`deny.toml`, and `lint.yml` installs the tool with cargo. The first run
found advisories in rustls, rustls-webpki, webpki-roots, and the SQLite
that rusqlite bundles. All four are updated.

- Where: `Cargo.toml` (every dependency pinned with `=`), all workflows.
- Problem: nothing reports a known-vulnerable crate, and no process
  moves the exact pins forward. The repository is public and ships a
  network client with OAuth.
- Fix: add `cargo deny check advisories licenses` (or `cargo audit`) to
  `scripts/lint.sh` or to the release gate. A release gate fits the
  "no scheduled workflows" rule.

### ~~CI-13 (Idea) The release ships one CPU architecture and no document says so~~

**Done.** The README, `AGENTS.md` §7.2, and the release body say that
the Mac app is built for Apple silicon (D33).

- Where: `apps/macos/scripts/package.sh:129-144`.
- Problem: `swift build` and `cargo build` target the host only. The
  runner is arm64, so `Vapor.zip` is arm64-only. macOS 26 still runs on
  some Intel Macs. `docs/tasks/cli.md` plans an `x86_64-apple-darwin`
  CLI, but the app has no stated position.
- Fix: state "Apple silicon only" in the README requirements, or build
  a universal app (`--arch arm64 --arch x86_64`, `lipo` for Rust).

### ~~CI-14 (P1) The next release fails at the publish step~~

**Done.** The publish job builds the body with `scripts/notes.sh`, which
cuts a long section at a line end and links the changelog (D26). The
`Unreleased` section is rewritten: 53 lines, 8 000 characters.
`AGENTS.md` §10 and the `vapor-docs` skill carry the rule: one line for
each pull request, 300 characters at most.


- Where: `CHANGELOG.md` `Unreleased`, `.github/workflows/release.yml:428-480`.
- Problem: the publish job copies the changelog section of the tag into
  the GitHub Release body. The `Unreleased` section holds 294 entries
  and 190 178 characters. GitHub refuses a release body over 125 000
  characters. The job fails after preflight, the four gates (about one
  hour), and packaging all passed.
- Verified: measured the section with the same regular expression the
  workflow uses. The GitHub limit is not tested here; it is the
  documented API validation ("body is too long").
- Related: the section has `### Changed` eight times, `### Added` six
  times, and `### Fixed` six times. Lines reach 2 183 characters. The
  release runbook asks for "concise and user-impact focused" sections.
- Fix:
  1. Rewrite the section before the next tag: one `Added`, `Changed`,
     `Fixed` group each, one or two sentences for each entry.
  2. Add a preflight check that fails when the extracted notes exceed
     the limit.
  3. Add a length guideline for an entry to the `vapor-docs` skill.

## Documentation (`DOC`)

Checked against the code where the finding says so. Findings that need
a code fix are in the code sections and point back here.

### ~~DOC-01 (P2) Four documents say a one-way mirror keeps no recoverable copy~~

**Done.** One statement in every place: the local trash keeps what a
mirror removes on this device; an overwrite, and a removal in a cloud
without a trash, have no undo.

- Where: `docs/architecture/sync-modes.md:146-147`,
  `docs/product/status-and-goals.md:31`, `docs/plans/core.md:119-121`,
  `docs/tasks/macos.md:258-263` (M4-5).
- Problem: `AGENTS.md` §4 and `sync-modes.md:131-136` say the local
  trash keeps what a mirror removes on this device for the retention
  window. The four places above say "no recoverable copy". `sync-modes.md`
  contradicts itself inside one section. M4-5 is the task that writes
  the data-loss warning for the app, so the wrong statement becomes UI
  copy.
- Fix: use one statement everywhere. The local trash keeps local
  removals. Overwrites and cloud-side removals on a provider without a
  trash have no undo.

### ~~DOC-02 (P3) `docs/plans/core.md` contradicts the system as built~~

**Done.** The core plan holds intent only (474 lines to 267). The parity
matrix is in `platform-abstractions.md`. The CLI and macOS plans are
corrected.

- §3.2: the Linux unit is `vapord.service` with `Restart=on-failure`.
  Built: `sh.arn.vapor.daemon.service` with `Restart=no`
  (`core/platform/src/service/linux.rs:38,87`).
- §3.3: headless Linux may keep secrets in an age-encrypted file under
  `vapor_dir`, and the `keyring` crate is the default. Built: an
  external command or the Secret Service, never a file (`AGENTS.md` §6).
- §9: the parity matrix says "needs impl" for the macOS metrics sampler
  and idle detector. Both shipped.
  `docs/architecture/platform-abstractions.md:231` sends the reader to
  this matrix.
- §2.1, §4, and §10 describe work that is complete.
- `docs/plans/cli.md:152`: `cargo run -p vapor` fails. The package is
  `vapor-cli`.
- `docs/plans/macos.md:52`: lists a `Flush now` control. IPC version 3
  removed it.
- Fix: cut each plan to what is still intent (principles, scope,
  non-goals). Move the parity matrix into `platform-abstractions.md`.

### ~~DOC-03 (P3) System documents describe test tiers and tools that do not exist~~

**Done.** Fuzz, `loom`, and long property cases read as open work with
the task file. The performance README no longer says "mechanical".

- `docs/architecture/testing-strategy.md:292-304`: a fuzz tier "via
  `cargo-fuzz`" with "short corpora in-tree". No fuzz target exists.
- `testing-strategy.md:210-218` and `docs/ci/README.md:42-45`: `loom`
  tests and "long-running property cases" in Tier 2, in the present
  tense. Tier 2 runs one soak cell.
- `docs/performance/README.md:14-17`: "pass/fail is mechanical" for five
  harness scenarios. See DOC-04.
- Fix: write "open work" with a pointer to the task file, as the
  `vapor-docs` skill requires for anything not shipped.

### ~~DOC-04 (P3) Most SLO thresholds have no measurement~~

**Done.** Each threshold says gated, reported, tested, or not measured.
`perf.sh` no longer checks for strings. The task id is gone.

- Where: `docs/performance/acceptance-budgets-and-benchmark-harness.md`,
  `scripts/perf.sh:19-25,52-65`.
- Problem: the gate asserts five things: no oracle violation, every
  phase converged, crash recovery, RSS p95 (SLO-3), and CPU average
  (SLO-2). `perf.sh` also checks that the strings `SLO-1` to `SLO-5`
  exist in the document, which proves nothing. No tool measures these:
  all of SLO-1, the SLO-2 p95 and latency lines, the SLO-3 entry bounds
  and the 15 minute convergence, all of SLO-4, and the SLO-5 30 second
  and 10 000 intent lines.
- Also: line 52 cites a task id (`T-16`), which `AGENTS.md` §8.9 keeps
  out of system documents.
- Fix: mark each threshold "gated", "reported", or "not measured".
  Remove the marker check from `perf.sh`.

### ~~DOC-05 (P3) Stale statements~~

**Done.** Each of the fifteen statements is corrected.

Each line is one statement that the repository contradicts today.

- `docs/architecture/testing-strategy.md:276`: Tier S is "scheduled and
  on demand". No workflow has a schedule.
- `testing-strategy.md:73`: `core/platform`, `core/lifecycle`, and
  `core/cli` are "planned crates". Line 299 waits for the lifecycle
  "to move over".
- `AGENTS.md` §9.2: only the secret store has a contract test.
  `FsWatcher` has one (`core/platform/src/fs_watch/contract.rs`).
- `docs/ci/README.md:34`: "three tiers", then four.
- `docs/product/status-and-goals.md:8`: a live-account e2e tier "remains
  future". `./scripts/e2e.sh --provider gdrive` is a leg of the release
  gate. Line 5 lists phase numbers.
- `docs/architecture/README.md:31`: `system-overview.md` holds "the
  planned implementation sequence". It holds a history. The trait list
  on line 58 omits `TrashBin`.
- `docs/architecture/system-overview.md`: no `core/ipc` and no `tools/`
  in the component list or the module sketch. It puts the IPC models in
  `core/shared`.
- `docs/architecture/macos/README.md:26` and the root `README.md`
  structure list: the bundle layout and the trait list omit
  `Contents/Helpers/vapor` and the trash trait.
- `docs/architecture/linux/ipc-transport.md:42`: `systemctl --user stop
  vapord`. The unit is `sh.arn.vapor.daemon.service`.
- `docs/operations/runtime-logging-and-localization.md:45-46`: the same
  sentence twice.
- `docs/operations/macos/launchagent-policy.md:44-45`: "`VAPOR_*`
  variables remain per-field overrides". The README and
  `docs/product/configuration.md` say no variable overrides a key.
- `launchagent-policy.md:152-155`: the SIGKILL scenario expects no
  restart for 30 s. With the app running, the health monitor restarts
  the daemon on its next tick. The scenario must say "no app and no
  supervisor".
- `docs/development/runbook.md:5`: the tools list omits `tools/soak`.
  `docs/development/README.md:27` says the runbook lists "planned
  crates".
- `docs/tasks/README.md:237`: two tasks "wait on the native Linux
  traits". They landed.
- `docs/operations/release-process.md:253`: "Stable releases are created
  as drafts". Every release is created as a draft.

### ~~DOC-06 (P3) The release runbook lists the tagging commands before validation~~

**Done.** Step 1 edits the changelog only, and step 3 holds the
commands. The publish job refuses to replace the assets of a published
release.

- Where: `docs/operations/release-process.md:202-241`.
- Problem: step 1 ("Prepare release changes") lists
  `./scripts/version.sh set|bump|prerelease|release`, then the changelog
  edit. Those commands commit and tag at once. Step 3 runs them again.
  A reader who follows step 1 tags before the changelog section exists
  and before the gate of step 2.
- Also: a rerun on a published release replaces its assets
  (`gh release upload --clobber`). Users may hold the old files and the
  old checksums.
- Fix: step 1 edits the changelog only. Make the workflow refuse to
  replace assets when the release is not a draft.

### ~~DOC-07 (P3) Em dashes in every document class except the agent files~~

**Done.** One sweep removed 813 em dashes from 134 files (D27).
`scripts/lint.sh` fails on a new one.

- Rule: `AGENTS.md` §8.11 and the `unslop` skill ban em dashes in every
  style.
- Count: `docs/` 449, Rust sources 347, Swift 23, `CONTRIBUTING.md` 15,
  `README.md` 10, `SECURITY.md` 10, scripts 5. `AGENTS.md` and
  `.agents/` have none.
- Fix: one sweep. Then add a grep to `scripts/lint.sh` so the count
  stays at zero.

### ~~DOC-08 (P3) The task lists hide the open work~~

**Done.** The three task files hold open work only (2 130 lines to 740).
Stale items are closed or rewritten, reminders are gone, and the README
lists the open waves.

- Where: `docs/tasks/{core,macos,cli}.md`.
- Problem: 239 items are done and 89 are open. A reader looks for the
  next task through 1 700 lines of history. Several open items are not
  tasks, or are stale:
  - `core.md` CT-6: the skew matrix tests exist
    (`core/ipc/tests/skew_matrix.rs`; C5-5 is closed with the same
    text).
  - `core.md` RV-12: half done (CT-1 records `proptest` as adopted).
  - `core.md` T-1 to T-16: `docs/tasks/README.md:390` calls them
    standing invariants. They can never be closed.
  - `core.md` CT-7, `macos.md` MT-3, `cli.md` LT-6: reminders and
    rules. The rules are already in `AGENTS.md` §9.3.
  - `macos.md` MT-2: waits for `core/lifecycle` "to land". It landed,
    and `testing-strategy.md` says the Swift tests stay.
  - `macos.md` M3-1: open, and its note says both controls landed.
- Fix: delete closed phases (git keeps them). Move standing rules to
  `testing-strategy.md`. Close or rewrite the stale items.

### ~~DOC-09 (P3) History in documents that describe the system now~~

**Done.** The history paragraphs are gone from the seven documents.
`ipc-contracts.md` describes version 4 only.

- `docs/architecture/ipc-contracts.md:21-23,35-110`: a "Historical note"
  and 75 lines of schema history for versions 1 to 3, which no build
  speaks.
- `docs/architecture/system-overview.md:54-72` ("Implementation
  history"), `docs/architecture/sync-modes.md:165-176` ("Build order"),
  `docs/development/soak-testing.md:213-218`,
  `docs/architecture/macos/app-lifecycle.md:66`,
  `docs/operations/macos/launchagent-policy.md:54`,
  `docs/architecture/data-flow.md:385` ("as a new module") and `:395`
  ("legacy ... path so single-scope setups upgrade in place").
- Fix: keep the fact, drop the history. `CHANGELOG.md` holds what
  changed.

### DOC-10 (P3) The same fact lives in many files

**In progress.** The toolchain baseline and the script list have one
owner each. The crash-loop schedule, the bundle layout, and the test
tiers still repeat: `docs/tasks/core.md` D-1.

- The toolchain baseline: `AGENTS.md` §8.3 and
  `docs/development/toolchain-baseline.md`, both dated March 2026.
- The script list: `README.md`, `docs/development/runbook.md`,
  `docs/ci/required-checks.md`.
- The crash-loop schedule: four documents and one skill.
- The bundle layout: six documents.
- The test tiers: `AGENTS.md` §9.5, `testing-strategy.md`,
  `docs/ci/README.md`, `e2e-verification.md`, `runbook.md`.
- Problem: DOC-05 shows these copies drift.
- Fix: one owner file for each fact. The other files link to it.

### ~~DOC-11 (P3) The README does not prepare a user for the first install~~

**Done.** Install states the requirements, the unsigned prerelease, the
setup command, and the macOS folder prompt. The empty Benchmarks section
is gone.

- Where: `README.md:15-21,77-79`.
- Problem: the prerelease app is ad-hoc signed. Gatekeeper refuses it
  (`SECURITY.md:82-85` says so). The Install section does not say that,
  and gives no requirement (macOS 26, CPU architecture). The
  "Benchmarks" section is "TBD".
- Fix: add the requirements and the unsigned-prerelease note to
  Install. Remove the empty section until numbers exist.

### ~~DOC-12 (Idea) Record the decisions that already exist as specs~~

**Done.** No backfill (D28). The specs README says so, and
`docs/specs/2026-11-14-full-review.md` is the first spec.

- Where: `docs/specs/README.md:45` ("None yet").
- Problem: `AGENTS.md` §1.2 sends a contradicting change back to the
  owner, and §0 tells a session to read the specs. No spec exists.
  Owner decisions are spread over other files: the file-only engine
  (2026-07-07, `data-flow.md:161`), no quarantine for one-way modes
  (`docs/tasks/core.md` C8-63), nothing created before onboarding
  (2026-09-10), the full Drive scope, and the decisions rule.
- Fix: write one short spec for each, with the decision and its date.

### ~~DOC-13 (Idea) Track the Google verification requirement as a task~~

**Done.** Tracked as `docs/tasks/core.md` P-5. The decision waits for
the owner.

- Where: `docs/operations/provider-auth-operations.md:60-74`.
- Problem: the `drive` scope is restricted. Google requires app
  verification and a yearly security assessment before more than 100
  users can sign in, and a client in Testing status signs every user
  out weekly. No task or plan mentions this. It gates general
  availability of the main provider.
- Fix: add a task with the owner's decision: verification, or a
  narrower scope.

### ~~DOC-14 (Idea) The Windows pipe name cannot serve two runtime directories~~

**Done.** The transport document names the pipe with the user and the
hash of the runtime directory. The build of the transport is
`docs/tasks/core.md` C6-9.

- Where: `docs/architecture/windows/ipc-transport.md:13-16,31-34`.
- Problem: the design names the pipe `vapord-<user-sid>`. One daemon
  serves one `vapor_dir`, and the e2e harness runs many daemons for one
  user at the same time. On Unix the socket path contains the
  `vapor_dir`. The Windows name does not.
- Fix: add the `vapor_dir` hash that the deep-path relocation already
  uses to the pipe name, before the transport is built.

## Agent files: `AGENTS.md` and skills (`AGT`)

### ~~AGT-01 (P2) The release skill describes a changelog step the script does not do~~

**Done.** The skill says that a person moves the entries and that the
script only checks the section.

- Where: `.agents/skills/vapor-release/SKILL.md:18-19`.
- Problem: the skill says "the script rolls them into the versioned
  section". `scripts/version.sh:259-277` only checks that a
  `## [<version>]` section exists and stops when it does not. The
  runbook says a person moves the entries. An agent that follows the
  skill gets "CHANGELOG.md missing section".
- Fix: correct the skill, or make the script move the section.

### ~~AGT-02 (P3) Skill descriptions name the wrong platforms~~

**Done.** The three descriptions name the right platforms and the Drive
leg.

- `.agents/skills/vapor-e2e/SKILL.md:3,154-155`: "macOS today; daemon
  scenarios skip elsewhere". Linux runs them.
- `.agents/skills/vapor-soak/SKILL.md:3`: "macOS only today". Four
  cells run on Linux.
- `.agents/skills/vapor-provider/SKILL.md:64-65,72`: a live-account tier
  is "a separate future"; the README section is named "Cloud
  Providers". The harness has `--provider gdrive`, and the section is
  "Providers".
- Problem: the description is the only text an agent reads to choose a
  skill (`AGENTS.md` §8.10).

### ~~AGT-03 (P3) The validate skill claims a check that no script makes~~

**Done.** `scripts/lint.sh` checks that each locale catalog parses, so
the sentence is true.

- Where: `.agents/skills/vapor-validate/SKILL.md:30`.
- Problem: "`lint` is cheap and catches broken locale JSON". No lint
  step reads `assets/locales/*.json`. `scripts/swift/resources.sh` only
  copies the files.
- Fix: validate the catalogs in `scripts/lint.sh`
  (`python3 -m json.tool`), or change the sentence.

### ~~AGT-04 (P3) `AGENTS.md` uses the task ids it forbids~~

**Done.** `AGENTS.md` points at the task file with no id, and the status
paragraph of §12 is gone.

- Where: `AGENTS.md` §9.2 (`CT-5`, `CT-4`), §12 (`R-1`).
- Problem: §8.9 keeps task ids out of code and system documents because
  they do not last. `AGENTS.md` is the most durable file. §12 also
  carries a "Current status" paragraph, which is state, not a rule.
- Fix: point at the task file without the id. Move the status line to
  the task file.

### ~~AGT-05 (P3) Small contradictions between `AGENTS.md` and the skills~~

**Done.** Fixed: the force-push wording, the Tier 1 workflow, the local
check list, the changelog groups, the scenario id, and the Linux recipe,
which now calls the wrapper scripts.

- Force push: `AGENTS.md` §10 says `--force-with-lease`. The `vapor-pr`
  skill forbids a bare `--force-with-lease` and requires
  `--force-with-lease --force-if-includes origin HEAD`.
- Tier 1: `AGENTS.md` §9.5 says `./scripts/test.sh` runs "via
  `lint.yml` / `test.yml`". `lint.yml` does not run it.
- Local checks: `docs/ci/required-checks.md:165-171` lists lint, test,
  build. `AGENTS.md` §8.4 and the validate skill list format, lint,
  test, e2e.
- Changelog groups: the `vapor-docs` skill allows `Added`, `Changed`,
  `Fixed`, `Docs`, `Tooling / CI`. `CHANGELOG.md` also uses `Testing`,
  `Security`, `Performance`, and four `Fixed (...)` groups.
- `.agents/skills/vapor-config/SKILL.md:44`: scenario "S1". The id is
  `S01`.
- `docs/development/runbook.md:168-177`: the Linux container recipe
  calls `cargo clippy` and `cargo test` directly. `AGENTS.md` §8.4 says
  never raw tool commands.

### AGT-06 (Idea) `AGENTS.md` is long for a file every session loads

**In progress.** The future Windows and Linux chains and the toolchain
list point at their owner documents. The tier catalogue stays: it holds
invariants. The file did not get shorter, because the decisions added
rules.

- Size: 886 lines, 51 KB.
- Problem: §0 says procedures live in skills. The file still holds
  procedure-level detail: the packaging metadata (§7.2), the future
  Windows and Linux chains (§7.3, §7.4), the toolchain list (§8.3), and
  the tier catalogue (§9.5, §9.8).
- Fix: move that detail to the owning document or skill and keep the
  rule in one line.

## Sync engine: `core/daemon` (`ENG`)

All findings in this section are from reading the code. None was
reproduced with a running daemon. The e2e harness pins neutral throttle
inputs (`VAPOR_THROTTLE_INPUTS=static`), so it cannot show ENG-01 to
ENG-04.

### ~~ENG-01 (P1) On battery or while the user types, a folder that had a burst stops syncing~~

**Done.** Built as D8 and D9: a reconcile scan starts in Light and in
Throttled at that state's caps, and a compacted folder is released at
the end of its deferral in every state but Suspended. e2e scenario S51
covers a user at the keyboard, battery power, and Low Power Mode at
once.

- Where: `core/daemon/src/reconcile.rs:98-111`,
  `core/daemon/src/throttle.rs` (`battery_candidate`,
  `user_activity_candidate`, `caps_for`),
  `core/daemon/src/event_intents.rs:391-394,458-461`.
- Facts:
  1. A reconcile starts only in `Idle`. Battery power gives `Light`.
     Keyboard or pointer input in the last 30 s gives `Throttled`. Both
     have `allow_reconcile: false`.
  2. A storm compacts a directory into one deferred subtree reconcile.
     The trigger is 200 unique paths or 600 events in one directory in
     2 s, or 5 000 pending events for the whole root.
  3. While a subtree is compacted, every later event and intent under
     it is absorbed. Nothing is uploaded from there until its reconcile
     completes.
  4. The runtime also feeds the storm detector itself: it reports the
     files of a directory that appeared (`runtime.rs:2937`), and the
     watcher reports the files that a bulk download wrote.
- Result: on a laptop on battery, a folder of 300 photos that is
  dragged in, unzipped, or received from another device does not sync.
  Later edits in that folder do not sync either. This lasts until the
  device is on AC power with no input for 30 s. On AC power it lasts
  until the user stops typing. The 10 minute cap
  (`DEFERRED_RECONCILE_MAX_DELAY_MILLIS`) only makes the reconcile
  releasable; it still needs `Idle`.
- The same gate holds the startup reconcile (changes made while the
  daemon was down), the repair after dropped watcher events, and the
  repair after an expired changes cursor.
- The only way out today is `vapor reconcile`.
- The README says "Changes are picked up within seconds while you work".
- Fix (owner decision): let a subtree reconcile run in `Light` and
  `Throttled` at the reduced caps, or bound the wait (for example, run
  one slice per minute in any state but `Suspended`). Keep per-file
  events flowing for a compacted subtree once its storm is quiet.

### ~~ENG-02 (P1) Low Power Mode stops all sync, with no way out~~

**Done.** Built as D10 and D19: Low Power Mode gives Throttled. Only
critical thermal pressure gives Suspended. High system CPU gives
Throttled. A disk that is almost full holds downloads only.

- Where: `core/daemon/src/throttle.rs` (`low_power_candidate`),
  `core/daemon/src/reconcile.rs:99`.
- Problem: `low_power_mode` gives `Suspended`: zero planner, hash,
  upload, and download slots. The user's own `vapor reconcile` is
  refused in `Suspended` too. macOS lets a user keep Low Power Mode on
  always, or always on battery. For those users Vapor never syncs, and
  the status shows only "low power mode is enabled".
- Fix (owner decision): map Low Power Mode to `Throttled` (one transfer
  at a time), or let an explicit user request through.

### ~~ENG-03 (P2) Every start on battery or with an active user syncs nothing for 60 s~~

**Done.** The barrier holds nothing while the state is Suspended, and
its deadline is logged at INFO. With D8 the scan starts in every other
state, so a healthy start no longer waits 60 s.

- Where: `core/daemon/src/runtime.rs:3571-3665,2720-2742`.
- Problem: a queued whole-scope reconcile raises the startup barrier.
  While the barrier is up, the runtime leases one intent at a time and
  requeues every non-reconcile intent. The reconcile cannot start
  outside `Idle`, so the barrier stays until its 60 s deadline. It then
  clears with a WARNING, on a healthy run.
- The barrier is raised again after a cloud-root recovery, a returned
  local root, dropped watcher events, a type-mismatch answer, and a
  reattach.
- Fix: raise the barrier only when the reconcile can start now.
  Log the deadline at INFO (`AGENTS.md` §8.1).

### ~~ENG-04 (P2) A reconcile that waits for idle costs two database writes per second~~

**Done.** A reconcile intent is not leased while the state cannot admit
it. A test counts zero leases and requeues under Suspended.

- Where: `core/daemon/src/runtime.rs:3610-3640`,
  `core/daemon/src/state_db.rs:470-545`.
- Problem: the waiting reconcile intent is leased (`UPDATE`), refused,
  and requeued (`UPDATE`) with a 1 s delay, for as long as the throttle
  is not idle. The tick report then counts leased and requeued work, so
  the loop uses the 250 ms cadence and not the 1 s idle cadence. On
  battery this continues for the whole session (ENG-01).
- Fix: do not lease a reconcile intent while the throttle state cannot
  admit it. Wake on the state change.

### ~~ENG-05 (P2) Release builds have no panic containment~~

**Done.** Built as D15: the release profile uses panic = unwind, so the
catch in the profile tick and in the provider-job worker holds in
release builds too.

- Where: `Cargo.toml` (`[profile.release] panic = "abort"`),
  `core/daemon/src/multi_runtime.rs:17-23,637,807,1036`,
  `core/daemon/src/provider_jobs.rs:234,484`.
- Problem: `catch_unwind` cannot catch with `panic = "abort"`. A panic
  in one profile tick or in one provider job ends the whole daemon. A
  panic that repeats on the same file gives five crashes and a
  crash-loop pause for every profile. The module comment says so. These
  documents promise containment: `docs/architecture/data-flow.md:397`,
  `docs/architecture/testing-strategy.md:105-107`, the `vapor-debug`
  skill (line 70), and `docs/tasks/core.md` (RV-5). The containment
  tests run in the debug profile only.
- Fix: remove `panic = "abort"` from the release profile for `vapord`
  and `vapor`, or correct the documents.

### ~~ENG-06 (P2) A profile suspended at runtime never comes back~~

**Done.** Built as D16: a profile that the runtime suspended is composed
again after 1, 5, and 30 minutes, then each 30 minutes. The daemon no
longer exits when every profile is suspended at runtime; that exit
handed a failure that clears on its own to the crash-loop guard. Open
for the owner: the first retry is now 1 minute for a single profile,
where the old exit and restart took seconds.

- Where: `core/daemon/src/multi_runtime.rs:53,645-668,1024-1038`.
- Problem: five consecutive tick errors (about 1.25 s at the busy
  cadence) suspend a profile for the life of the process. A short
  failure, for example a full disk or a locked database, is enough.
  With one profile the daemon exits and the crash-loop guard restarts
  it. With several profiles the others keep the daemon alive and the
  suspended one waits for a manual restart.
- Fix: compose the profile again with a backoff, as the code already
  does for a missing local root.

### ~~ENG-07 (P2) A broken `vapor.json` makes the daemon run the default profile~~

**Done.** Built as D5, as docs/tasks/core.md OB-1 planned. The two roots
have no compiled default. A file that cannot be read, a profiles value
of the wrong shape, or a scope key of the wrong type gives no profile
and never the default one. With no scope the daemon reports the new run
state unconfigured and names vapor profiles add; it creates no folder
and no state. A profile with one root set, or with an invalid id, is
named. The first scope composes without a restart, one config set for
each root. e2e scenario S61 fails on the base commit and passes now.

- Where: `core/daemon/src/bootstrap.rs:93-101`,
  `core/shared/src/config.rs:406-435,546-556`,
  `core/daemon/src/profiles.rs:40-43,118-124`.
- Problem: three cases end in the implicit `default` profile, which
  syncs `~/Vapor` with `~/cloud/Vapor` through the filesystem provider:
  1. the file is not valid JSON (the log says "continuing with
     defaults");
  2. the `profiles` value has a wrong shape (one bad field makes the
     whole key fall back to `[]`);
  3. every profile entry is invalid (for example an uppercase `id`).
- Result: on a fresh state the daemon creates and syncs two folders
  that the user did not configure. The configured profiles do not run.
  `AGENTS.md` §1 says Vapor never broadens scope beyond the configured
  roots. With existing state the root identity check holds the profile,
  which limits the damage.
- Fix: compose no profile in these cases and report the cause in
  `vapor status`.

### ~~ENG-08 (P2) The CPU ceiling is not a measured limit~~

**Done.** Built as D13: a regulator takes one worker from each cap for
each sample of the daemon's CPU above cpuPercent and gives one back
below three quarters of it. The three fixed own-CPU thresholds are gone.
The state steps to Throttled only when no worker is left to take.

- Where: `core/daemon/src/lib.rs:464-510`,
  `core/shared/src/constants.rs` (`*_VAPOR_CPU_PERCENT`),
  `core/daemon/src/resource_budget.rs:349-352,372-374`.
- Problem: `resourceLimits.cpuPercent` only scales the concurrency
  caps. No code compares measured CPU with the ceiling. The thresholds
  for the daemon's own CPU are constants: 8% gives `Light`, 15%
  `Throttled`, 25% `Suspended`.
  - With `cpuPercent: 5` the daemon can still use up to 8% before any
    state changes.
  - With `idleBoost.cpuPercent: 50` the state leaves `Idle` at 8%, the
    boost snaps to the base, and the ramp starts again. CPU-bound work
    gets a sawtooth, not a boost.
- `docs/architecture/data-flow.md:117` calls the ceilings hard caps.
- Fix: derive the three own-CPU thresholds from the effective ceiling,
  or document the ceiling as a concurrency scale.

### ~~ENG-09 (P2) The bandwidth ceiling is a share of an assumed 100 Mbps~~

**Done.** Built as D14 and D18. The ceiling is a share of the capacity
that the daemon measures: the highest rate a transfer step reached in
the last five minutes, for each direction, with one unlimited 1 MiB step
as a probe when the estimate is older. The estimate starts from the
interface speed that macOS reports (if_data.ifi_baudrate; 345.6 Mbit/s
on the test host, the same as ifconfig). With nothing known a direction
has no limit, and the first transfer is the first measurement. Uploads
and downloads have a bucket each. The input field is now
link_speed_kbps, and the slow-link throttle cause reads the estimate.
usedKbps is what moved in the last 5 s, and status also carries the two
capacities. Under VAPOR_THROTTLE_INPUTS=static the capacity is pinned at
100 Mbit/s so harness runs are the same on every host. Linux and Windows
report no interface speed yet.

- Where: `core/daemon/src/runtime.rs:2619-2627`,
  `core/shared/src/constants.rs` (`ASSUMED_LINK_CAPACITY_KBPS`).
- Problem: no platform sampler reports link capacity. The shaper rate
  is always `100 Mbps x bandwidthPercent`: 25 Mbps by default. On a
  slower link Vapor may use all of it. On a faster link the cap is
  lower than the documented share. The README lists the cap under
  "Available now" as a share of measured bandwidth, and
  `data-flow.md:112` says other traffic keeps the rest "by
  construction".
- Related: `network_throughput_kbps` is link capacity here, and "low
  throughput" (a throttle cause) in `throttle.rs`. One field has two
  meanings. A sampler that reports real throughput would throttle an
  idle link.
- Related: `resources.bandwidth.usedKbps` in `vapor status` is the
  shaper's allowed rate, not the usage (`runtime.rs:1586-1592`).
- Fix: measure capacity (open task), or name the setting and the README
  claim for what they do. Report real usage in `usedKbps`.

### ~~ENG-10 (P2) File hashing runs on the tick thread~~

**Done.** Hashing before an upload runs on the provider-job workers; the
worker checks the throttle between two 8 MiB steps and hands the hash
back under Suspended. The echo check reads no file: it compares the size
and the modification time that the apply recorded.

- Where: `core/daemon/src/executor.rs:1546-1609`.
- Problem: the hash stage reads up to 8 MiB for each hashing file in
  each tick, on the thread that also drains events, harvests jobs, and
  publishes status. "Hash workers" is a count, not threads. With eight
  workers one tick hashes up to 64 MiB. One file hashes at 32 MiB/s at
  most, so a 4 GB file needs two minutes before its upload starts. The
  upload then reads the file again.
- Related: the echo check hashes each downloaded file of 8 MiB or less
  on the tick thread, in every throttle state
  (`runtime.rs:3907-3914`).
- Fix: move hashing to the worker pool. For the echo check, compare the
  size and mtime that the apply recorded.

### ~~ENG-11 (P3) The memory ceiling shrinks the echo caches under bulk load~~

**Done.** The entry-count trigger is gone. The caches trim only when the
measured resident memory is over the ceiling, and a host with no memory
reading gets no trim.

- Where: `core/daemon/src/runtime.rs:2644-2691`.
- Problem: the entry budget is `memoryPercent x 400`, which is 4 000
  entries by default. The two echo caches are sized for 10 000 entries
  each. Above 4 000 entries the caches drop to 1 000 entries and a 5 s
  lifetime. That happens during a bulk transfer, when the caches matter
  most. An evicted echo becomes an upload plan with a hash and a remote
  probe. The resident-memory branch needs 10% of device memory and does
  not trigger in practice.
- Fix: derive the entry budget from the cache size, or drop it.

### ~~ENG-12 (P3) An unknown `syncMode` runs the profile as two-way~~

**Done.** An unknown syncMode stops the profile; status lists it
suspended with a reason that names the value and the three valid ones.

- Where: `core/daemon/src/profiles.rs:88-101`,
  `core/daemon/src/sync_directories.rs:58-72`.
- Problem: a hand-edited typo of `pull-only` makes local edits and
  deletions go to the cloud folder that the user wanted to protect.
- Fix: suspend the profile and name the bad value.

### ~~ENG-13 (P3) Errors are dropped in the "recreate the local root" path~~

**Done.** The merge flag is set first, the decision is marked applied
last, and the first failure stops the attempt and is logged.

- Where: `core/daemon/src/multi_runtime.rs:346-391,436-465`.
- Problem: the decision is marked applied first. Then the folder is
  created, the marker is written, and the merge-without-deletions flag
  is set, each with `let _ =`. If only the flag write fails, the next
  reconcile reads the empty folder as deletions. The mass-delete guard
  holds them only from 10 files.
- Fix: set the flag first, mark the decision applied last, and stop on
  the first error.

### ~~ENG-14 (P3) Smaller engine findings~~

**Done.** Every bullet is built. A profile that cannot compose is listed
with its reason. An answer that cannot be applied is tried again after 5
s, 60 s, then every 10 min, with one WARNING per reason, and the keep-
both answer is safe to try again. The first cloud-root probe of a daemon
profile runs on a worker at the first tick, so the IPC server answers at
once. A relative root is a profile problem with a reason; the CLI writes
full paths. The overlap check folds case for names that do not exist
yet. A rate limit narrows uploads, downloads, and planner probes to one.
The host name is read one time for each process. The watchers start
again when a local root returns. The stale comments and the comments
that narrate history are rewritten.

- A profile whose state database cannot be opened, or that fails to
  compose for another reason, is left out of `vapor status`
  (`multi_runtime.rs:1117-1129,1211-1220`). With one profile the status
  says "no enabled profiles".
- An answer that cannot be applied is retried on every tick with a
  WARNING each time (`runtime.rs:3188-3216`). The `keep-both` applier
  renames first, so its retry can never succeed.
- The first cloud-root probe is synchronous and runs before the IPC
  server starts (`bootstrap.rs:162-214`). Offline, `vapor status` gets
  no answer for the length of the HTTP timeouts.
- A relative `localRoot` resolves against the daemon's working
  directory (`sync_directories.rs:223`): `/` under launchd, the
  terminal's directory under `vapor run`.
- The overlap check misses two roots that do not exist yet and differ
  only by case on a case-insensitive volume
  (`sync_directories.rs:146-170`).
- The rate-limit slowdown clamps uploads only (`lib.rs:506`).
  Downloads and listings keep their width.
- `device_id::derive_device_id()` starts a `hostname` process twice for
  each profile at start (`runtime.rs:1269,1482`).
- `bootstrap.rs:310-322` has a `match Ok::<String, ()>(value)` with a
  dead arm. `bootstrap.rs:96-101` logs any load issue, an unknown key
  included, as ERROR "unreadable configuration".
- On Linux, a local root that returns at runtime gets no new watcher.
  An inotify watch follows the old inode. Not verified for FSEvents.
- Stale comments: `runtime.rs:401-403` (the idle signal is "a platform
  follow-up"), `runtime.rs:1824` ("a later task forbids"),
  `event_intents.rs:311` (`vapor flush`), `core/shared/src/config.rs:7-11`
  (environment variables override keys).
- About 75 comments narrate history ("pre-fix", "was retired",
  "previously"), which `AGENTS.md` §8.9 forbids. Examples:
  `core/lifecycle/src/crash_loop.rs:3`,
  `core/shared/src/runtime_paths.rs:131`,
  `core/providers/src/gdrive/mod.rs:1647`.

### ~~ENG-15 (P1) A file put back with an old modification time is deleted again~~

**Done.** Built as D4: both tombstone-against-mtime rules are gone, and
so is the tombstone table, which nothing else read. The sync index is
the only evidence of a deletion.

- Where: `core/daemon/src/reconcile_walk.rs:595-616,746-757`,
  `core/daemon/src/executor.rs:2389-2415`.
- Facts:
  1. A deletion made on this device leaves a tombstone for 30 days.
  2. In the walk, a cloud-only file whose modification time is not
     newer than the tombstone becomes a `Delete` intent
     (`local_deletion_wins`).
  3. The delete planner finds no sync-index row (the deletion removed
     it) and treats that as "unchanged" (`None => true`). The cloud
     object is deleted with no comparison.
- Failure: the user deletes `notes.txt` on device A. Later the user
  puts an older `notes.txt` back in the cloud with a tool that keeps
  modification times (`cp -p` into a filesystem cloud folder, a
  restore from backup, an upload through another Drive client). The
  daemon on A is not running at that moment, or its changes cursor
  expires. At the next reconcile, A deletes the file from the cloud.
  The filesystem provider removes it for good. Google Drive moves it to
  the Drive trash.
- The mirror case (`remote_deletion_wins`, lines 725-741) loses
  nothing, but the restored local file is never uploaded while the
  tombstone lives: the walk asks for a local delete, and the executor
  refuses it because the file has no index row.
- `AGENTS.md` §4: when the evidence is ambiguous, keeping data wins. A
  modification time is set by the user's tools. It is not evidence
  about which event came first.
- On macOS the rule also covers files that were deleted in the cloud.
  The daemon moves the local copy to the trash, the watcher reports
  that move as `Renamed`, and the echo check misses it (ENG-26). The
  resulting `Delete` intent finds the cloud object gone and records a
  tombstone with origin `Local` over the `Remote` one
  (`executor.rs:2375-2385`, `state_db.rs:1260-1278`). A file restored
  from the Drive trash keeps its modification time and then meets this
  rule. Read only for this part.
- Fix: remove the mtime-against-tombstone rule. The index rule
  (`deleted_here_while_away`) already covers deletions made while no
  daemon ran. In the delete planner, a missing index row must keep the
  cloud object.

### ~~ENG-16 (P2) A local deletion with no index row deletes the cloud object unseen~~

**Done.** Built as D3: no index row means the cloud file stays and comes
down again. A deletion that answers a decision (prefer-local on a type
mismatch) is the one exception, and its expanded children carry the
answer.

- Where: `core/daemon/src/executor.rs:2407-2412`.
- Problem: this is fact 3 of ENG-15 on its own. Any `Delete` intent for
  a path with no sync-index row removes the cloud object, whatever it
  holds. The comment says the code keeps "pre-guard behavior". The
  opposite direction (`deletion_loses_to_local_state`, line 3261)
  preserves a file of unknown provenance.
- Fix: allow the delete only when the cloud object carries this
  device's op-id. Otherwise keep it and enqueue a download.

### ~~ENG-17 (P3) A crash between two renames can strand a local edit in a temp file~~

**Done.** The temp name of a file that is set aside now carries the
file's own name. A walk that finds one gives it back as a conflict copy
and uploads it; bytes that equal the file at the original name are
dropped.

- Where: `core/daemon/src/executor.rs:3936-3955`,
  `core/daemon/src/reconcile_walk.rs:343-352`.
- Problem: the keep-both apply renames the local file to
  `.vapor-tmp-aside-<op>`, then renames the download into place. A
  crash between the two leaves the user's bytes under a hidden temp
  name. After the restart the download runs again and the temp file is
  never examined. The walk removes `.vapor-tmp-*` files whose mtime is
  older than 24 hours. A rename keeps the mtime, so a file last edited
  two days ago is removed at the next walk.
- Fix: at start, turn each `aside` file into a conflict copy.

### ~~ENG-18 (P3) Work on the tick thread that grows with the tree~~

**Done.** In-line hashes read at most 8 MiB; a larger file counts as
changed, which keeps data. The move check reads the queue one time for
each tick, not for each planned deletion (the review missed that the
query already had a limit of 256 rows). The walk queues a download for a
change that only the cloud made. A folder that the walk cannot read is
walked again after 15 minutes.

- Whole-file hashes with no size limit, in line:
  `deletion_loses_to_local_state` (`executor.rs:3279`),
  `child_delete_is_safe` (`:3863`), and the displaced file in the
  keep-both apply (`:3979`). Each runs when sizes match and mtimes
  differ.
- `app_has_pending_upload_of_size` and
  `pending_download_could_be_a_move` (`executor.rs:2311-2343`) load
  every queued path of a kind and stat each one, for each deletion that
  is planned. A rename of a folder with N files costs N x N.
- The walk sends a cloud-only change through the upload planner
  (`reconcile_walk.rs:471-481`). The unchanged local file is hashed in
  full, and the filesystem provider hashes the cloud file twice, before
  the download starts.
- A directory that the walk cannot read is skipped
  (`reconcile_walk.rs:357-374,398-407`). The walk still reports
  success, the compacted boundary is cleared, and nothing schedules the
  retry that the comment promises.

### ~~ENG-19 (P3) The state database has no integrity check and keeps three migrations~~

**Done.** Built as D24: schema version 7, no migration chain. The daemon
sets a database of another version aside and rebuilds from the startup
reconcile. PRAGMA quick_check runs at each daemon start and a damaged
file is set aside. Files set aside are removed after 30 days. A run-time
corruption error ends in a restart, where the page check finds it.

- Where: `core/daemon/src/state_db.rs:168-204,2116-2450`.
- Problem:
  - Corruption is detected only when the open itself fails. A damaged
    page inside a table fails later, at run time, on every start: five
    tick errors, exit, restart, crash-loop pause.
    `docs/architecture/state-schema-migrations.md` says "detect
    corruption early with integrity checks".
  - Quarantined files (`vapor.sqlite.corrupt-<ms>`) are never removed.
  - The code migrates versions 3 to 6, with about 400 lines of tests.
    No user has a version 3 database. `AGENTS.md` §1.1 prefers no
    migration shim before GA; §5 asks for tested migrations. The two
    rules pull in opposite directions here.
- Fix: run `PRAGMA quick_check` after an unclean stop, and quarantine
  on a run-time corruption error. Decide which rule wins for
  migrations, and delete the chain if §1.1 wins.

### ~~ENG-20 (P2) A local root that is a symbolic link reads as missing after the first start~~

**Done.** The presence check follows the link. The unit test starts
three times and the runtime test starts twice.

- Where: `core/daemon/src/root_identity.rs:93-96`,
  `core/daemon/src/runtime.rs:1270-1294`.
- Problem: `check_local_root` tests presence with
  `symlink_metadata(root).is_dir()`. For a symbolic link to a folder
  the answer is false. At start the function gets the configured path,
  before `normalize_watch_root` resolves the link.
  1. First start: no identity is recorded. `create_dir_all` succeeds on
     the link, the log says "Created missing local root", and the root
     is adopted.
  2. Every later start: an identity is recorded and the root is "not
     present". The runtime returns `LocalRootMissing`. The profile
     holds with a `root-missing` decision.
  3. The `recreate` answer does not help. The next start fails the same
     way.
- Verified: a probe test called `check_local_root` two times on a link
  to a directory. The results were `Ready`, then `Missing`. The test
  `start_canonicalizes_symlinked_local_sync_root` covers the first
  start only.
- Fix: resolve the root before the identity check, or use
  `fs::metadata`. Add a second start to the test.

### ~~ENG-21 (P2) Five changed text files in a minute count as a user at the keyboard~~

**Done.** Built as D11: the active-coding heuristic and its constants
are deleted.

- Where: `core/daemon/src/safeguards.rs:86-131`,
  `core/daemon/src/runtime.rs:2584-2590,2841-2842`,
  `core/daemon/src/debounce.rs:336-360`,
  `core/shared/src/constants.rs:1032-1033`.
- Problem: the active-coding heuristic sets `user_active` when 5
  stabilized events of the code, text, or key-config class arrive in
  60 s. The classes include `.json`, `.yaml`, `.csv`, `.txt`, `.md`,
  `.xml`, `.html`, and `.svg`. `user_active` gives `Throttled`.
- The heuristic exists for hosts with no input-idle signal. macOS has
  that signal, and the heuristic applies there too.
- Consequence: a script, an export, a `git pull`, or an app that writes
  JSON state keeps the daemon in `Throttled` with nobody at the device.
  The cloud poll interval goes from 5 s to 60 s, and no reconcile scan
  starts (ENG-01).
- Verified: in the e2e manual sandbox, where the throttle inputs are
  static, a few operations on `.txt` files moved the daemon to
  `Throttled` with the reason "user activity is active".
- Fix: use the heuristic only when the platform reports no idle signal.
  The measured CPU input already protects the device from heavy churn.

### ~~ENG-22 (P3) The auto-tuner changes a value that no longer sets the pace~~

**Done.** Built as D17: the auto-tuner, its constants, and the shared
step are deleted. A session reports its own step size
(TransferSession::step_bytes); Google Drive returns the chunk size it
already tunes, so its range above 16 MiB is reachable.

- Where: `core/daemon/src/auto_tune.rs`,
  `core/daemon/src/provider_jobs.rs:640-700`,
  `core/providers/src/gdrive/mod.rs:69-73,1381-1407`.
- Problem: every 90 s the tuner changes the transfer step between 4 and
  16 MiB. Transfers run on worker threads that call `step` in a loop
  until the transfer ends. The step is now a chunk size. It does not
  set the pace. The module comment still calls it a "per-tick" budget.
  - Under a rate limit the tuner halves the step. A Drive resumable
    upload then sends more requests for the same bytes.
  - The Drive provider has its own chunk tuner (256 KiB to 64 MiB). The
    step caps it, so the range above 16 MiB cannot be reached.
  - The regression signal is completed intents per cycle. That number
    follows the mix of file sizes, not the step.
- Fix: delete the auto-tuner and the shared step. Let each provider own
  its chunk size. If a tuner is wanted, tune concurrency.

### ~~ENG-23 (P3) An ignore-rule change does not sync the files it frees~~

**Done.** Built with D22. One function decides for the watcher and the
walk: a path under an excluded folder cannot be included again (write
`logs/*` and `!logs/keep.txt` to keep one file). A rule that ends in `/`
hides folders only; the walk passes the kind, and the watcher asks the
disk only when such a rule names the path. A rebuild runs on a worker
and the rules in force stay until it is in. A rebuild that changes a
decision queues a whole-scope reconcile. Open: the first build of the
rules still runs while the profile is composed; a task in
docs/tasks/core.md records it. e2e S64.

- Where: `core/daemon/src/fs_events.rs:226-248`,
  `core/daemon/src/runtime.rs:926-934`,
  `core/daemon/src/path_filter.rs:96-145,218-318,470-520`.
- Problem:
  - After an ignore file or a rule key changes, the filter is rebuilt.
    No reconcile is queued. A file that the old rules hid stays
    unsynced until it changes, the daemon restarts, or the user runs
    `vapor reconcile`.
  - The rebuild walks the whole tree for ignore files on the tick
    thread. The same walk runs at start. A `git checkout` that touches
    a `.gitignore` triggers it.
  - A negation under an ignored folder (`vendor/`, then
    `!vendor/keep.txt`) is honoured by the watcher and not by the
    reconcile walk, which never enters `vendor/`. The file syncs only
    when it changes while the daemon runs.
  - A folder rule (`build/`) also hides a file named `build`. The
    default rules have `build/`, `out/`, `dist/`, `target/`, `vendor/`,
    and `coverage/` (see SHR-06).
- Verified: a probe test built the filter with those rules.
  `should_ignore` was true for `vendor`, false for `vendor/keep.txt`,
  and true for a file `notes/build`.
- Fix: queue a whole-scope reconcile after a rebuild that changed the
  rules. Run the ignore-file discovery on a worker. Use one negation
  rule in both places (git's rule: a file under an excluded folder
  cannot be included again). Tell the matcher when a path is a folder.

### ~~ENG-24 (P2) No whole-scope reconcile runs between two starts~~

**Done.** Built as D12: a running daemon queues one whole-scope
reconcile 24 h after the last one, by wall time, when the throttle
admits a scan.

- Where: `core/daemon/src/runtime.rs:2693` (the callers of
  `enqueue_startup_reconstruction_reconcile`),
  `core/providers/src/filesystem/feed.rs:395-399`.
- Problem: a whole-scope reconcile is queued at start, after a watcher
  overflow, after a cursor expiry, after a root returns, and on the
  user's request. There is no cadence. A comment in the feed says so:
  "the next reconcile (which has no periodic cadence)".
- A daemon on a device that sleeps and wakes can run for weeks. Each
  change that the event path drops stays unconverged for that time.
  This review lists several such drops: ENG-18 (a directory that the
  walk skipped), ENG-23 (freed files), ENG-25 (echo records), and
  PRV-04.
- `AGENTS.md` §1 promises "converge eventually". Today that promise
  depends on complete event delivery or on a restart.
- Fix: queue a whole-scope reconcile on a slow cadence (for example
  one time each day, in `Idle` only). The walk uses the quick check, so
  an unchanged tree costs one listing for each directory.

### ~~ENG-25 (P3) Smaller engine findings, second batch~~

**Done.** Built. Trash: the metadata is written before the payload
moves, an entry directory is made with a call that fails when it exists
and the next id is tried, and the purge runs on a worker; an entry with
no payload is not listed and is removed after one hour. Echo records: a
delete record is dropped when the path is seen present again, on both
sides; the write echo already compares size and modification time for
every file. Conflict names: the parser splits at the last marker. The
three stale comments are rewritten, and the type-mismatch question says
what happens when the trash is off. Not changed: a directory deletion
lists the subtree again for each pass; the entries it deleted are gone
from the next listing, so the cost stays linear.

- Managed trash (`trash.rs`):
  - `meta.json` is written after the payload moves (`:273-308`). A
    crash or a full disk between the two leaves an entry that
    `vapor trash list` skips and that `purge_expired` never removes.
    Write the metadata first.
  - The entry id is `<ms>-<counter mod 10 000>`, and the counter starts
    at 0 in each process (`:265-269`). The entry directory is made with
    a call that accepts an existing directory. Two discards with one id
    share a directory: the second `meta.json` replaces the first, and a
    payload with the same name replaces the first payload. Use
    `create_dir` and try the next id on `AlreadyExists`.
  - `purge_expired` removes whole trees on the tick thread
    (`runtime.rs:1082`).
- Echo records (`self_write_cache.rs`, `runtime.rs:3868-3915`):
  - A delete record matches every `Removed` event at its path for
    30 s. It is not consumed. A user who creates the file again and
    deletes it inside that window has the deletion dropped until the
    next reconcile.
  - For a local file over 8 MiB the echo check compares the op-id tag
    and the size, and not the modification time (`matches_write`, not
    `matches_write_checked`). A same-size edit in place, inside 30 s
    after a download, is dropped until the next reconcile.
- Conflict names (`conflict.rs:97-133`): the parser splits at the first
  marker. For a copy of a copy it reports the wrong original and a
  device id that holds the first copy's suffix. Verified with a probe
  test. Split at the last marker.
- `type_mismatch.rs:22-47` says the local item "goes to the trash"
  also when the trash is off. `conflict.rs` words both cases.
- `safeguards.rs:11-17` says the mass-delete guard "pauses the daemon"
  and that `vapor resume` resets it. The guard now holds the batch
  behind a decision.
- `ipc_service.rs:3-6` has a broken sentence ("`//!, and the control
  endpoints`") and names "schema-v2". The supported window is 4 to 4.
- A directory deletion lists up to 10 000 cloud entries for each pass
  (`provider_jobs.rs:133`, `executor.rs:2458-2560`). The cap is safe
  (the directory intent comes back), and each pass lists the subtree
  again from the start.

### ~~ENG-26 (P0) After a keep-both conflict, the daemon deletes the winning version from the cloud~~

**Done.** The delete planner drops a deletion when the file is on this
device again, the echo check matches a delete record by the state of the
path, and a deletion with no index row keeps the cloud file (D3). Two
runtime tests replay the daemon's own renames and fail without the fix.
The e2e scenario comes with TST-01.

- Where: `core/daemon/src/runtime.rs:3868-3878` (echo check),
  `core/daemon/src/scheduler.rs:327-340` (intent kind),
  `core/daemon/src/executor.rs:2240-2300,2354-2412` (delete planner),
  `core/daemon/src/executor.rs:3206-3219` (conflict rename),
  `core/platform/src/fs_watch/notify_backend.rs:196-212`.
- Facts:
  1. In a keep-both conflict the daemon renames the local file to a
     conflict copy. It records a delete echo for the original path,
     removes the index row, and queues a download of the cloud version.
  2. On macOS, FSEvents reports a rename-away as `Renamed`. For a file
     that changed in the last seconds, the last event is `Modified`.
     The echo check matches a delete record only when the last event
     kind is `Removed`. The daemon does not recognise its own rename.
  3. When the download does not land inside the debounce window (0.9
     to 4 s), the path is missing at stabilization. The event becomes a
     `Delete` intent for the original path.
  4. The delete planner never checks that the local file is still
     absent. With no index row it deletes the cloud object with no
     comparison (ENG-16). With an index row, the row is the one that
     the download wrote a moment before, so the cloud object "is
     unchanged" and is deleted.
- Result: the cloud loses the version that won the conflict. Each other
  device receives a deletion of the file. When the download had not
  started (a retry after a network error), no copy of that version
  reaches this device.
- Verified, three ways:
  1. A probe with the real watcher: a move out of the watched folder
     arrives as `Renamed`, an unlink as `Removed`. For a file created
     seconds before, both sequences end with `Modified`.
  2. A probe with the runtime test fixture: the download was blocked
     for 10 s with transient errors, and one `Renamed` event was
     recorded for the original path. The run ended with the original
     gone on both sides, `queue 0`, `failed 0`. The other device's
     version existed nowhere.
  3. End to end in the e2e manual sandbox (real `vapord`, real
     FSEvents, filesystem provider, bandwidth ceiling at 1% so that a
     1.2 MB download took about 10 s). 30 s after a concurrent edit the
     cloud folder had lost `doc.txt`. The local folder kept it.
     `verify-trees` reported the difference. `vapor status` showed
     `queued 0, failed 0`. `vapor reconcile` uploaded the file again.
- Why no test found it: the tick-harness tests record events by hand
  and never record the daemon's own renames. The e2e conflict scenarios
  use small files, so the download lands inside the debounce window.
- Fix:
  1. In the delete planner, complete a `Delete` intent as a no-op when
     the local path exists. `ApplyRemoteDelete` has the mirror guard
     (`index.updated_at > intent.enqueued_at`).
  2. Match a delete echo by the record and the state of the path, not
     by the last event kind.
  3. Remove the `None => true` branch (ENG-16).
  4. Add an e2e scenario: a conflict where the download outlasts the
     debounce window (a low bandwidth ceiling does it).

### ~~ENG-27 (P1) A file removed and put back within seconds is deleted from the cloud~~

**Done.** Fixed by the same planner check as ENG-26. A runtime test
moves a file out for 3 s and back.

- Where: `core/daemon/src/executor.rs:2240-2300,2354-2412`.
- Problem: a removal that stays for longer than the debounce window
  becomes a `Delete` intent. Its first planning waits 3 s
  (`MOVE_SETTLE_DELAY_SECONDS`). When the file is back by then, the
  planner does not look. The upload of the returned file finds
  identical content and does nothing. Then the delete runs.
- Cases: an undo in the Finder a few seconds after a delete, and an app
  that replaces a file with a delete and a slow create.
- Result: the file exists on this device and not in the cloud. Each
  other device moves its copy to its trash. The difference stays until
  a restart or `vapor reconcile` (ENG-24).
- Verified:
  1. Runtime fixture probe: move out, 3 s, move back. End state: local
     file present, cloud file gone, `queue 0`.
  2. e2e manual sandbox: the same steps with real binaries. The cloud
     copy was gone 8 s later and stayed gone. `verify-trees` reported
     the difference. `vapor reconcile` repaired it.
- Fix: ENG-26, fix 1.

### ~~ENG-28 (P1) A transfer that waits for bandwidth spins the tick loop on a full core~~

**Done.** A dry bucket now returns the time until a useful grant, and
the executor holds the session until then (four resumes a second at
most). A transfer asks the bucket only for the bytes it still has to
move. The poll-permission line is logged on change only. e2e scenario
S59 measured 134% of a core on the base commit and 3% now.

- Where: `core/daemon/src/provider_jobs.rs:640-665` (a grant of 0
  returns `TransferHeld` and wakes the tick),
  `core/daemon/src/executor.rs:711-770` (the next tick dispatches the
  session again), `core/providers/src/bandwidth.rs:44-58`.
- Problem: when the token bucket is empty, the worker hands the session
  back and wakes the tick. The tick dispatches it again at once and
  does not ask the bucket. The loop repeats until the transfer ends.
  Each pass moves the few bytes that the bucket refilled.
- Verified in the e2e manual sandbox (debug build, filesystem
  provider):
  - Default ceiling (25%, 3.1 MB/s): a 60 MB upload took about 20 s.
    The daemon used 98 to 99% of a core for that time (`ps`). Idle use
    was 0.3%.
  - Ceiling at 1%: about 6 700 ticks each second, 85 to 115% CPU.
  - `vapor status` showed `cpu.limitPercent: 15` during both (ENG-08).
- Scope: a provider whose step is faster than the bucket refill. For
  the filesystem provider that is each file above about 3 MB at the
  defaults. A network provider spends the wait inside the request. Not
  measured with Google Drive.
- Side effect: at DEBUG, "Evaluated remote polling permission" is
  written on each tick. After two minutes the three 8 MiB log files
  held that line and nothing else. A failed e2e or soak run loses its
  log history this way.
- Fix: make the bucket return the wait time, and keep the session held
  (or sleep in the worker) until a useful grant is available. Log the
  poll-permission line only when the answer changes.

### ~~ENG-29 (P0) A case-only rename removes the file from the cloud, and can remove it from both sides~~

**Done.** Built as D1 and D2. The engine now keys every check by the
exact spelling that the filesystem stores (one shared helper,
vapor_shared::paths::stored_spelling). The walk pairs a local-only and a
cloud-only name that fold together and settles the spelling: the sync
index says which side changed it, and with no evidence the cloud's
spelling stays. A local change travels through the upload route's move
detection; a cloud change renames the local entry and re-keys its index
rows. The filesystem provider no longer answers for a spelling it does
not store. Files and folders, with the daemon running and stopped. Seven
runtime tests and e2e scenarios S56 and S57 fail on the base commit and
pass now. Google Drive is not checked live.

- Where: `core/daemon/src/reconcile_walk.rs:330-460` (entries are
  paired by exact name; `materialize_colliding_remote`),
  `core/daemon/src/name_collision.rs`,
  `core/daemon/src/executor.rs:3035-3070` (local rename in place of a
  download), `core/daemon/src/remote_sync.rs` (collision on a remote
  change).
- Problem: on a volume that folds case, `report.txt` and `Report.txt`
  are one file. The engine treats the two spellings as two files that
  collide. A rename that changes only the case is a common action on
  macOS.
- Verified in the e2e manual sandbox (real binaries, filesystem
  provider, both roots on the default case-insensitive volume). Two
  synced files, then two renames at the same moment:
  1. In the cloud folder, `notes.txt` to `Notes.txt`. The daemon logged
     "Remote change collides with a differently-cased local file" and
     ran a whole-scope reconcile. It renamed the local `notes.txt` to
     `Notes~conflict-<device>-<ms>.txt` and deleted `Notes.txt` from
     the cloud. The copy was not uploaded: its event was suppressed as
     an echo.
  2. In the local folder, `report.txt` to `Report.txt`. The walk saw
     the cloud `report.txt` as a second file that collides with the
     local `Report.txt`. It renamed the local file to
     `report~conflict-<device>-<ms>.txt` and deleted the cloud file.
     About 45 s later it applied a cloud deletion to the copy and moved
     the copy to the managed trash.
  3. End state after 80 s: the cloud folder was empty. The local folder
     held one conflict copy. The second file existed only in the
     managed trash, which purges after 7 days. Status showed
     `queued 0, failed 0, decisions 1`.
- Not verified with Google Drive, where the cloud keeps case. By the
  code, a local case-only rename uploads a second object and never
  deletes the first one, because the old spelling still resolves
  locally.
- Fix: in a directory where the local filesystem folds names, pair the
  local and the cloud entry by the folded name and treat a difference
  in spelling as a rename. Carry it as a move in the direction that
  made it. Keep the collision path for two cloud objects that fold to
  one name. Add one e2e scenario for each direction.

### ~~ENG-30 (P2) A deletion that the daemon makes in the cloud leaves no INFO line and no timeline entry~~

**Done.** Each cloud deletion writes an INFO line with the path and the
cause, and the timeline has a cloud-delete kind.

- Where: `core/daemon/src/executor.rs` (the `RemoteDelete` completion
  path), `core/daemon/src/timeline.rs`.
- Problem: the executor logs a remote deletion at DEBUG only. The
  timeline gets no entry. A local removal has an INFO line ("Moved a
  file Vapor removed on this device to the trash").
- Verified: in the sandbox runs for ENG-26, ENG-27, and ENG-29 the
  daemon deleted cloud files. The INFO log and `vapor timeline` had no
  line that named a cloud deletion.
- `AGENTS.md` §11 asks that observability explains the current state
  and the reason. A user who finds a file gone from the cloud cannot
  learn from Vapor that Vapor removed it, or why.
- Fix: log each cloud deletion at INFO with the path and the cause (a
  local deletion, a mirror, a directory expansion), and add a timeline
  kind for it.

## Providers: `core/providers` (`PRV`)

The Google Drive leg of the e2e suite has never run
(`docs/tasks/core.md`, TR-12). The Drive findings below come from the
code and the scripted tests. They need a check against a real account.

### ~~ENG-31 (P0) A file that is gone because the local root was swapped is deleted from the cloud~~

**Done.** Found while the fixes were built, and fixed in the same
change. The root is read before any work is leased and again right
before each cloud deletion.

- Where: `core/daemon/src/runtime.rs` (`process_ready_queue`,
  `check_roots_if_due`), `core/daemon/src/executor.rs` (the
  `RemoteDelete` plan outcome).
- Problem: the daemon checks every 15 s that the local root is the
  folder the profile adopted. A watcher event for the root itself
  starts that check at once. When the root is swapped (a volume
  unmounted, a folder put in its place) and the watcher reports a
  file's removal before it reports the root, or does not report the
  root, the Delete intent runs inside those 15 s. The cloud copy is
  removed.
- Verified: e2e S43 failed one time in this way under load. The log
  shows "Removed from the cloud ... cause=it was deleted on this
  device" 6.8 s before "Sync root replaced". A runtime test delivers
  only the file's event after a swap: it fails without the fix.
- Fix: `root_identity::local_root_is_adopted` reads the marker. The
  ready queue asks it before it leases anything, and holds the profile
  when the answer is no. The delete path asks it again at the last
  moment before the provider call, after the absence was observed.
- Not covered: the same window on the cloud side. A cloud root that is
  swapped is found by the 15 s probe. A local file that a cloud
  deletion removes in that window goes to the trash.

### ~~ENG-32 (P2) In push-only, a spelling that the cloud changed is not put back~~

**Done.** Found by the new soak operation (a rename that changes only
the case), and fixed in the same change.

- Where: `core/daemon/src/remote_sync.rs` (the push-only branches of
  the feed mapping), `core/providers/src/filesystem/feed.rs`
  (`normalize_watch_event`).
- Problem: in `push-only` the device is the source of truth. The cloud
  renames `report.txt` to `Report.txt` on a folder that folds case. The
  feed maps the change to a restore through the cloud's name. That
  name is not on this device, so the restore plans to nothing. The
  cloud keeps the wrong spelling until the next whole-scope scan, which
  can be a day away. A later change of the same file then queues an
  upload that the provider refuses for as long as the other spelling is
  there.
- Also: the filesystem feed reported an event under the spelling of
  the event, and not under the spelling the folder stores. After the
  rename, the event for the old name read as a change of an object
  that is not there.
- Verified: a push-only soak cell (seed 9, 5 minutes) did not converge;
  one upload stayed in `retrying` with "the cloud folder holds ...
  under another spelling". A runtime test fails without the fix.
- Fix: in `push-only`, a cloud change whose name this device stores
  under another spelling asks for a reconcile of that folder. The walk
  already removes the cloud's spelling and sends the local one. The
  feed reports each change under the stored spelling.

### ~~PRV-01 (P1) After a Drive upload over 5 MiB, the next edit of that file cannot upload~~

**Done.** Built from the Drive API documents (D34): a method returns
only its default fields when the request sets no fields parameter. The
request that opens a resumable session now asks for the checksum and the
modified time. The session also hashes the bytes it sends and reports
that hash when the final answer has none. The planner treats an empty
hash in the sync index as not known: it guards the overwrite with the
hash the object has now. Not checked against a real Drive account; that
check stays with the owner.

- Where: `core/providers/src/gdrive/mod.rs:1352-1379,1473-1486`,
  `core/daemon/src/executor.rs:2819-2830`.
- Facts:
  1. A resumable session starts at `...?uploadType=resumable` with no
     `fields` parameter. The multipart path asks for
     `fields={FILE_FIELDS}`.
  2. Without `fields`, Drive v3 answers with `kind`, `id`, `name`, and
     `mimeType` only. The final response then has no `md5Checksum` and
     no `modifiedTime`.
  3. The session reports `content_hash: unwrap_or_default()`, an empty
     string, and no remote mtime. The sync index stores both.
  4. At the next local edit the planner sees its own op-id on the
     object and picks `HashEquals("")`. `begin_upload` compares the
     real MD5 with `""` and answers `PreconditionFailed` in the begin
     phase. The executor retries that as transient, up to 10 000 times.
- Verified: facts 1, 3, and 4 by reading. The scripted test supplies
  `md5Checksum` in the final response
  (`core/providers/src/gdrive/tests.rs:284`), so it cannot see this.
  Fact 2 is Drive's documented default and was not run.
- Fix: add `fields` to the session request. Compute the MD5 while the
  chunks are read and use it when the response has none. Treat an empty
  hash in the index as "unknown", never as a value to compare.

### ~~PRV-02 (P2) A full Drive is a permanent failure for each file~~

**Done.** A full cloud is its own error kind. The upload goes back to
the queue with no attempt spent and is tried again after 15 minutes.
Status says that uploads wait and why; downloads and deletions go on.
The filesystem provider reports a full volume the same way.

- Where: `core/providers/src/gdrive/mod.rs:1655-1657`.
- Problem: `storageQuotaExceeded` maps to `Permanent`. Each upload is
  moved to `failed_intents` with an ERROR line. The message says "free
  space ..., then sync resumes", but nothing retries a failed intent.
  `docs/architecture/data-flow.md:318` says an out-of-quota cloud makes
  the profile wait and retry.
- Fix: hold the profile, as the sign-in hold does, and probe for space.

### PRV-03 (P2) Two Drive files with one name in one folder

**In progress.** A path stands for the file with the lowest id among all
files of that name, in a lookup and in a listing, whatever order Drive
answers in. A change of the other file cannot rebind the path. The file
that is left in the cloud is named in one WARNING. Not built: a
conflict-copy alias that brings the second file to the device; a task
records it.

- Where: `core/providers/src/gdrive/mod.rs:452-472,848-895`.
- Problem: Drive allows it, and its web upload creates it. `enumerate`
  returns both entries under one path, and the walk keeps the last one.
  `find_child` asks for two results and takes the lower id, so with
  three files the choice depends on the order Drive returns. The
  changes feed maps each id to the same path and overwrites the cached
  id. The path can therefore bind to different files at different
  times. One file never reaches the device.
- Fix: give the second file a conflict-copy alias, as the engine
  already does for names that differ only by case.

### PRV-04 (P2) Drive deletions are dropped by the feed after every restart

**In progress.** Not built as written: the Drive file id is not in the
sync index. The effect is smaller now: the startup whole-scope scan runs
under every throttle state but Suspended (D8), and it finds a deletion
that the feed dropped. A task records the index change.

- Where: `core/providers/src/gdrive/mod.rs:1077-1092`.
- Problem: a removed change carries only a file id. The id-to-path map
  lives in memory and is empty after a restart. The change is skipped
  with a DEBUG line "reconcile owns that convergence". The reconcile
  needs `Idle` (ENG-01). Until it has listed the tree, a deletion made
  on another device does not reach this one.
- Fix: store the Drive file id in the sync index.

### ~~PRV-05 (P2) Each Drive path lookup walks the whole parent chain again~~

**Done.** A folder id that a request confirmed stands for 60 seconds. A
path lookup no longer confirms every folder above it again, and an
upload into a folder that was just used asks nothing about its parents.
The changes feed drops a folder and everything under it from the cache
when it reports the folder changed.

- Where: `core/providers/src/gdrive/mod.rs:479-554,558-580`.
- Problem: a cached id costs one GET, then `cached_hit_still_matches`
  resolves the parent, which does the same up to the root. A stat of a
  file five folders deep is six requests. `ensure_parent_id` does not
  use the cache: one list query for each folder, for each upload. One
  small upload into a deep folder costs about fifteen requests.
- Fix: trust cached folder ids for a short time and let the changes
  feed evict them.

### ~~PRV-06 (P3) Smaller provider findings~~

**Done.** A download is compared with Drive's checksum and read again
when it does not match. The request body is shared across the one retry,
and a small upload no longer keeps a third copy of the file. One token
refresh runs at a time. A busy token endpoint is tried again and is not
a lost sign-in. The scripted transport is compiled for tests only. The
documentation stage covers the rest: Google Docs are skipped, no proxy
setting is read, and the error list in the provider documents.

- Google Docs, Sheets, Slides, and shortcuts are skipped
  (`gdrive/mod.rs:64-66`). No user document says so and no status
  counts them.
- A download does not compare its MD5 with Drive's `md5Checksum`
  (`gdrive/mod.rs:1596-1609`). Range requests are not pinned to a
  revision, so a file replaced during the download gives mixed content
  until the feed reports the change.
- `execute_authed_with` clones the request body for each attempt
  (`gdrive/mod.rs:1211`). `simple_multipart` holds the file, the
  multipart body, and that clone at once: three copies of up to 5 MiB
  for each small upload.
- Several workers that find the token near expiry each call the token
  endpoint (`gdrive/mod.rs:169-225`). Nothing makes the refresh single.
- A token-endpoint error below 500 that is not `invalid_grant`
  (`temporarily_unavailable`, a rate limit) holds the profile for a
  new sign-in (`gdrive/oauth.rs:273-281`).
- The transport uses rustls with built-in roots and reads no proxy
  setting (`ureq` default features). A device behind a TLS-inspecting
  proxy cannot sync.
- `ProviderErrorKind::CloudRootUnavailable` is missing from the error
  lists in `docs/architecture/provider-onboarding.md:32-37` and in the
  `vapor-provider` skill. The same document names one capability both
  `supports_server_side_rename` and `supports_server_side_move`.
- `ScriptedHttpTransport`, a test double, is compiled into the shipped
  library (`core/providers/src/http.rs:171`).

### ~~PRV-07 (P3) A name that the path model refuses gets no decision~~

**Done.** A local name with a backslash goes through the unsyncable-name
decision, from the walk and from a watcher event, and is never a failed
intent. A folder with such a name is not entered. A cloud name that
cannot be a sync path is reported one time and not at every listing; the
engine does not see it, so no decision is asked for it.

- Where: `core/providers/src/paths.rs:62-66,110-124`,
  `core/daemon/src/executor.rs:2069-2079`,
  `core/daemon/src/reconcile_walk.rs:273-283`,
  `core/providers/src/gdrive/mod.rs:873-884`,
  `core/providers/src/filesystem/mod.rs:465`.
- Problem: `RemotePath` refuses a backslash. macOS allows `\` in a file
  name, and a Drive name can hold `/`.
  - A local file with `\` in its name: the upload fails for good with
    "is not inside the local sync root". Status counts it as `failed`.
  - A local folder with `\` in its name: the walk logs a WARNING
    ("outside the scope root") on each pass. Its content never syncs.
  - A Drive entry with `/` in its name, and a name that is not UTF-8 in
    a filesystem cloud folder: one WARNING for each listing.
- The `unsyncable-name` decision exists for this case. It covers only
  local names that are not UTF-8.
- Fix: send all of these through `unsyncable-name` with the true
  reason, and log each name one time.

### ~~PRV-08 (P3) Three capability flags have no reader, and the inert stub accepts every write~~

**Done.** The three flags that nothing read are removed; write
preconditions are a requirement that the contract suite tests for every
provider. A profile that does not run gets a provider that refuses every
call. The documentation stage corrects the onboarding document.

- Where: `core/providers/src/lib.rs:108-143,445-545`,
  `core/daemon/src/remote_sync.rs:202`,
  `core/daemon/src/executor.rs:3434`,
  `docs/architecture/provider-onboarding.md:44-58`.
- Problem:
  - The engine reads `supports_remote_changes_feed` and
    `supports_server_side_move`. Nothing in `core/daemon` reads
    `supports_write_preconditions`, `supports_op_id_tags`, or
    `supports_content_hashes_in_metadata`. The onboarding document says
    the engine plans around all of them. A provider that sets
    `supports_write_preconditions: false` still gets `HashEquals`
    preconditions.
  - The document names two flags that do not exist
    (`supports_server_side_rename`, `supports_content_hash_in_listings`).
  - `FilesystemStubProvider` stands in for a suspended profile. It
    answers `enumerate` with an empty list, `delete` with success, and
    an upload with a finished session. Safety rests on the rule that a
    suspended profile never ticks (`multi_runtime.rs:1147-1153` says
    why).
- Fix: make the engine read each flag or remove the flag. Make the stub
  fail closed: each call returns an error.

## IPC: `core/ipc` (`IPC`)

### ~~IPC-01 (P3) The IPC contract describes behaviour that the code does not have~~

**Done.** The daemon logs a refused handshake at INFO with both
versions. The claims the code does not keep (the unknown-field log, the
deprecated flag, chunked frames, the Shutting Down frame) leave the
documents in the documentation stage.

- Where: `docs/architecture/ipc-contracts.md:118-124,129-135,144-148,176-177`,
  `docs/architecture/macos/ipc-transport.md:49-56`,
  `core/ipc/src/server.rs`, `core/daemon/src/ipc_server.rs:157-170`.
- Problem:
  - "Both sides log `ipc.handshake.incompatible`." The daemon drops the
    result of `serve_connection` (`let _ =`). No handshake failure
    reaches the log, and no log line has that name.
  - "Unknown fields are logged at debug level (`ipc.unknown_field`)."
    `serde` drops them with no log.
  - "Mark the field `deprecated = true` in the schema." There is no
    schema file. The Rust types are the schema.
  - "Streamed endpoints use chunked frames." Each response is one
    frame.
  - "Returns a `Shutting Down` error frame." No such error exists. On
    SIGTERM the process exits and an open session sees end-of-file.
- Fix: remove each claim or build it. Log a refused handshake at INFO
  with both versions: it is the one line that explains "the app cannot
  talk to the daemon" after an update.

### ~~IPC-02 (P3) A large `timelineLimit` breaks `vapor timeline`~~

**Done.** timelineLimit is at most 10 000 in the CLI and in the daemon.
A timeline response keeps the newest entries that fit one frame and sets
truncated, as Diagnostics does.

- Where: `core/daemon/src/timeline.rs:53-59`,
  `core/daemon/src/ipc_service.rs:227-251`,
  `core/ipc/src/framing.rs:92-99`.
- Problem: `timelineLimit` has no upper bound. The whole timeline is
  one frame, and a frame holds 4 MiB. With a limit of some tens of
  thousands, the response passes 4 MiB after enough activity.
  `write_frame` then returns an error and the session closes. The
  client reports an unexpected end-of-file. Read only.
- Fix: cap `timelineLimit`, or cut the response to what fits and set a
  `truncated` flag as `Diagnostics` does.

### ~~IPC-03 (P3) Smaller IPC findings~~

**Done.** A missing method is found from the frame's shape: a Call that
names a method this build does not know. An unknown envelope kind and a
malformed frame are Backend errors. A connection slot is given back by a
drop guard. The module comment names the PascalCase envelope, and no
comment dates a field by a schema version. Not built (an idea): a
profile argument for Pause, Resume, and Reconcile.

- `server.rs:186` finds an unknown method through the text "unknown
  variant" in the `serde` error. An unknown envelope `kind` gives the
  same text and gets `MethodNotFound("<unknown>")`.
- `protocol.rs:3-4` says payloads use camelCase keys and kebab-case
  values. The envelope tags, the method names, and the error codes are
  PascalCase (`Hello`, `Call`, `Status`, `Ok`, `MethodNotFound`).
- Comments in `protocol.rs` and `ipc_service.rs` date fields by schema
  v1 and v2. The supported window is 4 to 4.
- `ipc_server.rs:161-176` counts connections by hand. A handler that
  panics in a build that unwinds leaks one of the 32 slots. A drop
  guard removes the case.
- `Pause`, `Resume`, and `Reconcile` carry no profile. Each acts on all
  profiles. (Idea: a profile argument.)

## Lifecycle and service: `core/lifecycle`, `core/platform/service` (`LIF`)

### ~~LIF-01 (P2) The headless supervisor starts a daemon that the user stopped~~

**Done.** The manager reads the durable state at the start of each
health check and adopts it when another process wrote it. A test with
two managers on one state store fails without the fix.

- Where: `core/cli/src/main.rs:856-870`,
  `core/cli/src/commands/service.rs` (`check_loop`),
  `core/lifecycle/src/manager.rs:168-205,372-434`.
- Problem: `vapor service check --loop` builds one
  `DaemonLifecycleManager` and keeps it. The manager reads
  `lifecycle.json` only when it is built. Each other `vapor service`
  command is a new process that writes the file. The loop does not see
  the change.
  - After `vapor service stop`, the loop still holds
    `daemon_should_be_running = true`. Its next tick, 30 s later at
    most, counts a crash, starts the daemon, and writes its old state
    over the file.
  - After `vapor service acknowledge`, the loop still holds the pause.
- Verified: a probe test with two managers on one state file and the
  in-memory installer. After `stop` through the second manager,
  `check_daemon_health` on the first returned `RestartedAfterCrash`.
  The file then held `daemon_should_be_running: true` and
  `consecutive_crashes: 1`.
- The macOS app is not affected: it runs `vapor service check` as a new
  process for each tick.
- Fix: read the state from the store at the start of each
  `check_daemon_health`.

### ~~LIF-02 (P3) Smaller lifecycle and service findings~~

**Done.** A counted crash is saved before the restart is tried. On macOS
an install is skipped when the definition is unchanged and the job is
loaded, a start does not kill a running instance and loads a job that
launchd does not know, and a stop waits for the process to go. The
crash-loop numbers are in constants.rs, lifecycle.json uses camelCase
keys, and the comments about Swift code are rewritten. The launchd paths
cannot run on a contributor machine; the macOS CI job checks them. Not
changed: launchctl print is still matched as text (launchd offers no
stable format), and the backoff schedule note goes into the policy
document.

- `manager.rs:398-402`: when `start_daemon` fails in the `NoDelay`
  branch, the function returns before it saves. A one-shot process
  loses the crash it counted. Each later check sees a "first" crash and
  tries again. Backoff and pause never start.
- `service/macos.rs:259-262`: `status` returns `Stopped` when
  `launchctl print` fails, which is the case for a job that is not
  loaded while its plist exists. `start_daemon` uses `kickstart`, and
  `kickstart` cannot start a job that is not loaded. The health check
  then fails on each tick until the next install. Read only.
- `service/macos.rs:205-221,224-226`: `install_and_enable` boots the
  job out and in on each call, and `start_daemon` uses `kickstart -k`,
  which stops a running instance first. `vapor service bootstrap` (the
  app-start path) therefore stops a healthy daemon, lets `RunAtLoad`
  start it, stops it again, and starts it again. `vapor service start`
  restarts a daemon that runs. Read only: an agent must not install a
  LaunchAgent on a contributor machine. Fix: skip the install when the
  plist is unchanged and the job is loaded, and start without `-k`.
- The backoff delays (2, 4, 8 s) are shorter than the supervision
  cadence (30 s). The schedule in
  `docs/operations/macos/launchagent-policy.md` cannot be observed. The
  real delay is one or two ticks.
- `launchctl print` output is matched by substring
  (`"state = running"`, `"pid = "`). Apple does not keep that output
  stable.
- The crash-loop policy numbers are literals in
  `crash_loop.rs:64-72`. `AGENTS.md` §8.6 puts runtime defaults in
  `constants.rs`.
- `lifecycle.json` uses snake_case keys (`durable.rs:36-55`). Every
  other JSON file and wire shape uses camelCase.
- Comments point at Swift code that is gone: `crash_loop.rs:3,62,163`,
  `manager.rs:5-10` and each "Mirrors ..." line,
  `service/macos.rs:3-4`.

### ~~LIF-03 (Idea) Measure what `ProcessType=Background` costs~~

**Done.** Measured with taskpolicy -b, which applies the same background
band, on an idle Apple M2 Pro: SHA-256 of 768 MiB took 6.5 to 7.0 s
against 2.4 to 2.5 s (about 110 MiB/s against 310 MiB/s), and a 768 MiB
copy 0.55 s against 0.30 s. The policy document records it in the
documentation stage. Whether the job stays Background is the owner's
decision.

- Where: `core/platform/src/service/macos.rs:119`,
  `docs/operations/macos/launchagent-policy.md:58-59`.
- Problem: launchd applies its background resource policy to a job with
  this key. The policy lowers CPU and disk priority for the whole
  process. Idle boost raises Vapor's own ceilings and cannot lift that
  policy. Not measured.
- Fix: measure hash and transfer throughput with `Background` and with
  `Standard`, on an idle device, and record the result in the policy
  document.

## Platform, CLI, and secrets (`PLT`)

The service installer and the lifecycle crate are in the `LIF` section.

### ~~PLT-01 (P1) No surface warns before a one-way mode is enabled~~

**Done.** Built as D6. A profile in pull-only or push-only holds behind
a one-way-mode decision and syncs nothing, in either direction, until
the user answers enable. The answer is recorded for that mode and that
pair of roots; a change of either asks again, and a return to two-way
withdraws the question. vapor config set syncMode prints what the mode
does and that the daemon asks first. A hand-edited file starts no
mirror. e2e scenario S62 fails on the base commit and passes now. The
app shows the decision through the same list as the other decisions; it
has no control for the mode yet.

- Where: `core/cli/src/commands/config.rs:91-102,243-245`.
- Problem: `vapor config set syncMode pull-only` prints "syncMode
  saved; restart the daemon to apply it". No text says that the other
  side is overwritten and deleted. The daemon logs nothing special at
  start. The app has no control for the mode. Three documents require
  the warning: `AGENTS.md` §4 ("must surface an up-front data-loss
  warning before activation"), `docs/architecture/sync-modes.md:141-147`,
  and `SECURITY.md:86-91`, which calls a way in without the warning a
  finding.
- Fix: print the warning and ask for confirmation (`--yes` for
  scripts) when the value changes to a one-way mode. Log a WARNING when
  a profile starts in one.

### ~~PLT-02 (P3) Each update of an ad-hoc signed build breaks keychain access~~

**Done.** A background process reads the keychain with
kSecUseAuthenticationUIFail: a read that would open a dialog fails, and
the profile holds for a sign-in with the reason. The keychain contract
test runs with that mode too. The document for prereleases is in the
documentation stage.

- Where: `core/platform/src/secrets/macos.rs:290-325`,
  `docs/operations/provider-auth-operations.md:88-94`.
- Problem: the access list names binaries by their code identity. An
  ad-hoc signed binary is identified by its hash. Every prerelease is
  ad-hoc signed, so each update makes the daemon's first token read
  prompt, or fail when no prompt is possible. The document describes
  this for development builds only.
- Related: the daemon reads the keychain with the default
  authentication UI, on worker threads and once during start. A prompt
  blocks that call until the user answers.
- Fix: document it for prereleases. Read with
  `kSecUseAuthenticationUIFail` in the daemon so a refused read becomes
  the sign-in hold.

### ~~PLT-03 (P3) Secret names are not scoped to the runtime directory~~

**Done.** Every runtime directory other than the default one has its own
scope on each secret name (dir-<hash>), through one wrapper on every OS.
A development daemon and a test sandbox cannot read, replace, or delete
the sign-in of the real install.

- Where: `core/shared/src/constants.rs` (`secrets::STORE_NAMESPACE`),
  `core/daemon/src/profiles.rs` (`secret_key`).
- Problem: the keychain account is `auth.<profile>.<provider>.token`.
  A development daemon on `./.vapor` and the real install on `~/.vapor`
  read the same token for the same profile id. The e2e harness works
  around this by refusing binaries that carry an OAuth client.
- Fix: add the `vapor_dir` hash to the account name.

### ~~PLT-04 (P3) `vapor auth login --token <value>` takes a credential on the command line~~

**Done.** A value given to --token is refused and never stored; the
token comes from stdin (--token -). Status, logout, and doctor ask the
store for names only, so no secret is read and no dialog opens. A
credential that cannot be removed is an error.

- Where: `core/cli/src/main.rs:232-235,780-811`.
- Problem: the value is visible in the process list and in the shell
  history. The command prints a warning and continues. `SECURITY.md:54-56`
  lists a token in an argv as a real finding.
- Also: `logout_from` treats any read error as "nothing stored"
  (`core/cli/src/commands/auth.rs:127-140`), and `vapor auth status`
  reads the secret value to answer "signed-in", which can prompt.
- Fix: accept the token on stdin only.

### ~~PLT-05 (P2) No surface checks the sync roots or explains a denied macOS privacy permission~~

**Done.** vapor doctor has a profiles check (no profile yet, or what
stops the file) and one check for each profile: the local root lists and
takes a write, the filesystem cloud root lists, and a provider with an
account is signed in. A local root that the daemon cannot read becomes
the run reason, with the System Settings pane for a permission error. A
root marker that cannot be read no longer opens a root-replaced
decision. The README text comes with DOC-11. The cloud root of a network
provider is not probed (no network in doctor).

- Where: `core/cli/src/commands/doctor.rs:111-128`,
  `core/daemon/src/reconcile_walk.rs:357-407`, `README.md`, `docs/`.
- Problem: macOS asks for consent before a process reads `~/Documents`,
  `~/Desktop`, `~/Downloads`, iCloud Drive, or a removable or network
  volume. `vapord` is a background process. When the user refuses, each
  read fails with a permission error. The walk then skips each
  directory with a WARNING and reports success (ENG-18). Status shows a
  running, idle profile.
- `vapor doctor` checks the runtime directory, the socket, the binary,
  the secret store, the OAuth client, the throttle inputs, and the
  plist. It does not check that a profile's local root can be listed
  and written, that the cloud root answers, or that the profile is
  signed in.
- No document names these permissions. `AGENTS.md` §6: a permissioned
  feature must degrade safely when denied.
- Read only. The packaged app was not run.
- Fix: add one doctor check for each profile (list the root, create and
  remove a temp file, probe the cloud root). Turn a permission error on
  the root into a run reason that names the System Settings pane.
  Describe the prompt in the README.

### ~~PLT-06 (P3) `vapor trash empty` asks nothing, and the trash of a disabled profile is out of reach~~

**Done.** vapor trash empty asks on a terminal and needs --yes
elsewhere. list, restore, and empty reach the trash of every profile on
disk, also a disabled one and one that left vapor.json. The daemon
purges the trash of profiles that do not run, with the same retention.
vapor profiles remove also removes what is left of a profile that the
file no longer names.

- Where: `core/cli/src/commands/trash.rs`, `core/cli/src/main.rs`
  (`TrashAction::Empty`), `core/daemon/src/runtime.rs:1082`.
- Problem:
  - `vapor trash empty` deletes each entry for good, with no
    confirmation and no `--yes` flag. The trash is the last copy in
    cases such as ENG-29.
  - `list`, `restore` without `--profile`, and `empty` without
    `--profile` look at enabled profiles only. The daemon purges only
    the profiles that run. The trash and the state database of a
    disabled or removed profile stay on disk with no retention.
  - `vapor trash restore` works with the daemon stopped and keeps the
    old modification time. That is one way to reach the second case of
    ENG-15.
- Fix: ask for confirmation on a terminal and require `--yes`
  otherwise. Include every profile directory under `trash/` in `list`
  and in the purge. Remove the state of a profile that left the
  configuration, after a confirmation.

### ~~PLT-07 (P3) The support report is called redacted and holds every file path~~

**Done.** Built as D23: vapor report replaces each folder and file name
by a stable code in vapor.json, the logs, and the live state; --keep-
paths keeps them. The terminal and the manifest say which was written.
The default reports directory keeps the five newest. e2e S65.

- Where: `core/cli/src/commands/report.rs`, `docs/plans/cli.md:64`.
- Problem: `vapor report` copies `vapor.json`, each log file, and the
  live status, diagnostics, and timeline. These hold the sync roots,
  the device id, and the path of each file that synced. The plan calls
  the report "redacted". The manifest tells the user to review paths
  before sharing. The repository is public, so a report attached to an
  issue is public.
- Reports are never removed. Each one can hold 24 MiB of logs.
- Fix: add `--redact-paths` (replace each path component with a stable
  hash) and make it the default. Print the warning on the terminal, not
  only in the manifest.

### ~~PLT-08 (P3) Smaller platform and CLI findings~~

**Done.** The --user and --system service flags are gone. The task id is
out of the comment. Case sensitivity is read for each volume on macOS
(pathconf) and used by the walk and the overlap check. The plans
document is in the documentation stage. Not built: the system trash
through NSFileManager. It cannot be tested without files in the user's
own Trash, and Put Back needs a check in the Finder; a task records it.

- `docs/plans/cli.md:34` says each command supports `--json`. `logs`,
  `config get`, `config set`, `version`, and `run` do not. The README
  is correct.
- `vapor service --system` exists and answers "not implemented yet"
  (`main.rs:92-94,403-406`). `AGENTS.md` §8 allows no placeholder other
  than a platform stub.
- `core/cli/src/commands/service.rs:985` has a task id in a comment
  ("LT-1 style"). `AGENTS.md` §8.9 forbids it.
- The macOS `TrashBin` renames into `~/.Trash`
  (`core/platform/src/trash/macos.rs`). The Finder shows no "Put Back"
  for such an item, and a file on another volume is refused.
  `NSFileManager.trashItem` gives both.
- Case folding is answered for the host, not for the volume
  (`core/platform/src/fs_caps.rs:230-262`,
  `core/daemon/src/reconcile_walk.rs:433`). A case-sensitive volume on
  macOS gets conflict copies for names that it can hold side by side.

## macOS app: `apps/macos` (`APP`)

The SwiftUI views were not reviewed (`AGENTS.md` §9.3). The lifecycle
shim, the CLI controller, and the configuration store were read.

### ~~APP-01 (P3) App findings~~

**Done.** The restart at each app launch is gone with LIF-02: an install
is skipped when the definition is unchanged and the job is loaded, and a
start leaves a running daemon alone. The first launch syncs nothing
until a profile exists (D5): the daemon reports unconfigured, and the
app shows that state with the daemon's reason. The README says what the
first launch does in the documentation stage. Not built: an onboarding
flow and a syncMode control in the app (docs/tasks/macos.md), and one
long-lived IPC connection for the menubar status (an idea).

- Each app launch runs `vapor service bootstrap`
  (`VaporApp.swift:26`, `AppShellViewModel.swift:152-200`). That call
  installs the LaunchAgent again and restarts a daemon that runs
  (LIF-02). At login, launchd starts the daemon, and the app then stops
  and starts it two more times. Read only.
- The first launch has no onboarding. It installs the LaunchAgent,
  registers the login item, and starts sync between `~/Vapor` and a
  default folder before the user chooses anything. `AGENTS.md` §1 says
  the roots are user-selected. The onboarding work is open
  (`docs/tasks/macos.md`, phase M6). Until it lands, the README must
  say what the first launch does (DOC-11).
- The app has no control for `syncMode`, so the warning that PLT-01
  asks for has no place in the app yet.
- The status in the menubar comes from a health tick each 30 s. Each
  tick starts two `vapor` processes for as long as the app runs. (Idea:
  one long-lived IPC connection.)
- The good parts: the subprocess runner drains both pipes and has a
  timeout, the configuration store takes the same `vapor.json.lock`
  as the Rust writers, and no lifecycle policy lives in Swift.

## Tests and tools (`TST`)

### ~~TST-01 (P2) No test delivers the events that the daemon's own writes cause~~

**Done.** The fixture has a stand-in watcher, e2e S56 to S60 cover the
four cases, S51 covers the throttle states, and the soak workload has a
case-only rename and a removal that is undone. The new soak operation
found ENG-32.



- Where: `core/daemon/src/runtime.rs` (`BidirectionalFixture`, built
  with no watcher), `tools/e2e/src/scenarios/`, `tools/soak/src/`.
- Problem: the tick-harness tests record watcher events by hand. They
  record the user's events and never the events that follow a rename,
  a trash move, or a download made by the daemon. The e2e scenarios run
  the real watcher with files so small that each transfer ends inside
  the debounce window. ENG-26, ENG-27, and ENG-29 passed all tiers for
  that reason. Each one reproduced on the first try in the manual
  sandbox.
- e2e and soak pin static throttle inputs, so `Light`, `Throttled`, and
  `Suspended` have no black-box coverage (ENG-01, ENG-02, ENG-03).
- Fix:
  1. Give the fixture a mode that replays, for each write the daemon
     makes, the events that FSEvents delivers (a rename-away is
     `Renamed`; a file changed seconds before ends with `Modified`).
  2. Add e2e scenarios: a conflict with a low bandwidth ceiling, a
     removal that is undone after 3 s, a case-only rename on each side,
     and CPU use during a bandwidth-limited transfer.
  3. Add a case-only rename and a remove-then-restore operation to the
     soak workload. A search of `tools/soak/src/workload.rs` found
     rename and move operations and neither of these two.
  4. Give the e2e harness a way to set throttle inputs for a scenario.

### ~~TST-02 (P3) `thread::sleep` in test code~~

**Done.** The six sleeps in `skew_matrix.rs` are gone. The wedged-peer
test holds its stream with a channel, the check-loop test stops on the
restart, and the watcher contract waits for a probe event. The loop in
`singleton.rs` stays: it is a bounded wait for the OS to release an
inherited descriptor, and it asserts nothing about time. The other calls
are product code.

- A search finds `thread::sleep` in `core/ipc/tests/skew_matrix.rs`
  (6 calls), `core/platform/src/fs_watch/contract.rs`,
  `core/daemon/src/singleton.rs`, `core/daemon/src/runtime.rs`, and
  `core/cli/src/commands/auth.rs`. `AGENTS.md` §9.1 bans a sleep for a
  timing-dependent assertion. The calls were counted and not read one
  by one.

## Shared crate: `core/shared` (`SHR`)

### ~~SHR-01 (P3) The device id is written without the configuration lock~~

**Done.** The device id is written under the configuration lock.

- Where: `core/daemon/src/bootstrap.rs:108`,
  `core/shared/src/device_id.rs:59-92`.
- Problem: `resolve_or_persist` reads, derives, reads again, and writes
  `vapor.json` with no `with_config_lock`. The CLI and the lifecycle
  store hold the lock for the same file. A `vapor config set` at the
  daemon's first start can lose its change, or the id.
  `write_config_document` says callers hold the lock.

### ~~SHR-02 (P3) The configuration write has no fsync, and an empty file is silent~~

**Done.** The configuration write syncs the file before the rename and
the directory after it. An empty file that exists is reported as a load
issue.

- Where: `core/shared/src/runtime_paths.rs:71-84`,
  `core/shared/src/config.rs:420-422`.
- Problem: the temporary file is renamed without `sync_all`. After a
  power loss the file can be empty. An empty file loads as the defaults
  with no issue reported, which is the default profile (ENG-07).
- Fix: sync before the rename. Report an empty file that exists.

### ~~SHR-03 (P3) Two devices with the same host name get the same device id~~

**Done.** Built as D25: the id is the host name up to its first dot plus
four random characters. An id that vapor.json already holds stays.

- Where: `core/shared/src/device_id.rs:23-42`.
- Problem: the id is the normalized host name. Two Macs named
  "MacBook-Pro" share one id, so their conflict copies carry the same
  name part. Any label that compares the id with the local one is then
  wrong on the other device (check `conflict::question`). The domain
  part is kept too: `Alexs-MacBook-Pro.local` becomes
  `alexs-macbook-prolocal`.
- Fix: drop the part after the first dot and add a short random suffix.

### ~~SHR-04 (P3) The logger hides the OAuth error code~~

**Done.** The OAuth error code is logged under error_code, which the
redactor leaves as it is.

- Where: `core/providers/src/gdrive/oauth.rs:270`,
  `core/shared/src/logging.rs:296-330`.
- Problem: the code is logged under the key `oauth_error`. The logger
  redacts every key that contains `oauth`. The log shows `[REDACTED]`
  where the runbook expects `invalid_grant`.
- Fix: rename the key (`error_code`).

### ~~SHR-05 (P3) Constants that repeat or disagree~~

**Done.** The filter's internal names come from the provider constants.
The root marker is matched as a whole name, so a user file named .vapor-
roots.txt syncs. The default profile id and the profiles directory name
each have one definition. The comments of the throttle states and of the
HTTP socket timeout say what the code does.

- `filtering::INTERNAL_IGNORE_FILE_PREFIXES` repeats `.vapor-tmp-` and
  `.vapor-root`, and the suffix list repeats `.vapor-meta.json`. The
  `provider` module defines all three.
- `.vapor-root` is matched as a prefix. A user file named
  `.vapor-roots.txt` is ignored without notice.
- `provider::DEFAULT_PROFILE_FALLBACK`, `profile::DEFAULT_PROFILE_ID`,
  and `runtime::DEFAULT_PROFILE_ID` in the daemon are the same value in
  three places. `runtime_paths.rs` holds the literals `"profiles"` and
  `".vapor-tmp-"`.
- `throttle_state::LIGHT` says "a user at the keyboard". The code maps
  an active user to `Throttled`.
- `provider::HTTP_SOCKET_TIMEOUT_SECONDS` says a stalled read would
  wedge the tick loop. Provider calls left the tick thread.

### ~~SHR-06 (Idea) The default ignore list hides user data without notice~~

**Done.** Built as D20: the default list stays. vapor status counts the
paths that the ignore rules keep out of sync (ignoredPaths for each
profile, from the last whole-scope scan). vapor ignored lists each one
with the rule that hides it; a folder is listed one time. e2e S66.

- Where: `core/shared/src/constants.rs` (`DEFAULT_PRE_IGNORE_RULES`).
- Problem: the list has generic names: `build/`, `out/`, `dist/`,
  `target/`, `vendor/`, `deps/`, `obj/`, `coverage/`, `venv/`, `*.log`,
  `*.bak`, `*.part`. A person who is not a developer can have a folder
  with one of these names. Its content never syncs, and no status line
  says so. The README promises "Nothing is lost".
- Fix: show a count of ignored paths in `vapor status` and add a
  command that lists them. Consider shorter defaults.

## What this review did not cover

- `assets/` (the artwork, the rebuild tool, the generation notes).
- The SwiftUI views and the Swift tests.
- Most Rust test code. Tests were read only where a finding needed
  them.
- The Linux and Windows files of `core/platform`
  (`secrets/linux.rs`, `service/linux.rs`, `trash/linux.rs`,
  `metrics/linux.rs`, the Windows stubs).
- The internals of `tools/e2e` and `tools/soak` beyond the scenario
  list and the workload operations.
- `core/cli/src/commands/decisions.rs`, `ipc.rs`, and `config.rs` in
  detail, and the decision and migration code of `state_db.rs` beyond
  an outline.
- Google Drive against a real account. Each Drive finding is from the
  code. PRV-01 needs a live check first.
- The packaged app, the LaunchAgent, and the release workflow were not
  run.
