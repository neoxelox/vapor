---
name: vapor-pr
description: Takes the current branch to a merge-ready Vapor pull request (one squashed commit, a rebase on the latest main with an overlap analysis, the validation ladder, the push, the PR description template and label, then the CI watch and one adversarial review by the agent's own subagents at the same time, triage of every finding, and the readiness report). Use when the owner asks to open, update, or rebase a pull request, to check its CI or conflicts, to answer a review comment, or to judge whether a branch is ready to merge.
license: GPL-3.0-only
---

# Pull request

One branch, one PR. At open: one commit on the latest `origin/main`, a
green validation ladder, docs in step. Ready to merge: no conflict,
nine green checks, and every finding of one adversarial review fixed
or declined with evidence. Run the steps in order and verify each one.
Start at the step the request names ("open a PR", "check CI", "rebase
on main", "is it ready"), after confirming the earlier ones still hold.

## Rules

- Open a PR only when the owner asks. The request covers this branch's
  pushes (the first push, the fix commits, a rebase onto a moved
  `main`), nothing else, and never `main` (`AGENTS.md` §10).
- Never commit, rebase, or push on `main`. Never `git switch main`:
  another worktree has it checked out. Use `git fetch origin main` and
  compare against `origin/main`.
- Force only with the full command
  `git push --force-with-lease --force-if-includes origin HEAD`, only
  on this branch, only after a deliberate rewrite (the squash, a
  rebase). Never a bare `git push --force-with-lease`: the upstream of
  a fresh branch can be `main`.
- When a push is rejected, fetch the branch and read what is there.
  The owner may have pushed. Integrate it; never overwrite it.
- Never merge. The owner merges, and the repository allows squash
  merges only.
- Never weaken a test, loosen an assertion, or add a suppression to
  make an incorrect implementation pass. A command passed only when
  you ran it and saw the result.
- Shell variables do not survive between tool calls. Run each command
  block below as one call.
- The stash is shared across worktrees. Set work aside with a WIP
  commit. Scratch files (the PR body, the briefs) go under `.context/`.

## 1. Commit and squash

```sh
git fetch origin main
git status --porcelain --untracked-files=all
git log --oneline "$(git merge-base origin/main HEAD)"..HEAD
```

- On `main` or detached, run `git switch -c <slug>` first. Otherwise
  keep the branch; a Conductor workspace names its own. The name is
  public (`AGENTS.md` §6.1).
- Every path in the status list is committed or deliberately left out
  (`vapor-commit`). Only untracked files may be left out. A tracked
  change that does not belong to this PR goes to another branch as its
  own commit before the rebase; it never rides along.
- Before the PR exists, the branch is one commit:
  `git reset --soft "$(git merge-base origin/main HEAD)" && git commit`.
  Write the message for the whole change. Its subject is the PR title
  and the commit on `main`.
- Once the PR exists, do not squash. Fixes are ordinary commits.

## 2. Study what landed on main, then rebase

```sh
git fetch origin main
MAIN=$(git rev-parse origin/main); BASE=$(git merge-base "$MAIN" HEAD)
echo "main is $MAIN"
git log --oneline --no-merges "$BASE".."$MAIN"
comm -12 <(git diff --name-only "$BASE" "$MAIN" | sort) \
         <(git diff --name-only "$BASE" HEAD | sort)
```

Note the SHA that the block prints. Another worktree's fetch can move
`origin/main` at any time, so the study and the rebase both use that
SHA.

- Read every subject. Read the diff of each commit that touches the
  branch's crate, app, script, workflow, contract, or schema, and
  main's full diff of each file both sides touched (the `comm` output).
- Look for overlap with no textual conflict: a renamed function,
  constant, or config key; an IPC payload or `--json` shape both sides
  extended; a schema version both bumped; an e2e scenario id both
  took; two entries under `CHANGELOG.md` `Unreleased`.
- If main already fixed the bug or removed the code, stop and report.

```sh
git rebase <the main SHA from the block above>
```

No conflict is not proof of safety: check each overlap against the
rebased tree. On a conflict, `ours` is `origin/main` and `theirs` is
the branch.

- Read both whole files (`git show origin/main:<path>`,
  `git show REBASE_HEAD:<path>`), not only the hunks. Never take one
  side wholesale. The result carries both intents.
- When the intents contradict and you cannot decide, run
  `git rebase --abort` and report both to the owner.
- In `CHANGELOG.md`, keep both entries.
- No marker may remain:
  `grep -n '^<<<<<<<\|^=======\|^>>>>>>>' <files>`.

## 3. Validate, push, open

1. Run the `vapor-validate` ladder on the rebased tree. Stage each fix
   by name and fold it into the single commit with
   `git commit --amend --no-edit`. Before the push,
   `git status --porcelain` shows no tracked change.
2. Confirm the `vapor-docs` duties. Read the diff, the message, and
   the branch name for personal information (`AGENTS.md` §6.1).
3. Push, then open the PR ready for review, labelled with its dominant
   commit type:

```sh
git push -u origin HEAD                                        # first push
git push --force-with-lease --force-if-includes origin HEAD    # after a rewrite of a pushed branch
gh pr create --base main --title "<commit subject>" --label <type> \
  --body-file .context/pr-body.md
```

The description, in Simplified Technical English (`AGENTS.md` §8.11),
answers in this order:

1. What user or reliability problem does this solve? Link the spec and
   name the decisions the change builds, when there is one.
2. How does it preserve low-impact behaviour?
3. What durability or failure paths were validated?
4. What tests and e2e scenarios were added or updated?
5. What docs and contracts were updated, or why none were needed?

Then, one short section each:

- Verification: each script that ran and its last line.
- Judgement calls: choices no decision covered, and what was left out
  on purpose.
- Main integration: the overlapping commits and how each was resolved,
  or "none".
- Risk and rollback.
- Migration or compatibility (pre-GA: "none required" when a format
  changed without migration, and where that is documented).
- For UI changes, the Tier VM scenarios that ran with their verdicts,
  the screenshots that show the change, and a list of what the owner
  judges. An agent verifies behaviour and never the look
  (`AGENTS.md` §9.9).
- The generated-with line the harness requires.

## 4. Review and CI, at the same time

Start the adversarial review (step 5) in the background the moment the
PR exists. Watch CI and the base while the reviewers work. The review
must not add time to the CI wait (owner, 2026-11-14).

The reviewers read this worktree. Leave it alone until they return: no
rebase, no fix, no format. Note what CI and the base check show, and
act in step 6.

```sh
PR=$(gh pr view --json number -q .number)
gh pr checks "$PR" --watch    # in the background, or poll
gh pr view "$PR" --json mergeable
git fetch origin main && git merge-base --is-ancestor origin/main HEAD; echo "up to date: $?"
```

**CI.** Nine checks must pass: `lint`, `test`, and `build`, each on
macOS, Ubuntu, and Windows. Right after a push the list can be empty
or partial; that is not green. On a failure, read the log first. It
is readable when the run has finished, and so is the rerun
(`gh run view <run-id> --log-failed | tail -200`):

- A code failure: reproduce it locally with the same script and fix
  it. A failure on one OS only is a code failure until the log says
  otherwise.
- An infrastructure failure with no code signal (runner lost, network
  error): run `gh run rerun <run-id> --failed` once. A second failure
  is real.
- Never call a failure flaky without reading the log
  (`AGENTS.md` §9.6).

**Conflicts and base.** `mergeable: CONFLICTING`, or `up to date: 1`,
means `main` moved. Repeat step 2 and force-push with the command in
step 3: the ruleset wants what lands to be what CI tested.
`mergeStateStatus: BLOCKED` is normal, at the end too. The ruleset
asks for an approval a solo maintainer cannot give, and the owner
merges through the admin bypass (`docs/ci/required-checks.md`).

## 5. Adversarial review

One pass per PR, by your own subagents, read only, while CI runs.

**Reviewers.** One per coherent area of the diff, three at most,
because large parallel agent runs reach usage limits. A small diff
gets one. Launch them in one message so they run at the same time.

**Brief.** Each reviewer gets:

- its files, or `git diff origin/main...<head SHA> -- <paths>`;
- the intent: what the PR is for, the spec decisions it builds, and
  the behaviour that must hold;
- the risks of its area from the table, plus any the change raises;
- the rules: read only; edit nothing; run no build, test, or e2e
  script (the main agent reproduces runtime claims); check each claim
  against the code; give a concrete failure (the inputs and state,
  then the wrong result), a severity, and a confidence; no style nits;
  say "nothing real" when that is the result.

| Area | Risks to hunt |
|---|---|
| Engine: `core/daemon`, `core/shared`, `core/ipc`, `core/lifecycle` | Work in the fs-watch callback. Provider I/O on the tick thread. Expensive work no throttle state gates. An intent lost or doubled across a crash. A silent overwrite in `two-way`. A deletion honoured on ambiguous evidence, or one that skips the trash. A self-write loop. A schema or IPC change with no version bump. |
| Providers and platform: `core/providers`, `core/platform` | A capability the provider claims but lacks. A `RemotePath` outside the sync root. A transient failure classed as permanent, or the reverse. A secret outside `SecretStore` or in a log. `unsafe` with no `SAFETY:` comment. A fake that drifts from the native implementation. An OS in the CI matrix that no longer compiles. |
| CLI and app: `core/cli`, `apps/macos` | A `--json` shape or exit code changed with no shape test. Business logic or lifecycle policy in Swift. Window close tied to daemon stop. UI copy outside `assets/locales`. A literal key or default outside the constants modules. |
| Scripts, CI, release: `scripts/`, `.github/`, `tools/` | A script that can touch `~/.vapor` or the network. An action that is not allowlisted and SHA-pinned. A secret that crosses platform environments. A required check renamed without the ruleset. |
| Docs and agent files | Rules that contradict each other or the code. A stale name for a command, key, or path. A skill without its symlink or index rows. A task id or decision number in a code comment or a system doc. A README claim that is not true yet. |
| Every area | Personal information (`AGENTS.md` §6.1). Changed behaviour with no test, e2e scenario, or Tier VM scenario. A test that sleeps, retries, or restates the code. A breaking change with no `CHANGELOG.md` line. |

**Triage.** A finding is a claim to verify, not an order. Read the
code it points at, with its callers and tests. Reproduce a runtime
claim with a test or an e2e scenario. Then classify and act:

- Real defect or gap in the diff: fix it, with the test or scenario
  that would have caught it.
- Real, but older than the PR: report it and add a task under
  `docs/tasks/`. Fix it here only when it is small, safe, and in the
  same area.
- Wrong premise: decline it, with the evidence.
- Style or preference: apply it only when `AGENTS.md`, a spec, or a
  skill backs it.
- Valid finding, bad remedy: fix the finding on its merits. A silent
  fallback, an `#[allow]`, a loosened assertion, or business logic in
  the app is never the fix.

Two findings can share one cause. Find it before fixing either.

**Single pass.** No second review, and no subagent to check a fix.
Verify each fix yourself with `vapor-validate`.

**Other comments.** No external AI reviewer runs on this repository,
and none is to be added (owner, 2026-11-14): it sees the diff without
the task or the decisions. Never ask one for a review. If a bot or an
AI comments anyway, ignore it (no answer, no fix, no resolved thread)
and name it in the report. Answer every human comment in its thread,
and leave the thread for the human to resolve.

## 6. Fix, push, repeat

- Fixes from CI, the review, and human comments are ordinary commits.
  The merge is a squash, and the PR history shows each round.
- Put every fix that is ready into one push. Each push costs a CI
  cycle.
- Run `vapor-validate` before the push, then `git push origin HEAD`.
- After every push, repeat the CI and base checks of step 4. The
  review does not run again.
- Stop and report to the owner when conflicts and CI have not settled
  after three pushes.

## 7. Report

Say "ready to merge" only when all of these hold at the end of the
last round: no conflict, a branch up to date with `main`, nine green
checks, every review finding fixed or declined, every human comment
answered. In Simplified Technical English (`AGENTS.md` §8.11):

- the PR URL and the head SHA;
- the main commits integrated and how each conflict was resolved, or
  "no overlap";
- each check and its result;
- the reviewers and their areas, and each finding with its verdict
  (fixed, declined, out of scope) and where the fix or the evidence
  is;
- each human comment and its answer;
- for UI changes, the Tier VM verdicts, the screenshots, and the list
  of what the owner judges;
- anything left open or skipped, and why.
