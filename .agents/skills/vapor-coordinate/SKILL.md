---
name: vapor-coordinate
description: Runs a Vapor project as a coordinated build in T3 Code. One coordinator thread plans stages, starts one worker thread for each stage on the owner's personal providers, follows each worker to a merge-ready pull request, and ends every message to the owner with a status table. Use when the owner asks to coordinate a project, to act as a coordinator, or to run work as parallel threads or stages.
license: GPL-3.0-only
---

# Coordinate a project

The owner talks only to the coordinator. The coordinator plans, starts
workers, answers them, checks their PRs, and reports. It does not
implement. A worker does one stage, opens one PR with `vapor-pr`, and
keeps it ready to merge. The owner merges.

The owner reads **only the last status table** in the coordinator chat.
Every rule below keeps that table correct, short, and easy to find.

This skill needs the thread tools of T3 Code (`t3_thread_launch` and
the others below). In a session without them, say so and stop.

## Providers and models

Every thread and every task that a coordinator starts runs on a
personal provider of the owner (`AGENTS.md` §8.12).

1. Call `orchestrator_capabilities` one time, at setup. Use only an
   instance whose `displayName` starts with `Personal`. Two of them
   matter: the Claude instance, which lists Claude models only, and
   the Gemini instance, which lists the Gemini models and can list
   Claude models too. In the status file, write for each of them its
   `providerInstanceId` and each model of the table that it lists,
   with the thinking option of that model on that instance.
2. Never start a worker, a research task, or a reviewer on another
   instance, whatever model it offers. A message to a thread starts a
   turn on the instance of that thread, so the rule also covers a
   thread that you did not start.
3. Read your own thread with `t3_thread_configure({})`. If it does not
   run on a `Personal` instance, start nothing, tell the owner, and
   stop.

| Work | Instance | Model |
| --- | --- | --- |
| The coordinator and the workers | Claude | the latest Claude Fable or Claude Opus |
| A worker, when no Claude model has usage left ("No usage left") | Gemini | the latest Gemini Neon |
| The adversarial review of a stage that started on Claude | Gemini | the latest Gemini Neon |
| The adversarial review of a stage that started on Gemini | Claude | the latest Claude Fable or Claude Opus |

"Latest" is the highest version of the family in the list of that
instance. Take the plain model entry, not an `Orchestrator` entry.
Model ids change. Read them from `orchestrator_capabilities`, never
from memory.

**Which model a worker gets.** A worker starts on Claude while Claude
has usage. Pass `modelSelection: {instanceId, model, options}` in
every launch, and do not count on what a thread inherits.

**Which thinking level.** Every worker and every reviewer runs at
`xhigh`, or at the highest level that its model lists when the model
has no `xhigh` (`AGENTS.md` §8.12). Put the level in each call: a
thread or a task that starts with a model and no options runs at the
default level of that model, which is lower. The option belongs to a
model on an instance: the same model can have another option id
(`effort` or `thought_level`) and other levels on another instance.
Read both from `orchestrator_capabilities` for the instance that you
use. The shape is `options: [{id, value}]`.

**No usage left.** A worker that stops on a usage limit shows as
`failed` or `interrupted` in `t3_thread_list`, and its last items name
the limit.

Each time, move the worker to the first of these that it did not try
and whose limit is not in force:

1. the same Claude model on another `Personal` instance that lists
   it, with the thinking option of that instance;
2. the other Claude model of the two, on the Claude instance. A limit
   can apply to one model only;
3. the latest Gemini Neon. The worker stays there until its PR merges.

When none is left, the stage is **Blocked**. Put the reset time in its
"Now" cell, tell the owner, and move the worker when the first limit
resets.

Move it with `t3_thread_configure({threadId, environmentId,
modelSelection: {instanceId, model, options}})`, then send "Continue
from where you stopped. Read your brief again first." Then:

- Start new workers on the model that works, until the limit resets.
  Write the reset time in the status file. Stages that start after it
  use Claude on the Claude instance again.
- Tell the owner in the next owner message: "B2 continues on Gemini
  Neon. Claude usage returns at 18:00."

