# Incident Runbooks

## Purpose

What to look at, what to do, and how to know it is over, for the five
runtime incidents `AGENTS.md` §12 names. Each runbook has the same four
parts: detection, mitigation, the state a user sees, and the check that
proves recovery. Release failures have their own document,
`release-incident-playbook.md`.

Every command reads the runtime directory of the install (`~/.vapor`,
or `VAPOR_DIR`). `vapor report` writes a support report with file and
folder names replaced by codes; attach it when a runbook does not end
the incident.

## 1) Daemon crash loop

The daemon exits uncleanly again and again. The supervisor restarts it
with a growing delay, and after 5 crashes inside 10 minutes it stops
restarting (`macos/launchagent-policy.md`, Crash-loop interaction).

Detection:

- `vapor service status` reports `crash-loop-paused`. With `--json`,
  `crashLoop` holds the count and the time of the last crash, and
  `health` says what a supervision tick would find.
- `vapor status` answers `daemon not running`.
- `<vapor_dir>/logs/vapor-supervisor.log` has one line for each change
  of outcome. `vapord.log` and `vapord.stderr.log` hold the last lines
  of each run; a panic message is in the second one.
- On macOS, a crash report for `vapord` is under
  `~/Library/Logs/DiagnosticReports/`.

Mitigation:

1. Read the last ERROR line and the panic message of the last run.
2. Run `vapor doctor`. It checks the runtime directory, the daemon
   binary, each profile with its folders, the secret store, the OAuth
   client, and the service definition.
3. Fix the cause when it is in the environment (a folder the daemon
   may not read, a full disk under the runtime directory).
4. Run `vapor service acknowledge`. The pause ends and restarts resume.
5. When the daemon crashes again with the same message, keep it
   stopped (`vapor service stop`) and send `vapor report`.

One profile that panics does not need this runbook: the daemon
suspends that profile and keeps the others syncing
(`architecture/data-flow.md`, "What stops a whole profile").

User-visible state:

- The app's menu says that Vapor paused after repeated crashes and
  offers the acknowledge action. Nothing syncs.
- Changes made meanwhile are not lost: the startup reconcile of the
  next run finds them.

Recovery verification:

- `vapor service status` reports the health `running`, and `vapor
  status` answers.
- The daemon stays up for 10 minutes. After that the crash count is
  back at zero.

## 2) Sign-in refused or token refresh failed

The provider refuses the stored sign-in: an expired or revoked token,
or none stored. The profile is put on hold; no file fails for it
(`provider-auth-operations.md`, "Failure behavior: the sign-in hold").

Detection:

- `vapor status` prints a `Sign-in required` line. With `--json` the
  profile has `signInRequired: true`, and its run state is `error` with
  a reason that names the command to run.
- `vapor auth status` lists each profile as `signed-in`, `signed-out`,
  or `not-needed`.
- The daemon log has one WARNING when the hold starts. A rejected
  token request is logged with its `error_code`, never with its body.
- The timeline has a `sign-in` entry.

Mitigation:

1. Run `vapor auth login --profile <id>`, or select Sign In in the app.
2. When the error code is `invalid_client`, the build has a wrong
   OAuth client. A sign-in cannot fix that; the build or its
   environment has to. A build with no OAuth client at all asks for no
   token: the profile is suspended, and `vapor doctor` shows the cause
   in the `gdrive-oauth-client` row.
3. When the hold comes back each week, the OAuth client is in Testing
   status at Google, which ends every sign-in after 7 days.
4. When the endpoint answers with a server error or a rate limit, the
   daemon retries by itself and holds nothing. Wait.

User-visible state:

- The profile syncs nothing, and its queue stays as it is. Changes on
  this device are still recorded.
- On macOS the menu bar mark turns orange, and the menu and the
  Dashboard show the notice with the Sign In button.

Recovery verification:

- Within one root check (15 seconds) of the sign-in the daemon logs
  that the hold is lifted, and `signInRequired` is `false`.
- The queue count in `vapor status` goes down with no restart.

## 3) Provider rate-limit storm

The provider answers with a rate limit (HTTP 429, an exhausted quota)
for many requests in a row.

Detection:

- `vapor diagnostics` lists items in the stage `retrying`, with the
  reason `retry backoff active (attempt N)` and the provider's message
  in `error`.
- The queue count in `vapor status` goes down slowly or stands still.

What the daemon does by itself:

