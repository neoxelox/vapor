# Configuration in detail

The root `README.md` lists every setting in one table. This page holds
the detail each one needs beyond a line: the edge cases, the floors, and
how profiles merge. The source of truth for names and defaults is
`core/shared/src/constants.rs` (mirrored for the Mac app in
`VaporConstants.swift`).

## The file

All persisted configuration lives in `<vapor_dir>/vapor.json`
(`~/.vapor/vapor.json` unless `VAPOR_DIR` says otherwise). Keys are
camelCase; every value that names a choice is kebab-case. No environment
variable overrides a key: the file is the one place a setting lives, so
the app, the CLI, and a daemon started by the service all read the same
value.

`vapor config set <key> <value>` changes one key and leaves the rest of
the file as it is. A boolean takes `true` or `false`, a number takes a
number, a choice is checked against its values, and a list takes JSON.
One field of a group takes a dotted key and keeps the group's other
fields; `null` removes the field so it takes its default again. A whole
group given as JSON replaces the group, so fields it leaves out take
their defaults. `vapor config get` reads dotted keys too.

```sh
vapor config set syncMode pull-only
vapor config set resourceLimits.cpuPercent 10
vapor config set safeguards.massDelete.threshold 500
vapor config set resourceLimits.concurrentTransfers null
vapor config set postIgnoreRules '["build/", "*.log"]'
```

`vapor config set` refuses an unknown key, an unknown field inside a
group, and a value of the wrong type. A file edited by hand can still
hold one: at start, an invalid value takes its key's default; while the
daemon runs, an invalid new value keeps the value already applied and
the valid keys around it still apply. Both cases, and unknown keys, are
reported in the daemon log; the app's Settings banner names an invalid
value in a key it shows. `null` counts as unset everywhere. A file that
does not parse, or that exists and is empty, is reported as a load
issue and leaves the daemon with no profile: it stays up, syncs
nothing, and `vapor status` says what to fix.

The app and every command write the file under one lock
(`vapor.json.lock`), and each writes only the keys it changes, so a
setting changed in one place survives a save in another.

## When a change applies

- **Within a few seconds**, with the daemon running: `useGitignore`,
  `useVaporignore`, `preIgnoreRules`, `postIgnoreRules`,
  `timelineLimit`, and the `resourceLimits`, `idleBoost`, `safeguards`,
  and `trash` groups.
- **On the next start**: `localRoot`, `cloudRoot`, `provider`,
  `syncMode`, `profiles`, and `deviceId` reshape the pipeline. `vapor
  status` reports `restartRequired`, naming them, until the daemon
  restarts, and `vapor service restart` applies them. The one
  exception is the first profile: a daemon with no usable profile
  (run state `unconfigured`) starts it as soon as its two roots are
  set, with no restart.
- **Elsewhere**: `autoLaunch` is acted on by `vapor service install`
  and `uninstall` (which also set it) and by the app at startup through
  `vapor service bootstrap`. `language` is read by the app only.

`vapor config set` prints which case applies to the key it changed.

## Keys

### `autoLaunch`

Starts Vapor at login and keeps the daemon running in the background.
Default `true`.

### `provider`

The cloud backend: `filesystem` (a local folder plays the cloud side)
or `gdrive`. Default `filesystem`. Google Drive needs a sign-in: `vapor
auth login`, or Sign In in the app. The build carries the Google Drive
OAuth client (`VAPOR_GDRIVE_CLIENT_ID` in the README, and
`docs/operations/provider-auth-operations.md`); tokens live only in the
platform secret store, never in `vapor.json`.

### `syncMode`

The sync direction: `two-way` (default), `pull-only` (cloud to local, a
read-only local mirror), or `push-only` (local to cloud, a read-only
cloud backup). The one-way modes overwrite and remove whatever the
other side holds that the source does not, so a one-way profile syncs
nothing until its warning is answered (`vapor decisions`, or the
question `vapor profiles add` asks on a terminal). A value the build
does not know stops the profile; it never falls back to `two-way`.
`docs/architecture/sync-modes.md` has the full contract.

### `localRoot`

The folder on this device that Vapor syncs. It has no default: a
profile exists only when the user set its two roots, and until then
Vapor syncs nothing and creates no folder. `vapor profiles add`
suggests `~/Vapor`. Vapor creates the folder on first contact. The
path is absolute or starts with `~`; a relative path is a profile
problem, and `vapor profiles add` writes the full path for a relative
one. `vapor config get localRoot` prints an empty line while it is
unset.

### `cloudRoot`

The folder on the provider side. It has no default either; the setup
suggests `/Vapor` at the account root for a cloud provider and
`~/cloud/Vapor` for `filesystem`. Vapor creates it on first contact.
With `filesystem` this is a local path with the rules of `localRoot`,
and it must not overlap `localRoot`: one inside the other, or the same
folder under two spellings on a volume that folds case, refuses to
sync.

### `useGitignore` and `useVaporignore`

Apply the `.gitignore` and `.vaporignore` files found under the local
root. Both default to `true`.

### `preIgnoreRules` and `postIgnoreRules`

Lists of rules in `.gitignore` syntax, one rule per entry.
`preIgnoreRules` defaults to a built-in list of regenerable output
(dependency folders, build output, tool caches, OS and editor junk),
and a value of your own replaces that list. `postIgnoreRules` defaults
to `[]`.