**Which model reviews.** The reviewers of a stage come from the family
that the stage did not start on. Fix that when you launch the worker,
and put that one instance and model in its Coordination block. A move
to another model does not change it, and the block wins over the
general rule of `vapor-pr` step 5. When that family cannot run (no
usage left, a provider error), the worker's own subagents review, as
`vapor-pr` step 5 says, and the final report names the model that
reviewed. In two cases the family that reviewed also wrote a part of
the change: the other family could not review, or you moved the worker
to the family of its reviewer. The PR cell of the stage then says
`review by the same family`, so that the owner knows it before the
merge.

## Setup (once per project)

1. Choose a project key, 1 to 8 capitals (`MAC`, `CORE`), and a plain
   project name.
2. Rename your thread: `t3_thread_update({action:"rename",
   title:"[<KEY>] Coordinator: <name>"})`. No `threadId` renames your
   own thread.
3. Find your thread ID and your link with
   `t3_thread_list({titleContains:"[<KEY>] Coordinator"})`, and your
   environment ID with `t3_environment_read({})`. Workers need the
   three to message you.
4. Check the providers (section above).
5. Read what the plan depends on: `docs/tasks/README.md`, the task
   file of each surface of the project, the specs behind those tasks,
   and the open pull requests (`gh pr list`).
6. Create the status file (next section).
7. Send the owner the plan as an owner message, with one Decide card
   that asks for its approval. The card lists every stage, and says:
   "With your approval you ask for one PR for each stage of this
   project, the stages that I add later included."

**No worker starts before the owner approves the plan.** An agent
pushes a branch and opens a PR only when the owner asks for it, and
the approval of the plan is that request (`AGENTS.md` §10). It covers
the PR of each stage of the project, and nothing else. Keep the
owner's reply in the status file, word for word and with its date:
each brief quotes it. After the approval, ask no more for a start. A
stage that you add later, a split or a fix, starts as any other stage.
Name it and its reason in the next owner message.

## Status file

Keep the project state in `~/.coordinator/<KEY>.md`. Edit it with your
normal file tools. It holds:

- your coordinator IDs, and for each provider instance its models
  with their thinking options;
- the parallel limit, and a usage limit with its reset time;
- the stage list: ID, name, state, thread link, PR, the task ids that
  the stage closes, owned files, "after", the model family that the
  worker started on, and the reviewer that you gave it;
- the owner's approval of the plan, word for word, with its date;
- the contracts list and the decisions so far;
- the open owner items.

Update it in the same turn as every change. Read it first after a
context compaction. It is your memory, not something that you show the
owner. The owner sees the table. The file is outside the repository on
purpose: it holds the thread IDs and the paths of one machine, and
none of it goes into a commit (`AGENTS.md` §6.1).

## Stages

You own the plan. Split the work into stages. A stage is one PR-sized
unit that one worker can finish alone. It becomes one commit on `main`,
so one commit subject must be able to describe it (`vapor-commit`).

- **Source:** the open tasks in `docs/tasks/` and the agreed specs in
  `docs/specs/`. Write in the status file which task ids each stage
  closes. Task ids stay out of code comments and system docs
  (`AGENTS.md` §8.9).
- **ID:** a group letter and a number (`A1`, `B3`). Add a letter for a
  split (`C4a`).
- **Name:** natural words that the owner understands without the spec.
- **After:** the stages that must merge first.
- **Owned files:** two open workers never own the same file.
- **Shared files:** some files get a line from almost every stage.
  Name them in each brief. A worker adds only its own lines there:
  `CHANGELOG.md` (`Unreleased`), `docs/tasks/*.md`,
  `docs/specs/README.md`, the scenario group files
  (`tools/e2e/src/scenarios/*.rs`, `tools/vm/src/scenarios/*.rs`) with
  their catalogs (`docs/development/e2e-verification.md`,
  `docs/development/vm-verification.md`),
  `core/shared/src/constants.rs` with its Swift mirror,
  `assets/locales/en.json`, and the accessibility identifiers
  (`AccessibilityID.swift`, `tools/vm/src/scenario.rs`).
- **Numbers that two stages can both take:** a Tier E2E scenario id
  (`Sxx`), a Tier VM scenario id (`Vxx`), the state schema version,
  the IPC schema version. Give each one out yourself, and keep it in
  the contracts list.

You can add, rename, split, reorder, or drop stages at any time. When
you rename a stage, rename its thread too.

## Design comes first

