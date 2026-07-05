# Sync modes (directionality)

Vapor syncs a user-selected local directory against a user-selected cloud
directory. **Sync mode** selects *which direction* changes are allowed to
flow between the two. It is a core-runtime concept: the engine in `core/*`
owns it, and every surface (macOS app, `vapor` CLI, future Windows/Linux
apps) only reads and displays it.

This document is the authoritative design for the feature. The pipeline
mechanics are cross-referenced from `data-flow.md`; the intent and priority
are in `docs/plans/core.md §2.5`; the execution checklist is
`docs/tasks/core.md` C8-59…C8-66.

## The three modes

| `syncMode`  | Direction        | Source of truth | The other side is…            | Use case |
|-------------|------------------|-----------------|-------------------------------|----------|
| `two-way`   | local ⇄ cloud    | neither (peers) | an equal peer                 | normal bidirectional sync (default) |
| `pull-only` | cloud → local    | **cloud**       | a read-only replica of cloud  | read-only local copies / mirrors of a cloud folder |
| `push-only` | local → cloud    | **local**       | a read-only backup of local   | one-way backup of a local folder to the cloud |

- `two-way` is the default and the historical behavior. Both sides are
  equal peers; divergence is resolved by the *keep-both* conflict policy
  (`data-flow.md §Conflict handling`).
- `pull-only` and `push-only` are **one-way** modes. The mode names describe
  the direction from the local device's perspective: *pull* brings the
  cloud down, *push* sends the local up.

## Semantics: strict mirror

One-way modes are **strict mirror**: the subordinate (non-authoritative)
side is driven to *exactly* match the authoritative source. This is a
deliberate product choice — a "read-only backup/copy" that silently
accumulates drift is not a faithful mirror. It is also a deliberate,
opt-in departure from Vapor's default data-preservation posture, so it
carries the safety requirements in §Safety.

### `pull-only` (cloud authoritative)

The local side is a read-only replica of the cloud folder. On the local
side the engine will:

- **Download** remote creates and edits to local (this is the normal
  remote→local apply path).
- **Delete locally** any file that was deleted on the cloud.
- **Revert** a locally-modified file back to the cloud's canonical content
  (the local edit does not propagate and does not survive).
- **Remove** local-only files that do not exist in the cloud folder.
- **Never** upload, create, delete, or rename anything on the cloud.

### `push-only` (local authoritative)

The cloud side is a read-only backup of the local folder. Symmetrically,
the engine will:

- **Upload** local creates and edits to the cloud (the normal local→remote
  path).
- **Delete on the cloud** any file that was deleted locally.
- **Overwrite** a remotely-modified file with the local canonical content.
- **Remove** cloud-only files that do not exist in the local folder.
- **Never** download, create, delete, or rename anything on the local side.

### Interaction with the conflict policy

The *keep-both* conflict policy (`{stem}~conflict-{device_id}-{timestamp_ms}{ext}`)
applies **only in `two-way`**. In one-way modes there is a declared source
of truth, so divergence is resolved in favor of the authoritative side with
no conflict copy. This does **not** change the default: `two-way` remains
the default mode and *keep-both* remains its policy, exactly as
`AGENTS.md §4` requires. One-way strict mirror is a per-profile opt-in.

### Interaction with loop prevention and tombstones

- **`self_write_cache`** is orthogonal and still active in every mode. It
  suppresses the daemon from reacting to changes it just wrote itself
  (`data-flow.md §Loop prevention`). One-way gating decides *whether* a
  direction produces work at all; the self-write cache decides whether a
  given observed change is an echo of Vapor's own write.
- **Tombstones / deletion replay** stay durable in every mode. Strict
  mirror *uses* deletion propagation as a first-class outcome (that is the
  point), so the durable tombstone machinery from C8-16 is exercised, not
  bypassed.

## Where it lives in the pipeline

`syncMode` will be carried on the sync scope
(`core/daemon/src/sync_directories.rs` `SyncScope` — the field lands with
C8-59) and, once profiles land, on the per-profile resolved settings.
The engine consults it at these points (see `data-flow.md`):

- **Local→remote suppression** (`pull-only`): local watcher events do not
  produce upload / remote-delete / remote-rename intents. A local edit
  instead schedules a *restore-from-cloud* reconcile of that path so the
  replica converges back to the source.
- **Remote→local suppression** (`push-only`): the remote poll/apply pipeline
  does not write, delete, or revert anything locally.
- **Strict-mirror reconcile**: whole-scope and subtree reconcile compare the
  two sides and, in one-way modes, actively delete/revert the subordinate
  side to match the source.

Because the gate is a property of the scope/profile and not a special code
path per operation, deletion, rename, and edit all flow through the same
mode check — there is no operation that can bypass it.

## Profiles integration