The four keys apply within seconds, with no restart. The precedence of
the four sources, how a rule is read, what the built-in list covers,
what no rule can change, and how to see what the rules hold back:
`ignore-rules.md`.

### `timelineLimit`

How many entries the in-memory activity timeline (`vapor timeline`)
keeps. Default `1000`. A positive integer, at most `10000`.

### `language`

The app's UI language. Default `en`; a language without a catalog falls
back to English.

### `deviceId`

This device's name in conflict-copy names
(`report~conflict-mac-studio-k3x9-….pdf`). Made on the daemon's first
run from the host name (the part before the first dot, lowercase,
`[a-z0-9-]`) plus four random characters, so two devices with one host
name still write different copies. Written once and never regenerated,
so renaming the machine does not change it.

### `profiles`

Named sync profiles. Absent means one implicit profile named `default`
built from the top-level keys, which exists once both roots are set.
`vapor profiles list|add|remove` manages them: the first `add` is the
setup of a fresh install and starts syncing with no restart; `remove`
needs the daemon stopped, forgets the profile's sync state and
sign-in, keeps its trash, and leaves the files on both sides. Each
entry runs as an isolated pipeline with its own state database, trash,
and queue:

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | The profile's name in commands (`--profile <id>`) and on disk. |
| `name` | no | A display name. |
| `enabled` | no | `false` keeps the profile configured but not syncing. |
| `provider`, `localRoot`, `cloudRoot`, `syncMode` | no | Override the top-level value. |
| `resourceLimits`, `idleBoost` | no | Partial groups, merged per field (below). |

Unset fields inherit the top-level value. The two resource groups merge
per field and only toward caution: a profile's `resourceLimits` value
can only lower the top-level one; in `idleBoost`, `enabled: false` wins
daemon-wide, the three `*Percent` ceilings, `headroomCpuPercent`, and
`rampDownSeconds` can only shrink, and `minIdleSeconds` and
`rampUpSeconds` can only grow.

### `resourceLimits`

Ceilings on the daemon. Each one is a measured limit: the daemon
reads what it uses each second and corrects, so a short excursion is
possible and a lasting one is not. Defaults `{ cpuPercent: 15,
memoryPercent: 10, bandwidthPercent: 25 }`.

- `cpuPercent`: share of the whole device's CPU. While the daemon is
  above it, it takes one worker away at each sample, down to one.
- `memoryPercent`: resident size as a share of physical memory.
- `bandwidthPercent`: share of what the link carries, for uploads and
  for downloads separately. Vapor measures that capacity from its own
  transfers (the first estimate is the speed of the network
  interface), so a faster link gives a higher rate with no setting to
  change.
- `concurrentTransfers`: parallel uploads and, separately, parallel
  downloads, `1` to `16`. Absent means automatic: the idle width
  follows the core count (4 to 8 per direction) and always collapses
  while you use the device.

### `idleBoost`

Raises the ceilings while the device is idle. Defaults `{ enabled:
true, minIdleSeconds: 300, cpuPercent: 50, memoryPercent: 20,
bandwidthPercent: 80, headroomCpuPercent: 30, rampUpSeconds: 30,
rampDownSeconds: 10 }`.

The boost runs when the throttle state is `idle`, no input arrived for
`minIdleSeconds`, and other processes use at most `headroomCpuPercent`
of the CPU. It raises the ceilings to its own `cpuPercent`,
`memoryPercent`, and `bandwidthPercent`; a boost ceiling below its
`resourceLimits` counterpart counts as equal to it. Ramping down is
faster than ramping up, so a returning user never meets a busy daemon.

### `safeguards.massDelete`

The ransomware and bulk-mistake backstop. Defaults `{ enabled: true,
threshold: 1000, windowSeconds: 60, ratioPercent: 25 }`.

A burst of deletes in either direction that reaches `threshold` inside
`windowSeconds`, or `ratioPercent` of the synced files (never fewer
than 10), is held whole behind a `mass-delete` decision before any of it
reaches the other side; everything else keeps syncing. `vapor decisions
resolve <id> --choose apply` lets the deletes through; `--choose
discard` restores the files from the side that still has them. Values
below the floors (10 deletes, 5 seconds) are raised to them;
`ratioPercent: 0` turns the ratio rule off; `enabled: false` turns the
guard off, and the daemon logs a warning.

### `trash`

Where a file Vapor removes from this device goes: a delete made in the
cloud, a `pull-only` mirror removal, or a conflict version you did not
keep. Defaults `{ enabled: true, retentionDays: 7, useSystem: false }`.

The managed trash lives under `<vapor_dir>/trash/<profile>/`, or
`<volume root>/.vapor/trash/<profile>/` when the sync root is on another
volume, so a discard there is a rename and never a copy. `vapor trash
list` and `vapor trash restore <id>` read it; `vapor trash empty` asks
first, or takes `--yes`; entries older than `retentionDays` are purged,
for every profile on disk. `useSystem: true` tries the system trash
first (the Finder's Trash on macOS, the freedesktop trash on Linux) and
keeps the managed trash as the fallback. `enabled: false` deletes for
good.