A stage is built from an agreed spec, or from a task with one obvious
answer (`AGENTS.md` §1.2). A worker never designs with the owner.

- For work that needs a design, run `vapor-design` yourself before you
  plan its stages: the facts first, then the rounds. A round is one
  Decide card that holds the numbered questions, each with its
  recommendation. Its `Reply:` line is `<KEY>-<n>: ok`, and the owner
  can add corrections ("ok, Q2: one per weekday").
- You write the spec, as `vapor-design` step 3 says: every answer goes
  into it before the next round. You open no PR, so the file reaches
  the repository through a stage. Until then it is a draft at
  `~/.coordinator/<KEY>/<spec file name>`, next to the status file.
- The last round asks to start, as `vapor-design` step 4 says. When
  the owner says yes, set the status of the draft to `agreed`. If the
  owner approved the plan already, the stages of the design start as
  any other stage. If not, put them in the plan: the approval card of
  the plan (Setup) is what starts them.
- One open stage owns the spec file. The first stage of the design
  copies the draft, unchanged, to `docs/specs/` and adds its row to
  `docs/specs/README.md`. Compare the file in its PR with the draft
  (`diff`). When the stage that owns the spec merges or is dropped,
  give the spec to another stage of the design in the same turn: an
  open one, or the next one that starts. When no stage is left, plan
  a small stage for it.
- Every stage of the design reads the spec in full. Until the file is
  on `main`, its brief gives the path of the draft.
- A design question that a worker finds comes to you. Answer it when
  the spec settles it. If it does not, write a Decide card. Send the
  answer, word for word, to the stage that owns the spec, as the next
  decision number. Add it to the draft too while the file is not on
  `main`.
- The last stage of the design sets the status to `implemented` with
  the date, and adds what was verified. Say so in its brief.
- A change that contradicts a recorded decision is always a Decide
  card.

## T3 tools

**Start a worker.** Use one thread per stage. Do not use
`create_threads`, and do not use `delegate_task`, for stage work.

```
t3_thread_launch({
  title: "[MAC] B1: Menu bar states",
  clientRequestId: "mac-b1",                     // reuse it on a retry; never launch twice
  workspaceStrategy: { type: "worktree", baseRef: "origin/main",
                       startFromOrigin: true, branch: "mac/b1-menu-bar-states" },
  modelSelection: { instanceId: <instance id>, model: <model id>, options: <options> },
  delegatedFrom: { threadId: <your id>, environmentId: <your env>, title: <your title> },
  message: <brief>
})
```

- The branch name is public (`AGENTS.md` §6.1). Use the key, the stage
  ID, and plain words.
- A stage always starts from `origin/main`. Do not stack a stage on
  an unmerged branch: `vapor-pr` makes one commit of everything that
  is not on `main` and opens the PR against `main`, so the PR would
  hold the other stage too. A stage that needs the code of another
  stage waits for its merge.
- `modelSelection`: see "Which model a worker gets" and "Which
  thinking level". Omit the modes, so that the worker inherits yours.
- Keep the parallel limit that the owner set. With no limit, use 3.
  Each worker builds the workspace in its own checkout and runs the
  validation ladder on the same machine, every push to an open PR
  starts nine CI jobs, and all the workers use the usage of one
  account.

**Research** whose result only you need: `delegate_task({task,
role:"research"})`. With no `target` it runs on your instance and
model. The result returns to your thread. Stop it with `task_cancel`.
It gets no table row.

**Send to a worker:** `t3_thread_send({threadId, environmentId,
message, mode:"queue"})`. Use `mode:"steer"` only to correct a running
worker now ("stop, B3 owns that file"). A send to a finished worker
starts a new turn.

**Read a worker:** `t3_thread_read({threadId, environmentId})`, newest
first.

**Check for silent workers:** `t3_thread_list` shows the status of
each thread. If a worker is idle but did not report, read it and wake
it with a send.

**Wait:** never. Do not poll, sleep, loop, or block on
`t3_thread_wait`. End your turn. A worker message wakes you.

**Stop a worker:** `t3_thread_interrupt({threadId, environmentId,
stop:true, reason})`.

**After a merge:** settle the worker thread with
`t3_thread_organize({threadId, environmentId, action:"settle"})`.

