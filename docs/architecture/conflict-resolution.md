# Conflict surfacing and resolution

How keep-both conflicts become visible and how they get resolved, across
every surface. The *creation* mechanics (suffix template, determinism,
a deletion against a change) live in `data-flow.md §Conflict handling`; this
document owns everything that happens after a conflict copy exists.

## Who keeps the name

The cloud settles it. When a file is edited in more than one place
before the edits sync, the first edit to reach the cloud stays under the
file's name. Each device whose edit lost the race finds the cloud
changed under it, moves its own edit to a copy named after itself, and
takes the cloud's version under the name. Three devices editing
`report.md` at once leave `report.md` and two copies:

```
report.md                                  the edit that reached the cloud first
report~conflict-macbook-air-1731500000000.md   macbook-air's edit
report~conflict-mac-studio-1731500004000.md    mac-studio's edit
```

One case has no device that lost the race. Two uploads of one file can
land in the same moment, each having checked the cloud before the other
wrote. The later one then replaced a version that its device never saw.
The provider keeps that version as `report~conflict-replaced-<ms>.md`:
`replaced` stands where the device id goes (`data-flow.md` §Conflict
handling). It is a version of the file like any other copy.

The copies sync like any file, so every device ends up with the same
three files under the same names. Which version holds the name does not
depend on the device you look from.

## Design position: the files are the record, the decision is the question

A copy is named `{stem}~conflict-{deviceId}-{timestamp_ms}[-{seq}]{ext}`,
and the files themselves are the durable conflict record:

- **They cannot be capped or lost.** The in-memory activity timeline is a
  bounded notification stream (default 1000 events, non-persistent across
  restarts), fine for "something happened" and unusable as a ledger. Files
  survive restarts, reinstalls, and even removing Vapor entirely.
- **They sync.** Every replica (and the cloud web UI) can see and resolve
  a conflict, including one that started on *another* device.
- **They resolve truthfully.** Removing the copies, through Vapor, the
  Finder, or a shell on any machine, is exactly what ends the conflict.
- **The name is machine-parseable.** `vapor_daemon::conflict::
  parse_conflict_copy_name` is the strict inverse of the generator: it
  recovers the original name, the device, the divergence timestamp, and
  the collision sequence, and rejects user files that merely contain the
  marker text (the timestamp segment must be a 12+ digit epoch-ms value).
  A name that holds the marker twice (a copy of a copy) is read at its
  last marker.

What the user answers is a decision, the same kind of question as a held
mass delete or a missing root (`data-flow.md` §Decisions), so every
surface lists and answers one thing: `vapor decisions`. A `conflict`
decision covers one file and every version of it: the file under the
name and all its copies. It opens while a copy exists and closes when
none is left, whoever removed them.

## The three stages

### 1. Notify

- The daemon pushes a `conflict` event onto the activity timeline in the
  tick that created a copy.
- Every few seconds (`constants::conflict::DECISION_SYNC_SECONDS`) the
  daemon matches the open `conflict` decisions to the files on disk. A
  file with copies gets a decision, announced on the timeline like every
  decision. When a version arrives or goes (a third device's copy lands,
  one copy is deleted by hand), the decision is rewritten in place and
  keeps its id. When no copy is left, it is withdrawn. A folder that
  cannot be listed (an unplugged drive) leaves its questions as they
  are. Finding the candidates is a read of the sync index (two partial
  indexes over the marker cover the indexed and the still-queued
  copies); each conflicted file then costs one listing of its folder.
  The pass does not run while the throttle is `Suspended`.
- Two cases wait while the name is still empty: a copy whose original
  is still landing (the cloud's version is being downloaded under the
  name) is asked about once it lands, and a copy made by answering a
  file/folder clash with `keep-both` is that answer (the folder is on
  its way), not a question.
- `vapor status` counts open decisions (`decisions`, total and per
  profile), conflicts included. App surfaces treat the count and the
  timeline event as the *trigger* (the macOS menu bar mark turns the
  brand colour while the count is non-zero) and the decision list as the
  *content*.

### 2. List

