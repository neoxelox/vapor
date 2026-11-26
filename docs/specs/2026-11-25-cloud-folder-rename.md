# A folder renamed in the cloud is one rename on the device

Status: implemented 2026-11-26

## Goal

The question was what to do with a local folder that a rename in the
cloud emptied of synced files and that still holds ignored files. The
owner asked why the device does not rename the same folder, which
leaves nothing behind (owner, 2026-11-25). A user gets a rename or a
move of a cloud folder as one step on each device: the folder keeps
everything in it, ignored files included, and no file is sent again.

## Facts

What was true when the design started.

- A rename of a cloud folder is applied file by file. Google Drive has
  one entry in its changes feed for the folder and none for what it
  holds. The provider reports the old place and the new place as
  `FolderMoved`, the engine walks both, and each synced file is moved
  on the device (`core/providers/src/gdrive/mod.rs`,
  `core/daemon/src/remote_sync.rs`).
- That leaves the old folder on the device. An empty one is removed.
  One that holds an ignored file stays for good (`docs/tasks/core.md`
  E-14).
- The same design has two more costs: one device can put the files in
  the trash and download them again (P-15), and a rename at the top of
  the synced folder costs a scan of everything (P-16).
- The device already renames a folder in one step in one case, a
  change of spelling only: the walk renames the local folder, updates
  the index rows under it, and tells the watcher's echo apart
  (`settle_spellings` in `core/daemon/src/reconcile_walk.rs`).
- The provider knows which cloud folder is which by its Drive id, and
  keeps the ids in memory only. After a restart it cannot say what the
  old path of a renamed folder was (P-2).
- The `filesystem` provider has no ids. A renamed cloud folder there is
  one folder that is gone and one that is new.
- The built-in ignore rules hold three names that an OS writes into a
  folder by itself: `.DS_Store`, `Thumbs.db`, `desktop.ini`
  (`DEFAULT_PRE_IGNORE_RULES`). KDE writes `.directory` on Linux; it
  was not ignored and synced as a file (from memory, not checked on a
  Linux desktop).

## Decisions

### Round 1, 2026-11-25

| # | Decision |
|---|---|
| D1 | A folder that is renamed or moved in the cloud becomes one rename of the same folder on the device, with everything in it, ignored files included. Where Vapor cannot pair the old and the new place, it works file by file as before (owner, 2026-11-25). |
| D2 | The Drive ids are kept in the sync index first (P-2), so the pairing also works for a rename that was made while no daemon ran. |
| D3 | Where the device still works file by file, a leftover folder is removed only when everything left in it is one of the three names `.DS_Store`, `Thumbs.db`, `desktop.ini`. They go to the trash with it. A folder with anything else stays. The list of names is replaced by D4. |

### Round 2, 2026-11-25

| # | Decision |
|---|---|
| D4 | `.directory`, the file that KDE writes into a folder for its view settings, joins the built-in ignore rules next to `.DS_Store`, `Thumbs.db`, and `desktop.ini`, and behaves as they do. The list of D3 has these four names (owner, 2026-11-25). D4 replaces the list of D3. |

### Round 3, 2026-11-25

| # | Decision |
|---|---|
| D5 | When the new name is taken on the device by a folder or a file that is not synced, the device does not rename. It works file by file there: the two folders merge, and two different files with one name become a keep-both copy. |
| D6 | When both sides gave one file or one folder a different name or place before either synced, one side has priority and no time is compared. In `two-way` the name and the place on this device stay and go to the cloud. In `pull-only` the cloud's stay. In `push-only` this device's stay (owner, 2026-11-25). The device that syncs last so decides the name for all devices. |
| D7 | D6 covers the name and the place of a file or a folder only: a change of capitals, a rename, a move. Content still keeps both versions, a deletion still follows the sync index, and a file against a folder still asks. |
| D8 | D6 replaces D2 of `2026-11-14-full-review.md`, "the cloud's spelling stays": in `two-way` the spelling on this device stays (owner, 2026-11-25). |
| D9 | A move into another folder is the same one step on the device as a new name. |
| D10 | The `filesystem` provider has no folder ids and keeps working file by file. Pairing a renamed folder there is a task. |
| D11 | In `pull-only` the device folder is renamed as the folder in the cloud was. In `push-only` a rename in the cloud is undone, as every change in the cloud is. |