**PR facts:** read them from GitHub at the moment you report.
`pull_request_read` gives the state, `mergeability`, the checks, and
`behindBy`, which is 0 when the branch holds the latest `main`. `gh pr
view <url> --json reviewDecision` gives `CHANGES_REQUESTED` when a
person asked for changes. Never copy "ready" or "green" from a worker
message without this check. `REVIEW_REQUIRED` and `mergeStateStatus:
BLOCKED` are normal here: the owner merges through the admin bypass
(`docs/ci/required-checks.md`).

## The brief

The brief must stand alone. The worker has none of your context.
Include:

1. the stage, its goal, why it matters, and the task ids that it
   closes;
2. the documents to read in full: `AGENTS.md`, the spec (on `main`,
   or the path of your draft), the architecture documents of the
   area, and the skills that the stage
   needs (`vapor-validate` always, then `vapor-e2e`, `vapor-vm`,
   `vapor-config`, `vapor-provider`, `vapor-docs`, `vapor-commit`,
   `vapor-pr`);
3. "Done when": what passes, what is proved, and how. New behaviour
   needs a scenario that fails before the change and passes after it
   (`AGENTS.md` §9.8, §9.9);
4. the invariants of `AGENTS.md` §3 to §6 that the stage touches, and
   the decisions and contracts that it depends on;
5. its owned files, the files that other workers own, the shared
   files, and the numbers that you gave it;
6. local run isolation (below);
7. the owner's request for the PR: the sentence of the approval
   card, the owner's reply, word for word, and its date. Then: "Open the PR with `vapor-pr` when the stage is complete
   and the local ladder is green. The request covers this one PR and
   the pushes of its branch. Every push to an open PR starts nine CI
   jobs, so put every fix that is ready into one push. Do not merge.";
8. the Coordination block, filled in.

**Local run isolation.** A worker's checkout has its own `.vapor/`, so
Tier 1, Tier E2E, and a soak of two workers do not meet. Three things
are shared, and the brief says so:

- Tier VM takes one lock for the whole machine. A second run waits for
  it. That is "queued", not "blocked".
- The Google Drive test account is one account. A worker that uses it
  works in a cloud folder that has its stage ID in the name, and
  touches no other folder. The workload tool reads the sign-in of the
  account from the `.env` of the checkout
  (`tools/vm/workloads/README.md`). A worker whose checkout has no
  `.env` reports it, and never puts a secret in a message.
- `~/.vapor` and the host's launchd jobs belong to the owner. No
  worker touches them (`AGENTS.md` §10.1).

```
## Coordination
You are worker <ID> of project <KEY>. Coordinator: <coordinator link>
(threadId <id>, environmentId <env>). Do the work yourself. Start no other threads.

- Report with t3_thread_send({threadId:<id>, environmentId:<env>, mode:"queue",
  message:"[<KEY> <ID>] <message>"}). Report when you have a question, when you are
  blocked, when the PR is ready, when it fails, and when it merges. Keep each report
  short: what happened, what you need, the PR URL.
- Say "blocked" only when you cannot continue because of something outside your
  control (a decision, another stage, an account). Waiting for the Tier VM lock is
  "queued": do not report it as blocked.
- With a question, continue with the parts that do not depend on the answer.
- A design question is not yours to settle. If the spec and the brief do not answer
  it, and the answer changes what a user gets, report it as a question.
- Before you touch a file outside your list, ask the coordinator.
- A bug or a possible improvement that you find and do not fix becomes a task under
  docs/tasks/ in your PR. Name it in the final report.
- The one child work that you start is the adversarial review of vapor-pr step 5.
  Start its reviewers with delegate_task and target {providerInstanceId:<reviewer
  instance id>, model:<reviewer model id>, options:<reviewer options>}. This target
  stays the same if the coordinator moves you to another model, and it wins over the
  general rule of vapor-pr step 5. If it cannot run (no usage left, a
  provider error), your own subagents review, and your final report says so. Use no
  other provider instance.
- After you open the PR, call watch_pull_request with until:"merged". On a conflict,
  or when the coordinator says that main moved, repeat vapor-pr step 2 (the rebase)
  and push one time. On red checks, fix them. Report "ready" only when vapor-pr
  step 7 holds: no conflict, the branch holds the latest main, nine green checks,
  every review finding fixed or declined, every human comment answered.
- Before you report "ready", write your **final report** as a message in your own
  thread, in Simplified Technical English. The owner reads it before merging, with
  no other context. Sections:
  1. **Asked:** the task, in 2 or 3 lines.
  2. **Changed:** what you built or fixed, in plain words, then the main files.
  3. **Decisions:** each choice you made that the brief did not fix, and why.
  4. **Result:** how it looks or behaves now. For a change that a user sees, embed
     the Tier VM screenshots with Markdown image syntax and absolute paths:
     ![before](/abs/path.png). Never only link them. A picture shows a guest only.
     List what the owner must judge: you verify behaviour, never the look.
  5. **Evidence:** each script of the validation ladder and its last line, the
     scenarios that you added, and the review: the model, each finding, its verdict.
  6. **Not done / risks:** anything skipped, deferred or uncertain, and the tasks
     that you added.
  7. **PR:** the link, and the GitHub state.
  Then send the coordinator a short "ready" report that points to it.
- Never message the owner. The coordinator decides what reaches the owner.
```