`vapor decisions list [--json]` lists every open decision of every
enabled profile. A conflict reads `kind: "conflict"`, `scope: "path"`,
`path` (the file's own path), a `question`, one option per version, and
`evidence.versions`:

| Field | Meaning |
|---|---|
| `file` | the version's file name; also its option key |
| `device` | the `deviceId` whose edit a copy holds, or `replaced` for a version that an upload replaced unseen; `null` for the file under the name |
| `sizeBytes` | its size |
| `modifiedAtMs` | its modification time |

Option labels say whose edit each version is: "Keep the version already
under the name", "Keep this device's edit", "Keep the edit made on
mac-studio", "Keep the version that another upload replaced". The list reads the profile state databases directly, so it
works with the daemon stopped; a conflict with no copy left is left out
until the daemon withdraws it. The `--json` shape is locked in
`core/cli/src/commands/decisions.rs`.

### 3. Resolve

`vapor decisions resolve <id> --choose <file name>` keeps that version
under the file's name and moves every other version to the trash
(reason `conflict`, restorable with `vapor trash restore`).

A conflict answer is applied by the command itself
(`vapor_daemon::conflict::keep`), never by the daemon: it is plain local
file operations, so it works with the daemon stopped, and a running
daemon syncs the result like any user edit, removing the discarded
copies from the cloud and from the other devices. The command:

1. lists the versions again and refuses when they are no longer the
   ones the question lists, sizes and modification times included; the
   question is rewritten and the user looks again;
2. claims the answer, which only succeeds while the question is still
   open, so two answers cannot both apply;
3. moves the file under the name to the trash, moves the kept copy onto
   the name with a rename that never replaces anything (a file that
   lands there meanwhile stays, and the answer stops), and only then
   moves the other copies to the trash;
4. records the answer as applied. When a step fails, nothing is lost
   (every removed version is in the trash) and the question opens again
   with the versions there are then.

An answer still recorded but not applied a minute later
(`constants::conflict::ANSWER_GRACE_SECONDS`) belongs to a command that
died; the daemon opens the question again rather than touching the
files. Every file an answer touches comes from `Conflict::find`: the
file under the name and the machine-named copies of it, never anything
else. Manual resolution (the Finder, `rm`, the cloud web UI) remains
fully supported and equivalent: the question closes on its own.

When the file under the name was deleted and copies remain, the
question asks which copy takes the name; with a single copy left it
offers just that one.

One-way caveat: conflicts are only *created* in `two-way` mode, but copies
can arrive in a one-way scope via sync. In `pull-only`, resolve on the
cloud side (local edits are mirror-reverted); in `push-only`, resolve
locally.

Name-clash copies: a copy that stands in for a cloud name this
filesystem folds onto another local name (`data-flow.md` §Directory and
symlink semantics, name collisions) is aliased to its own cloud object,
not an edit of the local file. Its question offers only the local file
that holds the folded name; keeping it moves the copies to the trash,
which drops their cloud objects through the normal delete path. Moving
a copy onto the kept name would only swap which cloud object is
stranded.

## Per-surface responsibilities

| Surface | Notify | List | Resolve |
|---|---|---|---|
| `vapor` CLI (shipped) | `vapor timeline` / the `decisions` count in `vapor status` | `vapor decisions list [--json]` | `vapor decisions resolve <id> --choose <file>` |
| macOS app | the menu bar mark in the brand colour and a count in the menu while `decisions` is non-zero (shipped); a native notification on `conflict` and `decision` timeline events (planned, `docs/tasks/macos.md`) | a decisions pane driving `vapor decisions list --json` (planned) | one "Keep" button per version driving `vapor decisions resolve --json` (planned) |
| Windows / Linux apps (future) | same model over the same CLI | same | same |

App shells never reimplement matching or resolution logic; the CLI and
the daemon are the single engine (`AGENTS.md §2`: no business logic in UI
code).

## Testing

- Name parsing, grouping, the question, and resolution:
  `core/daemon/src/conflict.rs`.
- One decision per file that grows with a third version and closes when
  no copy is left, and an answer never applied that is asked again
  without the daemon touching a file: the runtime tests
  `a_conflicted_file_is_one_decision_until_no_copy_is_left` and
  `a_conflict_answer_never_applied_is_asked_again_and_the_files_stay`.
- The command's side (refusing changed versions, losing a race to
  another answer, reopening after a failure part way):
  `core/cli/src/commands/decisions.rs` `conflict_answers` tests.
- End to end: Tier E2E S15 `conflict-decisions` (three versions of one
  file are one decision naming each device, keeping the third device's
  version applies at once, the others land in the trash, the result
  syncs, and the question closes).
