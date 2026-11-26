# A coordinator and its worker threads

Status: implemented 2026-11-26. The open points wait for the owner.

## Goal

The owner asked for a way to develop Vapor with several agents at the
same time in T3 Code: one coordinator agent that starts one worker
thread for each part of the work. The owner brought a skill from
another project for it and asked for the skill to fit Vapor (owner,
2026-11-26). The owner talks to the coordinator only, and merges each
pull request.

## Facts

- T3 Code gives an agent tools to start a thread in its own checkout,
  to send a message to a thread, and to read and set the provider and
  the model of a thread. A thread that starts with no model inherits
  the provider, the model, and the options of the thread that started
  it. Checked on 2026-11-26 with the read-only calls
  (`orchestrator_capabilities`, `t3_thread_list`,
  `t3_thread_configure`, `t3_environment_read`).
- The provider list of T3 Code gives each instance a display name. The
  owner's personal instances have names that start with `Personal`.
- Before this change, the adversarial review of a pull request was the
  work of the agent's own subagents, so the model that wrote a change
  also reviewed it (`AGENTS.md` §10).
- An agent pushes a branch and opens a pull request only when the
  owner asks (`AGENTS.md` §10). The skill from the other project
  started every stage with no question to the owner.
- The ruleset of `main` wants a branch that holds the latest `main`
  (`docs/ci/required-checks.md`). Each merge sends every other open
  pull request through a rebase and nine CI jobs.
- Each checkout has its own `.vapor/`, so Tier 1, Tier E2E, and a soak
  of two checkouts do not meet. Tier VM takes one lock for the whole
  machine (`tools/vm/src/runner.rs`).
- The skill from the other project told a worker to merge `main` into
  its branch, and let a stage start on the unmerged branch of another
  stage. Vapor rebases, a branch is one commit when its pull request
  opens, and that commit holds everything that is not on `main`
  (`vapor-pr` step 1).

## Decisions

### Round 1, 2026-11-26

Each decision is the owner's, from the request.

| # | Decision |
|---|---|
| D1 | Vapor has a coordinator skill for T3 Code. One coordinator thread plans stages and starts one worker thread for each stage. A worker takes its stage to a pull request. The owner talks to the coordinator only, and merges. |
| D2 | The coordinator uses the owner's personal provider accounts only, because Vapor is a personal project. |
| D3 | Implementation runs on the latest Claude Opus or Claude Fable model. |
| D4 | When that provider has no usage left, implementation can run on the latest Gemini Neon model. |
| D5 | For now, the adversarial review runs on the latest Gemini Neon model, so that the review is not always biased towards Claude, which implements. |

### Round 2, 2026-11-26

The build made D6 to D13 with no ruling, and the owner then accepted
them with one "ok". D14 is the owner's addition.

| # | Decision |
|---|---|
| D6 | The name of the skill is `vapor-coordinate`, an action, like `vapor-validate`. |
| D7 | With no limit from the owner, three workers run at the same time. Each worker builds the workspace in its own checkout and runs the validation ladder on one machine, each push to an open pull request starts nine CI jobs, and all the workers use the usage of one account. |
| D8 | The worker runs the one adversarial review of its pull request. The coordinator checks each pull request itself and starts no second review. Changed by D19. |
| D9 | A stage that starts on Gemini gets its review from Claude, for the reason of D5. The reviewer of a stage is fixed when the stage starts, so a move to another model does not change it. |
| D10 | When the other model family cannot review, the agent's own subagents do. The report names the model, and the status table of a coordinator says it. |
| D11 | A worker that stops on a usage limit tries the other one of the two Claude models one time before it moves to Gemini Neon. A limit can apply to one model only. |
| D12 | The coordinator writes the spec. It opens no pull request, so the spec is a draft next to the status file until the first stage of the design copies it, unchanged, into `docs/specs/`. When that stage merges or is dropped, another stage takes the spec. |
| D13 | A stage always starts from `origin/main`. A stage that needs the code of another stage waits for its merge, because `vapor-pr` would put both stages into one commit and one pull request. |
| D14 | A worker that stops on a usage limit can continue on the same Claude model on another personal instance that lists it. |

### Round 3, 2026-11-26

| # | Decision |
|---|---|
| D15 | The owner approves the plan of a coordinator one time, before a worker starts. The approval is the owner's request for one pull request for each stage of the project, the stages that the coordinator adds later included. After it, the coordinator starts every stage with no more questions. `AGENTS.md` §10 says so. |
| D16 | A worker and a reviewer run at the thinking level `xhigh`, or at the highest level that their model lists when it has no `xhigh`. The level is in each call, because a call with no level starts at the default of the model. |

### Round 4, 2026-11-26

The pull request of this spec took 100 minutes, and the owner asked
where the time went. Two thirds went to reviews and to CI runs that
the change did not need. Each decision is the owner's.