## Start work as soon as it can start

After the owner approved the plan, never let a ready stage wait for
the owner to ask. At the end of **every** turn, before you write the
output, check the plan. Start each stage whose dependencies are all
resolved, up to the parallel limit, without a question to the owner. A
dependency is resolved when:

- the stage it waits for has merged;
- the owner answered the card it waits for;
- the external fact it waits for arrived (an account, a result, a
  usage limit that reset).

When a slot frees up because a stage merged or stopped, fill it in the
same turn.

**No batches.** Never hold back a stage that can start so that it
starts together with other stages. Each stage starts on its own, the
moment it can. A wave in `docs/tasks/README.md` orders the work. It is
not a reason to start stages together. The only valid reasons to keep
a startable stage waiting are:

- it would own a file that an open stage owns;
- the parallel limit is full;
- its design is not agreed;
- the owner did not approve the plan yet, or told you to hold the
  stage.

The **Next** line gives the reason for every stage that is not
running, for example "C1a (after B3)" or "F5 (limit full)". A stage
with no reason must be running. If you find one, start it before you
send the message. Ask the owner only when a stage needs a decision
that only the owner can make. Report each start in the next owner
message ("Started C1a: trash restore").

## Quality and consistency (the coordinator owns it)

Workers do the work. **You are responsible for the result.** Every
stage must make the right change, take the right decisions, run the
tests it needs, and agree with the spec and with every other stage. A
worker's claim is not evidence. Check it.

- **Contracts list.** In the status file, keep the shared facts that
  more than one stage depends on: IPC payloads and `--json` shapes,
  config keys and constants, file and path names, the numbers you gave
  out, and the owner's decisions. Put the relevant ones in each brief.
  When one changes, tell every affected worker in the same turn.
- **Plan check for large or risky stages.** In the brief, ask the
  worker to send its plan (approach, files, tests) before it writes
  most of the code. Compare the plan with the spec, the contracts, and
  the other stages, and correct it early. Ask for it for every stage
  that touches a rule of `AGENTS.md` §3 to §5.
- **Scope.** A worker that needs a file outside its list must ask you
  first. You decide, and you tell the owner of that file.
- **Check every PR before you mark it ready:**
  - read the diff against the brief, the spec, and the contracts list;
  - read the final report, and check each decision in it. A wrong
    decision goes back to the worker;
  - check that the ladder that `vapor-validate` asks for really ran and
    passed (the PR body, the CI logs, the scenario output), and that a
    new scenario covers each new behaviour;
  - check that the adversarial review ran on the reviewer that you
    gave the stage, or that the final report says why it could not,
    and that each finding has a verdict with evidence;
  - check the documentation duties of `vapor-docs`: one `CHANGELOG.md`
    line, the tasks closed, each finding added as a task;
  - read the diff, the commit message, the branch name, and the PR
    text for personal information (`AGENTS.md` §6.1);
  - look for conflicts with other open PRs: the same files, the same
    names, or behaviour that contradicts another stage.
- **One adversarial review per PR.** The worker runs it. Do not start
  a second one (`AGENTS.md` §10). Your check above is not a review by
  subagents: you do it yourself.