Before these decisions, a folder that got two different names ended as
two folders on both sides, each with every file: the device renamed
`Photos` to `Pics`, the cloud renamed it to `Pictures`, and both names
stayed (checked in the e2e manual sandbox on 2026-11-25, with the
`filesystem` provider). No time was compared there either. A time is
known only for a rename that a running daemon saw, and for a change in
Google Drive, and the two come from two clocks.

## Design

### What crosses the `Provider` trait

All of it is generic. The engine never reads an id.

- `RemoteEntry::id`, `RemoteChange::id`, and `TransferOutcome::remote_id`
  are an optional opaque text: the id that the backend has for the
  object. A backend with ids gives one for every object, and the id
  stays with the object when the object gets another name or place. A
  backend without ids gives none (`filesystem`).
- `RemoteChangeKind::Moved { from, entry_kind }`: one object, a file or
  a folder, got another name or place. `path` is where it is now and
  `from` is where it was. A backend sends it only when it knows both
  places. `FolderMoved` stays for a folder whose old place is not
  known, and for a place that could not be read.
- `Provider::restore_remote_ids(known)`: the engine hands back the ids
  that it stored, one time, before the first tick. The call reads
  memory only. The default does nothing.
- No capability flag. An entry with an id is the signal.

### What the sync index stores

The state schema is version 10.

- `sync_index.remote_id`: the id of the cloud object of a synced file.
- `sync_folders`: one row for each synced folder that has an id, with
  its local path and its id. The sync index had no row for a folder
  before.
- `name_carries`: one row for each name of this device that still has
  to go to the cloud as one move (see "Priority").

The id of a file is written when a transfer of it ends, and when a
scan compares the file and the listing has another id than the row.
The id of a folder is written when a scan compares a folder that is on
both sides, and when a move of it is applied. A row that moves, or
that leaves, takes its id with it.

### How a start gives the provider its ids

Before the first tick of a profile, the runtime reads every id of the
two tables and calls `restore_remote_ids`. Google Drive fills its two
maps (path to id, id to path) from it. A feed entry that comes after
the start then has an old path, so a rename and a deletion that were
made while no daemon ran are reported as what they are. A restored
path is a hint, as every cached path is: a lookup confirms it with a
request before it is used for a write.

### How the device applies a move

One type, `remote_move::DeviceRenames`, does it: `settle` decides by
the sync mode what a move becomes, and `rename` renames the file or
the folder. It runs on the tick thread, where the scan renames for a
change of spelling. It is a metadata operation: no content is read.

1. It checks that the old path is on this device, as a file or a
   folder, and not as a link.
2. It checks that nothing is queued or in work at the old path or
   under it. Removals that a scan queued for the same move are
   withdrawn.
3. It makes the folders above the new path, then renames with a call
   that fails when the new path is taken
   (`fs_ops::atomic_noclobber_rename`, which takes a folder since this
   change). Nothing is ever replaced.
4. The index rows, the folder rows, and the name aliases under the old
   path get the new path, in the transaction of the tick.
5. It gives the caller the two paths, and the caller records the
   rename as the daemon's own, so the report of the watcher is dropped
   (see "Loop prevention").
