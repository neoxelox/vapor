# Removal of the mass-delete guard

Status: implemented 2026-11-24

## Goal

The owner asked whether the mass-delete guard can be tuned so that it
stays a safeguard and does not get in the way of ordinary work, and
then chose to remove it: Vapor syncs a folder, and a deletion that the
user made is a deletion to carry to the other side (owner,
2026-11-24). A user gets deletions that sync at once, with no question.

## Facts

What was true when the design started.

- The guard held a burst of deletions behind a `mass-delete` decision
  before the burst reached the other side. A burst was 1 000 deletions
  in 60 s, or a quarter of the synced files and 10 or more
  (`safeguards.massDelete.*`).
- A count cannot tell a mistake from routine work. With the installed
  product and with the Google Drive test account the question opened
  on the maintenance that git runs by itself (about 1 000 loose
  objects), on each `rm -rf node_modules`, and on the rename of a
  folder of 10 files in a small tree, because a folder rename counted
  as deletions. One removal of 360 files asked 8 times, because each
  answer left a smaller tree. Each device asked again for the same
  deletion (`docs/tasks/core.md`, task E-20 at commit `4401992`).
- The guard made deletions slower. On Google Drive the removal of 400
  files took 78 s when the question was answered at once, and 610 s
  when a second device opened its own question later.
- No numbered decision of an earlier spec covered the guard. Seven
  questions about a narrower guard were open in
  `2026-11-24-core-cli-strengthening.md` (design round 1, Q1 to Q7);
  this spec answers them by removing their subject.
- What protects a file without the guard, unchanged by this work:
  - A deletion propagates on the evidence of the sync index only, and a
    file that changed on the other side is kept (`AGENTS.md` §4).
  - A sync folder that is missing or replaced never becomes deletions
    (the root identity, `data-flow.md` §Root identity).
  - A file that Vapor removes on a device goes to Vapor's trash, kept 7
    days by default. Google Drive moves a deleted file to its trash.
- What nothing protects once the guard is gone:
  - The cloud side of the `filesystem` provider has no trash: a
    deletion that syncs there removes the file for good.
  - A device whose user turned Vapor's trash off.
  - A disk that is mounted inside the sync folder and then unmounted.
    Checked in the e2e manual sandbox on 2026-11-24: the watcher
    reports nothing at the unmount, and the next scan of everything
    removed the 6 files of the disk from the cloud folder, as files
    deleted on this device. The guard caught this only for 10 files or
    more and a quarter of the profile.

## Decisions

### Round 1, 2026-11-24

| # | Decision |
|---|---|
| D1 | The mass-delete guard is removed completely: no question, no hold, no settings. A deletion propagates at once under the deletion rule of the sync index, which does not change (owner, 2026-11-24). |
| D2 | The `safeguards` group of `vapor.json` is removed with no migration. A file that still has it gets the note that every unknown key gets. |
| D3 | Nothing new tells the user about a large deletion. `vapor trash list` and the trash of the provider are the undo. |
| D4 | The cloud side of the `filesystem` provider keeps removing a file for good for now. A trash there is a separate feature, recorded as a task. |
| D5 | A disk mounted inside the sync folder is checked as part of this work, and recorded as a task when the check shows a loss. It did. |

## Design

- The guard, its settings, the `mass-delete` decision kind, and its two
  answers (`apply`, `discard`) are gone from the daemon, the CLI, the
  constants, and the Swift mirror.
- Only the guard parked an intent behind a decision. The intent state
  `held`, the column `queue_intents.decision_id`, the decision scope
  `batch`, and the `held` field of `vapor decisions list --json` went
  with it. The state schema is version 9; a database of version 8 is
  set aside and its index is built again, as for every schema change
  before GA (`docs/architecture/state-schema-migrations.md`).
- A deletion takes the path that one deletion always took: the
  move-settle wait, the probe of the other side, the deletion rule, the
  trash. Nothing in that path changed.
- The `approved` flag of an intent stays: the merge after a root answer
  and the answer to a `type-mismatch` question use it.

## Verification

- Tier 1 and Tier E2E pass. The tests and the two scenarios of the
  guard are removed with it (S31, S40).
- Scenario S85 is new: 40 of 80 files are removed one by one on the
  device and leave the cloud, the other 40 are removed in the cloud and
  leave the device, and no decision opens. Against the binaries of
  `4401992` it fails: the guard holds the deletions and the wait for
  the cloud times out.
- The soak ran on the final tree with every fault and the seed of the
  runs before (4217, 15 minutes): 12 phases, 794 operations, 19
  faults, no violation. The 4 faults fewer than before are the trips
  of the guard, which the driver counted as faults. The driver no
  longer answers a question that cannot exist, and still fails a run
  on any decision it does not expect.
- Tier VM ran on the final tree, 14 scenarios pass: one constant and
  one comment changed under `apps/macos`.
- Not checked with the Google Drive test account. The Drive provider
  did not change, and the path of one deletion is the path that the
  sessions of 2026-11-24 used.

## Open points

1. A disk mounted inside the sync folder (`core.md` E-47). The loss is
   real and now has no size at which a question stops it. The fix is a
   design of its own: hold a folder that is on another device than its
   root, as a root is held.
2. A trash for the cloud side of the `filesystem` provider (`core.md`
   P-19).