| # | Decision |
|---|---|
| D17 | The full adversarial review runs when the pull request opens, and not before. |
| D18 | A review is as deep as the change needs. The agent sizes it from the diff: a change of one line gets a short review, and a large or risky change gets depth. A review can be extensive, and it must make sense for the change. |
| D19 | After the first review, a pull request gets another one only when later commits change it a lot. That review covers only what changed since the first one. Small fixes get none. D19 changes D8: the worker can run a second review. The coordinator still starts none. |
| D20 | A CI job runs only what a change touches. A change to documents runs the text checks, a change to the Mac app runs the macOS jobs, and any other change runs everything. |
| D21 | A test that is broken or flaky is fixed, not recorded and left. |

## Design

- The skill is `.agents/skills/vapor-coordinate/SKILL.md`. It keeps the
  parts of the owner's skill that do not depend on a project: the
  status file, the stages, the brief, the status table, and the cards
  for the owner.
- `AGENTS.md` §8.12 holds D2 to D5, and applies D2 to every agent that
  chooses a provider for a delegated task or a reviewer. `vapor-pr`
  step 5 says how a pull request gets its reviewers.
- What the skill takes from Vapor: a worker opens its pull request
  with `vapor-pr` (one commit, a rebase, nine checks, one adversarial
  review); a design goes through `vapor-design`; a finding becomes a
  task; messages to the owner use Simplified Technical English.
- `vapor-pr` step 5 has a table that sizes a review from the diff (D18)
  and the rule for a review after the first one (D19).
- `scripts/scope.sh` gives each CI job one mode for a change: `full`,
  `docs`, or `skip` (D20). A job always starts and reports, because the
  ruleset of `main` requires the nine checks by name. The detail is in
  `docs/ci/overview.md`.
- The names of the files that most stages change (`CHANGELOG.md`, the
  task files, the scenario files, the constants) are in the skill, and
  so are the numbers that two stages can both take (a scenario id, a
  schema version).

## Verification

- `./scripts/lint.sh` on the change.
- The mirror symlink resolves, the front matter parses, and the skill
  is in the skill list of the session that added it.
- The reviewer path of `vapor-pr` step 5 ran one time: one review of
  this change by the latest Gemini Neon model, started with
  `delegate_task`. It gave ten findings. Nine are fixed in the change.
  The tenth became D15. The call gave no thinking level, so the
  review ran at the default level of the model. The skills now give a
  level in every call.
- The adversarial review of the pull request ran on the latest Gemini
  Neon model at `xhigh`, started with `delegate_task`: nine findings.
  Eight are fixed in the pull request. One is declined: it said that
  the `.env` of a checkout never holds the sign-in of the test
  account, and `tools/vm/workloads/README.md` says that it does.
- CI failed one time on the pull request, in e2e scenario S26, on a
  commit that changed documents only. The cause was a defect of the
  daemon, and D21 asked for the fix. The listing of a cloud folder
  that is being removed failed when a subfolder left during the
  listing, the planner read that as "the folder is gone", and the
  empty folder stayed in the cloud. The listing in
  `core/daemon/src/provider_jobs.rs` now starts again when a subfolder
  is gone at its turn, and a folder that changes under every pass is
  looked at again later. Six unit tests cover it.
- The second review, under D19, covered only the fix and the CI gate:
  two reviewers on the latest Gemini Neon model at `xhigh`, one sized
  as small and one as normal. It found that the first version of the
  fix was unsafe: it took the subfolder out of the listing and went
  on, so a folder whose only subfolder got another name looked empty,
  and on a provider that removes a folder with its content the
  planner would have removed a file that it never compared. The
  listing now decides nothing from such a pass. The review of the gate
  gave five findings, all fixed: a path check that failed on Windows,
  git variables that the test did not drop, a file of the Mac app
  whose name changes only in its capitals, a name outside ASCII, and
  Markdown inside the targets of the app.
- The test of `scripts/scope.sh` makes git repositories of its own.
  Its first version did not drop the variables that git sets for a
  hook, and in the pre-commit hook its `git init` set `core.bare` in
  the local repository. The setting was restored. The test now drops
  every such variable and stops when git looks at another repository.
- `scripts/tests/scope.sh` checks the mode of each of the nine jobs
  for each class of change, and the changed files of a pull request
  and of a push in real repositories. Tier 1 runs it.
- Not checked: a CI run in the `docs` and the `skip` modes. The pull
  request of this spec changes the core, so its own CI runs in `full`.
  The first pull request that changes documents only is that test.
- Not checked: the launch of a worker thread, the move of a thread to
  another model after a usage limit, and a whole coordinated project.
  No thread was started for a test. The first project that the owner
  coordinates is that test.

## Open points

Each one is a choice that the build made with no ruling. The first
option of each is what is built.

1. **The files of a coordinator stay under `~/.coordinator/`**, as in
   the owner's skill, and not under `.context/`. They hold thread IDs
   and paths of one machine, and a place outside the repository cannot
   reach a commit. `AGENTS.md` §10.1 lists it.
2. **On a usage limit, the same model on another instance (D14) comes
   before the other Claude model (D11).** The worker then continues on
   the model that wrote its work so far.
