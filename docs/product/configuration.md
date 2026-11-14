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
value in a key it shows. `null` counts as unset everywhere.

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
  restarts, and `vapor service restart` applies them.
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
other side holds that the source does not; `docs/architecture/
sync-modes.md` has the full contract.

### `localRoot`

The folder on this device that Vapor syncs. Default `~/Vapor`. Vapor
creates it on first contact.

### `cloudRoot`

The folder on the provider side. Vapor creates it on first contact.
Unset by default, which resolves by provider: `~/cloud/Vapor` with
`filesystem`, `/Vapor` at the account root otherwise. With `filesystem`
this is a local path (`~` and relative paths resolve like `localRoot`)
and it must not overlap `localRoot`; overlapping roots refuse to sync.
`vapor config get cloudRoot` prints the default for the selected
provider.

### `useGitignore` and `useVaporignore`

Apply the `.gitignore` and `.vaporignore` files found under the local
root. Both default to `true`.

### `preIgnoreRules` and `postIgnoreRules`

Lists of rules in `.gitignore` syntax, one rule per entry. Precedence,
lowest to highest: `preIgnoreRules`, then `.gitignore` files, then
`.vaporignore` files, then `postIgnoreRules`, so a `!rule` in
`postIgnoreRules` wins over everything. `preIgnoreRules` defaults to a
built-in list of regenerable output (dependency folders, build output,
tool caches, OS and editor junk); `postIgnoreRules` defaults to `[]`.
The README's Ignore rules section describes what the defaults cover and
what they deliberately leave in.

### `timelineLimit`

How many entries the in-memory activity timeline (`vapor timeline`)
keeps. Default `1000`. Must be a positive integer.

### `language`

The app's UI language. Default `en`; a language without a catalog falls
back to English.

### `deviceId`

This device's name in conflict-copy names
(`report~conflict-mac-studio-….pdf`). Derived from the hostname on the
daemon's first run (lowercase, `[a-z0-9-]`, at most 32 characters, a
random id when nothing is left), written once, and never regenerated, so
renaming the machine does not change it.

### `profiles`

Named sync profiles. Absent means one implicit profile named `default`
built from the top-level keys. Each entry runs as an isolated pipeline
with its own state database, trash, and queue:

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

Hard ceilings on the daemon. Defaults `{ cpuPercent: 15, memoryPercent:
10, bandwidthPercent: 25 }`.

- `cpuPercent`: share of the whole device's CPU.
- `memoryPercent`: resident size as a share of physical memory.
- `bandwidthPercent`: share of the measured bandwidth.
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
list` and `vapor trash restore <id>` read it; entries older than
`retentionDays` are purged. `useSystem: true` tries the system trash
first (the Finder's Trash on macOS, the freedesktop trash on Linux) and
keeps the managed trash as the fallback. `enabled: false` deletes for
good.
