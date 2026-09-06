# Conflict surfacing and resolution

How keep-both conflicts become visible and how they get resolved, across
every surface. The *creation* mechanics (suffix template, determinism,
tombstone interaction) live in `data-flow.md §Conflict handling`; this
document owns everything that happens after a conflict copy exists.

## Design position: the files are the registry

A keep-both conflict produces one artifact: a sibling file named
`{stem}~conflict-{device_id}-{timestamp_ms}[-{seq}]{ext}`. That file **is**
the durable conflict record. There is deliberately no separate conflict
table, quarantine folder, or database:

- **It cannot be capped or lost.** The in-memory activity timeline is a
  bounded notification stream (default 1000 events, non-persistent across
  restarts) — fine for "something happened", unusable as a ledger. Files
  survive restarts, reinstalls, and even removing Vapor entirely.
- **It syncs.** Conflict copies replicate like any other file, so every
  replica (and the cloud web UI) can see and resolve them — including
  copies that originated on *another* device.
- **It self-resolves truthfully.** Deleting the copy — through Vapor
  tooling, Finder, or a shell on any machine — is exactly what marks the
  conflict resolved. A registry would have to chase that state; the
  filesystem simply is it.
- **The name is machine-parseable.** `vapor_daemon::conflict::
  parse_conflict_copy_name` is the strict inverse of the generator: it
  recovers the canonical name, origin device, divergence timestamp, and
  collision sequence, and rejects user files that merely contain the
  marker text (the timestamp segment must be a 12+ digit epoch-ms value).

Trade-off, stated honestly: listing conflicts costs a directory walk of
each profile's local root (pruned by the ignore rules, so `node_modules/`
-class subtrees are never entered — an ignored name can never host a
conflict copy because ignored names never sync). The walk is user-initiated
and read-only. If profiling ever shows it hurting on pathological trees, a
derived cache can be added behind the same CLI contract without changing
any surface.

## The three stages

### 1. Notify

- The daemon pushes a `conflict` event onto the activity timeline in the
  tick that created a copy, and `vapor status` carries a cumulative
  conflict counter (IPC schema v2).
- App surfaces treat the timeline event as the *trigger* (menubar badge,
  native notification) and the CLI listing below as the *content*. The
  timeline's cap is irrelevant here: a missed notification costs nothing,
  because listing never depends on it.

### 2. List

`vapor conflicts list [--json]` scans every enabled profile's local root
and reports, per conflict: profile, canonical path, copy path, origin
device, divergence time, both sizes, and whether the canonical file still
exists. It reads config directly, so it works with the daemon stopped, and
it reports unreadable roots (`skippedRoots`) and unreadable
subdirectories encountered mid-walk (`skippedDirectories`) explicitly
rather than returning a silently incomplete empty list.

The `--json` shape is a locked contract (shape test in
`core/cli/src/commands/conflicts.rs`); app surfaces consume it instead of
reimplementing the scan — the same shim pattern the macOS app already uses
for lifecycle (`vapor service … --json`).

### 3. Resolve

`vapor conflicts resolve <copy-path> --keep <canonical|copy>`:

- `--keep canonical` deletes the copy.
- `--keep copy` renames the copy over the canonical name (atomic on Unix;
  remove-then-rename on Windows, where the at-risk window only ever holds
  the version being discarded).

Both are plain local file operations. A running daemon syncs them like any
user edit — no IPC, no special pipeline — and a stopped daemon converges
them on next start. The command refuses any path whose name does not parse
as a machine-generated conflict copy, so it can never be talked into
deleting an arbitrary file. Manual resolution (Finder, `rm`, the cloud web
UI) remains fully supported and equivalent.

One-way caveat: conflicts are only *created* in `two-way` mode, but copies
can arrive in a one-way scope via sync. In `pull-only`, resolve on the
cloud side (local edits are mirror-reverted); in `push-only`, resolve
locally.

## Per-surface responsibilities

| Surface | Notify | List | Resolve |
|---|---|---|---|
| `vapor` CLI (shipped) | `vapor timeline` / `vapor status` counter | `vapor conflicts list [--json]` | `vapor conflicts resolve` |
| macOS app (planned, `docs/tasks/macos.md`) | menubar badge + native notification on `conflict` timeline events | conflicts pane driving `vapor conflicts list --json` | per-row keep-canonical / keep-copy actions driving `vapor conflicts resolve --json` |
| Windows / Linux apps (future waves) | same model over the same CLI | same | same |

App shells never reimplement scan or resolution logic — the CLI is the
single engine (`AGENTS.md §2`: no business logic in UI code).

## Testing

- Name parsing: round-trip + rejection tests in `core/daemon/src/conflict.rs`.
- Scan/resolve behavior + locked `--json` shape: `core/cli/src/commands/conflicts.rs`.
- End to end: Tier E2E S15 (list finds the S11 conflict, resolve promotes
  the copy, the resolution syncs, list drains to empty).