- **Conflicts are yours to resolve.** When two stages disagree, or a
  stage disagrees with the spec, decide which way is right, tell both
  workers, and record it in the contracts list. Ask the owner only
  when the spec does not settle it and the choice changes the product.
  Then it becomes a Decide card.
- **Spec changes.** When a decision changes what the spec says, send
  it to the stage that owns the spec ("Design comes first"), so that
  later briefs and the spec agree.
- **After a merge:**
  1. Check that `main` stays green for that commit (`gh run list
     --branch main --limit 5`). If the merge broke something, start a
     fix stage at once.
  2. Tell each worker with an open PR that `main` moved, in one
     message. The ruleset wants a branch that holds the latest `main`,
     so the worker rebases and pushes one time. A worker with no PR
     yet rebases when it opens one.
  3. Start the stages that the merge unblocks.
- **Lessons.** When a worker makes a mistake that a rule could prevent
  (for example, it runs a test against `~/.vapor`), add the rule to
  every later brief.

## Handle a worker message

1. Read the status file.
2. Act:
   - **Question:** answer it yourself when the spec, the code, or a
     sensible default can settle it. Ask the owner only for a real
     owner decision.
   - **Ready:** check that the worker wrote its final report in its
     thread. If not, ask for it. Check the PR yourself (the list
     above). Check it on GitHub. Mark the stage **Ready to merge**
     only when there is no conflict, the branch holds the latest
     `main`, the nine checks are green, and no changes are requested.
     Do not add an owner item for it.
   - **Conflict or red checks:** send the worker back.
   - **Stopped on a usage limit:** see "No usage left".
   - **Merged:** mark the stage done, settle the thread, and do the
     three steps of "After a merge".
3. Update the status file.
4. End the turn with an owner message or an internal line (below).

## Owner messages and internal lines

Each turn ends in exactly one of two forms. Both use Simplified
Technical English (`AGENTS.md` §8.11).

**Internal line.** Use it for every worker event that does not need
the owner. End with exactly one line that starts with `·`, for example
`· B3 question answered.` No table, heading, or recap.

**Owner message.** Use it when an owner item is added or closed, when
a PR becomes ready to merge (it shows only in the table), when the
owner wrote to you, or when a stage fails in a way that the owner must
know about. Shape:

```
<1 to 4 short lines: what happened since the last owner message.>

## Needs you
<one card for each open item, oldest first; or "Nothing.">

## Status
<In progress table>
<Next line>
<Done line>
```

`## Status` is always last, and always fresh: check the PRs on GitHub
in the same turn. The owner reads only the latest owner message, so it
always repeats **every** open item in full, not only the new ones.

### Needs you

One card for each open item. The owner may not remember the stage, so
each card gives the full context before the question:

```
### MAC-4 · Decide: what does the menu bar icon show during a pause?
**Stage:** [B1 Menu bar states](<thread link>). It makes the menu bar icon show the
state of the daemon.

**What happened:** the spec gives an icon for a sync in progress, for a finished
sync, and for a problem. It gives none for a pause that the user started. Today the
icon stays on "finished".

![The menu bar of the guest during a pause](/abs/path/states-03-paused.png)

**Options:**
1. Keep the "finished" icon. No new image. The user cannot see that sync is off.
2. **A pause mark on the icon (recommended).** One new template image from the master.
3. The orange attention icon. The app has it already. It looks like a fault.

**Why 2:** the user chose the pause, so the icon must show it and must not look like
a fault.

**Reply:** `MAC-4: 2`
```

- **Heading:** ID, verb, and the action or question. Verbs: Decide (a
  choice), Do (a physical or account action), Answer (a fact that only
  the owner has).
- **Never a card for a merge.** A PR that is ready to merge appears
  only in the Status table, as **Ready to merge**.
- **Stage:** linked to the worker thread, with one line on what the
  stage does.
- **What happened:** the facts that the owner needs, in plain words.
- **Images:** embed them with Markdown image syntax and an absolute
  path. Never only link them. A picture of the app comes from a Tier
  VM guest.
- **Options:** one numbered line each, with its effect. The
  recommended one in bold, then one line on why.
- **Do items:** what the owner needs, then the steps. Link to a
  document section instead of copying a long procedure.
- **Reply:** the exact text to send.
- **IDs:** `<KEY>-<n>`, numbered upward. Never reuse an ID.
- **Complete:** never write "the 13 decisions". Write a card for each,
  or one card with every option listed. A design round is one card
  with every question in it.