6. When the entry left its folder, that folder is asked about as after
   every cloud change that empties a folder (see "The leftover
   folder").

Two callers use it:

- The changes poll, for a `Moved` change. This is the path for a
  rename while the daemon runs, and for a rename while it was stopped
  when the feed still has it.
- The scan, when a cloud entry has an id that the index holds under
  another path. This is the path when the feed has no history (a
  cursor that expired).

When a check fails, or the rename fails, the device works file by file
as before, and one log line says why. For a folder that is a scan of
the folder that held it and of the one that holds it now. For a file
it is the removal of the old name and the download of the new one,
where the planner still turns a moved file into a local rename.

### Loop prevention

A rename of the daemon is kept for the life of a self-write record (30
seconds), as a pair of paths. An event at or under the old path, for a
path that is gone, is the daemon's own. An event at or under the new
path is the daemon's own when it names the renamed folder, a synced
folder in it, or a file that still has the size and the time of its
index row. A file under the new path that was changed after the
rename is a change of the user and syncs. So does a folder that is new
to the index. An event for an old path whose file changed around the
moment of the rename queues the upload of that file under its new
path, so a rename waits for no quiet moment.

The feed also reports a move that the daemon made itself, when it
carried a name of this device to the cloud. The record of that move
drops the entry. When the record is gone, the entry is still no
change: the device has the entry at the new path with its index rows.

### Priority (D6, D7, D8, D11)

No time is read. The index says which side changed: the side whose
path is not the path of the index row.

- Only the cloud changed. `two-way` and `pull-only` rename on the
  device. `push-only` moves the cloud object back.
- Only this device changed. As before: the cloud follows.
- Both changed. `two-way` and `push-only` move the cloud object to the
  name on this device with one `Provider::move_object`. `pull-only`
  renames the entry of this device to the cloud's name.

A move of the cloud object is owed, not made at once: a provider call
never runs on the tick thread. The engine writes a row in
`name_carries` (where the index rows are, where the cloud object is,
and the path on this device) and queues one intent for the path. The
executor makes the move before it plans any intent at or under that
path, moves the index rows to the path of this device, removes the
row, and queues a comparison of the folder (for a file: its upload,
when the file is not what its row holds). This is the mechanism that
carried a change of spelling before; it now takes the three paths from
the row. When the move is refused (the object is gone, or the name is
taken in the cloud), the row and the index rows of the old path go,
and a scan compares the folder as one that never synced, which keeps
the data of both sides.

While the row is there, the three paths are between two places. An
intent for a path under the cloud's name waits one moment for the
move, at most `MOVE_SETTLE_MAX_DEFERRALS` times. A scan leaves the
three paths out of its comparison, keeps the index rows, and queues
the intent again in case something dropped it. Without this, a scan
that runs while the move is on its way reads the two names as two
entries and forgets the rows.

How the engine knows the entry of this device that both sides renamed:

- A change of capitals only: the two names fold to one, as before.
  When the index holds the entry under a third spelling, both sides
  changed, and the spelling of this device stays in `two-way`. This
  works for every provider.
- Another name: the provider has an id, the old path is gone on this
  device, and one entry in the folder of the old path is new to the
  index and holds what the index rows of the old path describe. For a
  file that is its size and time. For a folder it is the size and the
  time of the first 16 files of the index rows, each at the same place
  under the folder. Two candidates, or none, mean no pairing.

A name that both sides changed is paired only when this device renamed
the entry inside its folder. When this device also moved it to another
folder, the device works file by file.

A pair that never synced has no last name to compare with, so no side
changed it: the cloud's spelling stays, as before. So does the merge
after an answer about a root.

An upload that is the new name of a synced file moves the cloud
object. When the cloud has no object at the old name and the index has
an id for it, the object may have another name there that the feed has
not said yet. The upload then waits one settle window, at most
`MOVE_SETTLE_MAX_DEFERRALS` times, and the feed is polled at once.
Without the wait, the device's name would be uploaded as a second
object before the pairing could run.

### The leftover folder (D3, D4)

The four names are `OS_FOLDER_FILES` in `core/shared/src/constants.rs`.
The built-in ignore rules are built from that constant, so the list is
written one time. The app does not need it, so Swift has no mirror of
it.

A folder that a cloud change emptied is asked about when everything
left in it is a file with one of the four names (before: when nothing
was left). The removal runs the checks of every cloud deletion: the
cloud has no folder at that path, and the cloud root is the adopted
one. The files with the four names go to the trash, then the folder is
removed. They go only when nothing else stays in the folder, so a
folder with any other entry is not changed. The four names count
whatever the ignore rules of the user say; a file with one of the
names that is synced (the user took the name out of the rules) is
judged by its index row, as every synced file. The rule holds for
every folder that the cloud no longer has, because the device cannot
tell a folder that was renamed away from one that was removed.

The same code had a fault that the build found and fixed. When a
provider reports the removal of a folder with one entry, as Google
Drive does, a file that the removal kept got an upload, also when an
ignore rule hides it: the folder then came back in the cloud with the
ignored file. The executor asks the ignore rules now, and an ignored
file is kept and not sent.

### What each decision gets

| # | How it is met |
|---|---|
| D1 | `DeviceRenames::rename`, from the poll and from the scan. A check that fails gives the file-by-file path. |
| D2 | `sync_index.remote_id`, `sync_folders`, `restore_remote_ids`. |
| D3, D4 | `OS_FOLDER_FILES`, the emptied-folder check, and the removal of a folder. |
| D5 | The rename fails when the new path is taken. Nothing is replaced. |
| D6, D8 | The priority rule above, for a change of spelling and for another name. |
| D7 | Only names and places changed. Content, deletions, and a file against a folder use the code that they used. |
| D9 | A move into another folder is the same `Moved` change and the same rename. The folders above the new path are made first. |
| D10 | `filesystem` gives no id, so nothing is paired and it works file by file. |
| D11 | `pull-only` renames on the device. `push-only` writes a `name_carries` row that moves the cloud object back. |

### Choices where the decisions leave room

- A rename waits for nothing. When work is queued under the old path,
  the device works file by file at once.
- A cloud rename of a folder that this device removed brings the
  folder back under its new name, as before. The index row of the old
  path is no evidence for a deletion at the new path.
- A rename in the cloud to a name that an ignore rule hides, or from
  one, is not applied as a rename. It is a removal or a new entry, as
  before.
- A folder that this device made in the cloud gets its id in the index
  at the next scan that lists it. Until then a stop of the daemon
  loses the id, and a rename of that folder made while the daemon is
  stopped is applied file by file, by the ids of its files
  (`docs/tasks/core.md` P-21).
- A folder that is renamed on this device still goes to the cloud file
  by file. The decisions are about a rename in the cloud; the other
  direction is a task (`docs/tasks/core.md` E-49).
- On a host that is neither macOS nor Linux, the rename that refuses a
  taken name is not one call for a folder (`docs/tasks/core.md`
  C6-13).

## Verification

What proves each decision. A test in `core/daemon` runs in the tick
harness with a test provider that has ids: the `filesystem` provider
with an id for each object, a feed that names a moved object by its
two paths, and a count of every upload, download, deletion, and move
that a daemon asks for. A stop of the daemon keeps the cloud, the two
folders, and the state database, and a start gets a new provider.

| # | Proof |
|---|---|
| D1 | `a_folder_renamed_in_the_cloud_is_one_rename_on_this_device`, `a_file_renamed_in_the_cloud_is_one_rename_on_this_device` (`core/daemon/src/runtime_cloud_rename_tests.rs`): no request to the cloud, an empty trash, the ignored files under the new name, the index rows moved, a scan that finds nothing. `a_file_changed_under_a_folder_right_after_its_rename_still_syncs` for the loop prevention. `a_renamed_file_that_got_a_new_time_in_the_cloud_is_still_no_change` for a backend that gives a renamed file a new modification time. In the scan: `the_walk_records_the_id_of_each_entry_that_both_sides_hold`, `the_walk_renames_an_entry_whose_id_the_index_holds_under_another_path` (`core/daemon/src/reconcile_walk.rs`). |
| D2 | `a_rename_made_while_the_daemon_was_stopped_is_applied_at_the_next_start` (the provider gets every id back, the feed has the rename) and `a_rename_that_the_feed_no_longer_holds_is_paired_by_id_in_the_scan_of_the_start`. In the provider: `after_a_restart_a_renamed_folder_is_reported_with_its_old_and_its_new_path`, `after_a_restart_a_deletion_in_the_feed_is_applied_by_the_path_that_came_back`, `a_renamed_file_is_reported_as_one_move_with_the_state_of_the_file`, `an_entry_for_an_id_that_the_provider_does_not_know_is_no_move`, `a_path_that_came_back_is_confirmed_before_it_is_used_and_a_listing_of_this_run_stands` (`core/providers/src/gdrive/tests.rs`). The contract suite: `contract_object_ids_are_for_every_object_or_for_none_and_stay_with_a_moved_object`, which locks that a backend without ids reports none (no backend of the suite has ids; `docs/tasks/core.md` CT-13). The index: `the_id_of_a_synced_file_or_folder_follows_it_and_leaves_with_it`. |
| D3, D4 | `a_folder_left_with_only_the_files_an_os_wrote_is_removed_and_they_go_to_the_trash`, `a_folder_left_with_anything_else_stays_as_it_is`, `a_folder_removed_in_the_cloud_leaves_no_folder_with_a_file_of_the_os_behind`, and scenario S86 with the real binaries. |
| D5 | `a_new_name_that_is_taken_on_this_device_is_merged_file_by_file` (the feed has the rename) and `a_new_name_that_is_taken_is_merged_file_by_file_when_the_scan_finds_the_rename`; `a_new_path_that_is_taken_is_never_replaced` (`core/daemon/src/remote_move.rs`); `a_folder_is_renamed_with_what_it_holds_and_never_onto_a_taken_name` (`core/platform/src/fs_ops.rs`). |
| D6, D11 | `a_name_that_both_sides_changed_stays_as_the_sync_mode_says` (`core/daemon/src/remote_move.rs`), `the_walk_leaves_out_the_names_that_are_owed_a_move_in_the_cloud`, `two_way_keeps_the_name_of_this_device_when_both_sides_renamed` (one move for the folder and one for the file, no transfer), `pull_only_keeps_the_name_of_the_cloud_when_both_sides_renamed`, `push_only_keeps_the_name_of_this_device_when_both_sides_renamed`, `pull_only_renames_on_this_device_and_push_only_moves_the_cloud_object_back`, `a_rename_on_this_device_that_meets_a_rename_in_the_cloud_while_the_daemon_runs_ends_as_one_folder`. |
| D7 | The tests of content, deletions, and a file against a folder were not changed and pass. `an_entry_that_this_device_removed_or_that_two_entries_could_be_is_not_paired` shows that a removal on this device is not read as a rename. |
| D8 | `when_both_sides_changed_the_spelling_the_spelling_of_this_device_stays` (`core/daemon/src/runtime.rs`; it asserted the old rule before), `when_both_sides_changed_the_spelling_of_a_folder_the_spelling_of_this_device_stays`, `a_one_way_mode_keeps_the_spelling_of_its_source_when_both_sides_changed_it`, and scenario S87 with the real binaries. |
| D9 | `a_folder_moved_into_another_folder_in_the_cloud_is_one_rename_on_this_device`; the provider test of D2 has the move into another folder. |
| D10 | The scenarios and tests of the `filesystem` provider for a rename in the cloud pass unchanged (S25, S48, S57, S73). S86 and S87 run on it. |

S86 and S87 fail with the binaries of the commit before this change
and pass with this one.

With the Google Drive test account, on the final build (2026-11-26):
a Tier VM guest with the Internet, two profiles on one Drive folder as
two devices, and a folder of six files with one ignored file on each
device. Each change in the cloud was made through the Drive API by a
script, not by Vapor.

| Case | Result on both devices |
|---|---|
| The folder gets another name in the cloud | The new name in about 2 s. The same folder (the same inode), the ignored file still in it, nothing in the trash. |
| The folder is moved into another folder | The same, in about 2 s. |
| One file gets another name | The same file (the same inode) under the new name, in about 2 s. |
| The folder gets another name while the daemon is stopped | The new name 3 s after the start. The provider took back 207 ids from the index. The same inode. |
| Both sides rename the folder while the daemon is stopped (`Pics` on a device, `Pictures` in the cloud) | One folder named `Pics` on both devices and in the cloud. The same inode on each device, nothing in the trash. |

The seven cases of `drive_tests.py` passed on the same build, and the
daemon log of the session has no `ERROR` line.

Not checked:

- Whether a rename in Google Drive gives a file a new modification
  time. The build works for both answers: the pairing does not read
  the time, and the index takes the time that the feed reports when
  the content is the same.
- The ignored file that a removal of a folder kept
  (`an_ignored_file_that_stays_in_a_folder_the_cloud_removed_is_kept_and_not_sent`)
  is proved with the test provider. The fault was read from the code
  and shown there; no run with Google Drive showed it.
- Windows and Linux. The tests ran on macOS. The rename of a folder
  that refuses a taken name has a path of its own on each OS; the
  Linux and Windows paths compile and are run by the CI jobs of those
  hosts only.
- A real watcher for the echo of a renamed folder. The tick harness
  delivers each path that left and each file that came, which is more
  than FSEvents reports. S86 and S87 use the real watcher, on the
  `filesystem` provider, where no folder is renamed in one step.
- The look of the Mac app. Under `apps/macos` this change adds one
  line, `.directory` in the mirror of the ignore list (D4). Tier VM
  ran for the pull request.
- A name that both sides changed while this device also moved the
  entry into another folder (`docs/tasks/core.md` E-48), and a move
  out of a folder on a volume whose trash takes a copy (E-50).

## The review of the pull request

Three reviewers read the whole pull request on 2026-11-26: the guard
removal, this change, and the overwrite check. Each finding was
checked against the code before it was acted on. A fixed finding has a
test that fails without the fix, unless the row says otherwise.

| Area | Finding | Verdict |
|---|---|---|
| Engine | A rename on the device moved the index row of a file whose removal was in work: the file was in the trash, and the cloud copy was then deleted. Narrow: a volume with no trash folder. | Fixed. No rename is made while a removal under the old path is in work (`a_removal_that_is_in_work_under_the_old_path_stops_the_rename`). |
| Engine | A scan that found a rename on both sides forgot the index rows of the folder before they could follow it. A deletion inside the folder then came back. | Fixed (`the_scan_keeps_the_index_rows_when_it_finds_a_rename_of_both_sides`). The case where the cloud entry is in another folder is in task E-48. |
| Engine | One page of the feed with a removal or a new file under a folder, and then the rename of that folder: the first change was dropped until the next scan. | Fixed. The page so far is in the queue before a move is settled, and a folder whose removals waited is compared after its rename (two tests, `..._before_its_folder_got_another_name_...`). |
| Engine | The check for a lost write could start for content of another program, because an upload that sent nothing wrote its own op-id into the index row. | Fixed. The row keeps the tag of the object (`a_row_keeps_the_tag_of_its_object_when_no_bytes_are_sent`). What is left is in task P-12. |
| Engine | Content that the cloud changed under its own name was not fetched when the name of this device stayed. | Fixed for `two-way` (`content_that_the_cloud_changed_under_its_own_name_is_fetched_...`). `push-only` is task E-55. |
| Engine | A removal in work whose queue row was withdrawn ended with a warning, and a comment said that it stops. | Fixed with the first row: no removal in work is withdrawn. The comment is corrected. |
| Engine | A deletion by the user inside the 30 seconds after a rename reads as its echo; a wrong sentence in a doc comment. | Task E-52; the comment is corrected. |
| Providers | The ids of one cloud root went to the provider of another folder after the user took that folder as the root. A change of an old object was then reported at a path of the new root, and a local file could be removed. | Fixed. The provider gets the ids when the root is ready and adopted, a new root forgets them, and the Drive provider drops its paths when its root is another folder (`the_ids_of_one_cloud_root_never_reach_the_provider_of_another_folder`, `a_provider_whose_root_is_another_folder_now_forgets_...`). |
| Providers | A Drive name with a `/` became a nested path, and now a move to it. | Fixed. Such a name has no path in the feed (`a_file_that_gets_a_name_with_a_slash_leaves_its_path_and_is_no_move`). This also closes task P-18. |
| Providers | A move by this device through the Drive provider had no test. | Test added (`a_folder_that_this_device_moves_keeps_the_paths_of_what_it_holds`). |
| Providers | A path that its file left stayed bound to it, so a new file of that name was dropped as a second file of one name. | Fixed (`a_path_that_its_file_left_is_free_for_a_new_file_of_that_name`). |
| Providers | A folder was reported as moved from a path that another folder holds now. | Fixed (`a_folder_whose_old_path_another_folder_has_now_is_no_move_from_that_path`). |
| Providers | The contract text of `note_unrecorded_write` said what the engine does not do. The id half of the contract test runs for no backend. | Text corrected. The test says what it locks, and task CT-13 has the rest. No new test. |
| Providers | A check that runs late reads the wrong holder of the name; a late copy goes to the old folder and name. | Task P-12, two new points. |
| Platform | On Windows a failed folder rename removed an empty folder that another program made. On Linux the fallback had no test and made a stray folder for a move into itself. | Fixed: a failed rename removes nothing on Windows, a move into itself is refused first, and the fallback has a test that the Linux and Windows jobs run. The race that is left on Windows is task C6-13. |
| Documents | The limits of the Drive version check left the task list with task P-13. | Back, in task P-12. |
| Documents | The debug skill named schema 9; one sentence about the guard stayed; two documents and two skills had the rule for the Drive account from before 2026-11-25; two sentences of `AGENTS.md` read as a contradiction; this spec said that nothing under `apps/macos` changed; `.gitignore` had a change with no reason. | Each corrected. No test. |
| Tools | Scenario S86 could fail when one cloud listing lands a tick before another. | Declined. 30 of 30 runs passed. The order that would break it is the product race of task P-15, and the assertion is what shows a regression of the pairing. |

## Open points

None. Every question of the three rounds has its decision.