- Each rate-limited item waits. The wait starts at 15 seconds and
  doubles, up to 15 minutes. A `Retry-After` from the provider is
  honoured up to 1 hour.
- While a slowdown is in force, the daemon makes one upload, one
  download, and one planner request at a time, because a provider
  counts every kind of request against one quota.
- The slowdown is stored. A restart does not end it, so a restart does
  not help.

Mitigation:

1. Wait. In most cases the storm ends when the quota window of the
   provider ends.
2. Look for another program that uses the same account and quota.
3. When the device must stop asking at once, run `vapor pause`.
   `vapor resume` continues from the same queue.
4. Do not remove the state database. That turns the backlog into a
   whole-scope reconcile, which makes more requests, not fewer.

User-visible state:

- Sync goes on at a low rate. No file is lost and no item fails: an
  item fails for good only after 10 000 attempts.

Recovery verification:

- `vapor diagnostics` shows no item in `retrying` with a rate-limit
  error.
- The queue count in `vapor status` reaches zero.

## 4) State database set aside

At its start the daemon found a state database that it cannot use: a
damaged file, or a file of another schema version. It moved the file
aside and started with an empty one
(`architecture/state-schema-migrations.md`).

Detection:

- The daemon log names both paths: an ERROR line for a damaged file,
  an INFO line for another schema version (`vapor logs`).
- Next to the database is a file `vapor.sqlite.corrupt-<ms>` or
  `vapor.sqlite.schema-v<n>-<ms>`.
- `vapor status` shows a whole-scope reconcile.

Mitigation:

1. Do nothing to the folders. The reconcile compares both sides and
   builds the sync index again. In a `two-way` profile nothing is
   deleted on either side with no index row, and equal files converge
   with no transfer.
2. A profile in `pull-only` or `push-only` asks for its confirmation
   again, because the confirmation was in the old database. Answer it
   with `vapor decisions`. A one-way profile needs no index row to
   remove what its source does not hold: after the answer the mirror
   goes on as before.
3. Keep the file that was set aside when the cause is not known. Look
   inside it with `sqlite3 -readonly <path>`. The daemon removes it
   after 30 days.
4. A file that is set aside again at the next start means the storage
   under the runtime directory is at fault. Check the disk.

User-visible state:

- Sync continues during the reconcile. In a `two-way` profile a
  deletion that was made while the index was gone is not carried over:
  the file comes back from the other side.
- A one-way profile holds until its question is answered.

Recovery verification:

- `vapor status` shows no `Scan:` line any more (`reconcile.state` is
  `idle` in `--json`), and the queue count is zero.
- The next start sets no file aside.

## 5) Reconcile backlog that does not converge

The queue does not empty, or the same scan runs again and again.

Detection:

- The queue count in `vapor status` does not go down over some
  minutes while the run state is `running`.
- `vapor diagnostics` gives, for each item, its stage and the reason it
  does not move.

Mitigation, by what the items say:

| Stage or state | Meaning | What to do |
|---|---|---|
| `held` | the item waits for a decision | `vapor decisions list`, then `vapor decisions resolve <id> --choose <option>` |
| `retrying` | a transient failure or a rate limit | check the network; for a rate limit, runbook 3 |
| `waiting-to-hash`, `waiting-to-upload`, `waiting-to-download` | the throttle admits less work now | read the throttle line of `vapor status`; only `suspended` (a critically hot device) stops sync |
| run state `error` | the profile is on hold | the reason names the cause: a missing or replaced folder (a decision), a sign-in (runbook 2), a full cloud (free space, then `vapor reconcile` tries the waiting uploads at once, after a restart too; with no request they are tried each 15 minutes), a cloud folder that cannot be reached |
| run state `paused` | a user paused sync | `vapor resume`, with `--profile <id>` for one profile |
| a `failed` count above zero | items failed for good | the daemon log has an ERROR line for each, with the path and the error |

When no item explains it:

1. Run `vapor reconcile`. The scan starts now, in every throttle state
   but `suspended`.
2. Compare a file that differs by hand: the daemon log has one line
   for each decision about a path.
3. Send `vapor report`.

User-visible state:

- `vapor status` and the app show the run state and its reason. The
  reason is the first thing to read.

Recovery verification:

- The queue count is zero, and `vapor status` shows no `Scan:` line
  (`reconcile.state` is `idle` in `--json`).
- A file that differed is the same on both sides.