### In progress

Only open stages, sorted by urgency: Needs you, Ready to merge,
Question, Blocked, In review, Working, Queued. A stage that cannot
continue without an owner decision, task, or answer shows **Needs
you** and the item ID, so each card and its row point to each other. A
stage whose PR is ready shows **Ready to merge**: the work is
finished, and nothing waits to continue. Its Stage link opens the
worker thread, which ends with the final report. The owner reads it,
then merges. Its "Now" cell says in a few words what the PR does.

When two or more PRs are ready, add one line under the table:
`**Merge order:** #41, then #43`. After each merge, the other PRs need
a rebase and a CI run before they are ready again. Put first the PR
that unblocks the most stages.

```
| Stage | State | PR | Now |
| --- | --- | --- | --- |
| [B1 Menu bar states](<thread link>) | **Needs you** | | MAC-4: choose the icon for a pause |
| [C2 Faster folder scan](<thread link>) | **Ready to merge** | [#41](<url>) · checks green | Scan of 50 000 files: 9.1 s, was 21 s |
| [C3 Trash restore](<thread link>) | **Blocked** | | Waits for the payload of C1 |
| [B2 Settings window](<thread link>) | Queued | | Waits for the Tier VM lock |
```

- **Stage:** ID and name, linked to the worker thread (the `link` from
  `t3_thread_launch`).
- **State:** one word from the table below. Bold only the states that
  may need the owner.
- **PR:** number linked to the PR, then the GitHub facts: `checks
  green`, `checks running`, `checks failing`, `conflict`, `behind
  main`, `changes requested`. Add `review by the same family` in the
  two cases of "Which model reviews".
- **Now:** at most about 60 characters. For Blocked, name what it
  waits for.

| State | Meaning |
| --- | --- |
| **Needs you** | Cannot continue until the owner decides, does, or answers something. "Now" gives the item ID. Never use it for a merge. |
| Working | Running now |
| Queued | Waiting for its turn on a shared resource (the Tier VM lock). Normal. Needs nobody. |
| Question | Waiting for the coordinator's answer |
| **Blocked** | Cannot continue because of something external, not the owner: the merge of another stage, an account, a usage limit. Never use it for a queue. |
| In review | PR open. Checks or the review are running, or the branch is behind `main`. |
| **Ready to merge** | Work finished. Final report written. GitHub: no conflict, latest `main`, nine green checks, no changes requested. No owner card. |

### Next and Done

One line each:

```
**Next:** C4 Trash window (after C3) · D1 Onboarding (design not agreed, MAC-5)
**Done:** A1 Status payload ([#38](<url>)) · A2 Pause reasons ([#39](<url>))
```

### Owner replies

The owner replies in your chat. Usually a reply starts with the ID
(`MAC-4: 2`), but accept plain words, and one message for several
items. Match each part to an item. If the match is not certain, ask,
and name the items. Then act, and in your owner message say which
items you closed ("Closed MAC-4: a pause mark on the icon."). Remove
closed items from the list.

When the owner asks "status", reply with the `## Needs you` and
`## Status` sections.

## Taking over a running project

When the owner asks an existing coordinator to switch to this skill,
or a new thread to continue a project:

1. Do Setup steps 1 to 5. Keep the open worker threads. Do not
   restart them. If `~/.coordinator/<KEY>.md` exists, read it first.
2. Write the status file with every stage: merged, open, and planned.
3. Read each open worker thread with `t3_thread_configure({threadId,
   environmentId})`: it gives the instance, the model, and the
   options. Send no message to a thread on an instance that is not a
   `Personal` one. Move it first with `t3_thread_configure`, and tell
   the owner that you moved it. Set the level of "Which thinking
   level" on each thread that has another one.
4. Rename each open worker thread to `[<KEY>] <ID>: <name>`
   (`t3_thread_update({threadId, environmentId, action:"rename",
   title})`).
5. Write every open owner question as a Needs-you card in the format
   above. If no approval of the plan is on record, add the approval
   card (Setup). Until the answer, start no new worker, and send no
   worker the Coordination block.
6. With the approval on record, send each open worker the filled-in
   Coordination block and item 7 of the brief with `mode:"queue"`.
7. Send one owner message.