`syncMode` is a **per-profile, categorical** setting (unlike `resourceLimits`
/ `idleBoost`, which resolve by MIN-lowering):

- The top-level `syncMode` in `vapor.json` is the **default** for any profile
  that does not set its own.
- Each profile may override `syncMode` with any of the three values. There is
  no lowering/merging — the profile's value wins outright for that profile.
- Different profiles on the same device may run **different** modes
  concurrently. This is what makes the headline use case work: keep several
  independent **`pull-only`** profiles, each mirroring the same cloud folder
  into a different local directory (or different clouds), to maintain several
  read-only backups/copies — while an unrelated profile stays `two-way`.
- Per-profile isolation follows the existing multi-profile rules
  (`data-flow.md §Multi-profile watch coordination`): one watcher per
  canonical local root, per-profile queues, shared workgate/throttle. A
  profile's mode only affects that profile's queue.

Classification (per C8-19 / C8-21): `syncMode` is **profile-override-capable**,
not app-global.

## Safety

Vapor's "never lose data" guarantee is **scoped to `two-way`**. The one-way
modes deliberately trade it for a faithful mirror: strict mirror can
**overwrite and delete user data** on the subordinate side (`pull-only`
reverts local edits and removes local-only files; `push-only` does the
symmetric thing to the cloud), permanently, to match the source. That is the
whole point of the feature, so the protection is *informed opt-in*, not a
recovery net — there is no quarantine or undo:

- **Opt-in, per profile.** A profile is `two-way` unless the user explicitly
  sets `syncMode` to `pull-only` or `push-only`. The mode is never inferred,
  and the default never deletes/overwrites to converge.
- **Up-front overwrite warning.** Before a one-way mode is enabled, the macOS
  app and the CLI must state, in plain language, that the subordinate side
  will be made to exactly match the source — divergent local (or cloud) edits
  are overwritten and local (or cloud)-only content is removed, permanently.
  (macOS: M4-5.) The warning is the safeguard; there is no recoverable copy.
- **Observability.** Diagnostics/IPC expose each profile's `syncMode` and a
  count of mirror-driven reverts/deletes so the behavior is never silent
  (C8-65).

## Config surface

- **Key.** `syncMode` (string enum) in `vapor.json`. Values: `two-way`
  (default), `pull-only`, `push-only`.
- **Source of truth.** `core/shared/src/constants.rs` (`config::KEY_SYNC_MODE`
  + `ALL_KEYS`, and the `SyncMode` enum + its default). Mirrored in the Swift
  `VaporConstants` per `AGENTS.md §8.6`.
- **CLI.** `vapor config get|set syncMode <value>` validates against the enum
  and rejects unknown values with an actionable error (like the existing
  boolean/int typed keys). Per-profile editing arrives with the profiles CLI
  surface.
- **Docs.** Documented in root `README.md` **Configuration**, this file, and
  `data-flow.md`.

## Build / rollout order

The feature ships in Wave 8 (runtime capability completion), **before** the
macOS app UX (Wave 9), matching the priority that the core runtime must be
proven before any app surface exposes the toggle. Within the sub-wave the
build order is deliberately:

1. **`pull-only` first** — exercises and validates the remote→local
   download/apply pipeline (C8-1…C8-13) in isolation, with no upload risk.
   This is the cleanest way to confirm Vapor downloads and mirrors cloud
   content correctly.
2. **`two-way` second** — binds `syncMode = two-way` to the bidirectional
   keep-both pipeline (C8-14…C8-18) and asserts the one-way gates are inert.
3. **`push-only` third** — validates local→cloud strict mirror (remote
   overwrite + remote-only deletion).

## Testing

Covered by C8-66 and the standing bidirectional coverage in CT-7:

- `pull-only`: a local edit is reverted to the cloud canonical; a local-only
  file is removed; a cloud deletion removes the local copy; no upload ever
  occurs.
- `push-only`: a remote edit is overwritten by the local canonical; a
  cloud-only file is removed; a local deletion removes the cloud copy; no
  download ever occurs.
- `two-way`: keep-both behavior is unchanged; one-way gates never fire.
- Mode change mid-run converges to the new mode's steady state.
- Mixed per-profile modes stay isolated (one `two-way`, several `pull-only`).
- `self_write_cache` still suppresses echoes in every mode.
- A one-way mode never activates for a profile without an explicit `syncMode`
  opt-in (never inferred).

## Cross-references

- Intent + priority: `docs/plans/core.md §2.5`.
- Pipeline mechanics + conflict/loop-prevention detail: `data-flow.md`.
- Execution checklist: `docs/tasks/core.md` C8-59…C8-66.
- Roadmap placement: `docs/tasks/README.md` Wave 8.
- Safety invariant: `AGENTS.md §4`.
