//! Names and shapes: renames, moves across subtrees, directory trees,
//! and a path that changes type. These are the operations a real fs
//! watcher reports as fragments the engine has to reassemble.

use std::fs;
use std::time::Duration;

use crate::host::Need;
use crate::scenario::{CONVERGE_TIMEOUT, Ctx, Expect, Scenario, read_string, write_file};
use crate::scenarios::sync::start_primary;
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S25",
            name: "rename-and-move",
            proves: "renaming a file, renaming a directory with children, and moving a file across subtrees converge to the same shape in the cloud",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: rename_and_move,
        },
        Scenario {
            id: "S47",
            name: "local-rename-is-a-move",
            proves: "renaming a synced file locally moves the cloud object in place (same inode, no re-upload) and re-keys the index",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: local_rename_is_a_move,
        },
        Scenario {
            id: "S73",
            name: "cloud-folder-rename-leaves-no-empty-folder",
            proves: "a synced folder that is renamed in the cloud while Vapor does not run: the next start moves its files on this device and removes the old folder, which the move left empty",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: cloud_folder_rename_leaves_no_empty_folder,
        },
        Scenario {
            id: "S86",
            name: "cloud-folder-rename-leftover-folder",
            proves: "a folder that a rename in the cloud emptied of synced files is removed when everything left in it is a file that an OS wrote there by itself (.DS_Store, Thumbs.db), and those files are in the trash; a folder that holds anything else (an ignored build tree) stays as it is",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: cloud_folder_rename_leftover_folder,
        },
        Scenario {
            id: "S87",
            name: "both-sides-case-rename-keeps-this-device",
            proves: "a synced file and a synced folder whose names both sides respelled in two ways while Vapor does not run end under the spelling of this device on both sides, with no conflict copy, no deletion, and nothing in the trash",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::CaseInsensitiveFs,
            ],
            expect: Expect::Pass,
            run: both_sides_case_rename_keeps_this_device,
        },
        Scenario {
            id: "S48",
            name: "cloud-rename-is-a-move",
            proves: "renaming a synced file in the cloud renames the local file in place (same inode, no download) and leaves nothing in the trash",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: cloud_rename_is_a_move,
        },
        Scenario {
            id: "S56",
            name: "local-case-rename",
            proves: "renaming a synced file on this device so that only the case of its name changes moves the cloud object to the new spelling (same inode), with no conflict copy, no deletion, and nothing in the trash",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::Unix,
                Need::CaseInsensitiveFs,
            ],
            expect: Expect::Pass,
            run: local_case_rename,
        },
        Scenario {
            id: "S57",
            name: "cloud-case-rename",
            proves: "renaming a synced file in the cloud so that only the case of its name changes renames the local file to the new spelling (same inode), with no conflict copy, no deletion, and nothing in the trash",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::Unix,
                Need::CaseInsensitiveFs,
            ],
            expect: Expect::Pass,
            run: cloud_case_rename,
        },
        Scenario {
            id: "S58",
            name: "remove-and-put-back",
            proves: "a synced file moved out of the folder and moved back a few seconds later stays in the cloud",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: remove_and_put_back,
        },
        Scenario {
            id: "S26",
            name: "tree-removal-and-type-flip",
            proves: "rm -rf of a tree removes it from the cloud; recreating the name as a file converges to a file on both sides",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: tree_removal_and_type_flip,
        },
        Scenario {
            id: "S36",
            name: "directory-trees",
            proves: "nested directories flow up when created locally and down when created in the cloud; an emptied directory's files are removed",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: directory_trees,
        },
    ]
}

fn rename_and_move(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("notes.txt"), "rename me\n")?;
    write_file(&home.local.join("folder/a.txt"), "child a\n")?;
    write_file(&home.local.join("folder/sub/b.txt"), "child b\n")?;
    write_file(&home.local.join("src/moving.txt"), "move me\n")?;
    ctx.converge_from(&mark, 4, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("folder/sub/b.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("src/moving.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Rename a file.
    fs::rename(home.local.join("notes.txt"), home.local.join("renamed.txt"))?;
    ctx.wait_exists(&home.cloud.join("renamed.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("notes.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        read_string(&home.cloud.join("renamed.txt"))? == "rename me\n",
        "renamed file carries the wrong content in the cloud"
    );

    // Rename a directory with children.
    fs::rename(home.local.join("folder"), home.local.join("moved-folder"))?;
    ctx.wait_exists(&home.cloud.join("moved-folder/sub/b.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("folder/a.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("folder/sub/b.txt"), CONVERGE_TIMEOUT)?;

    // Move a file across subtrees.
    fs::create_dir_all(home.local.join("dst"))?;
    fs::rename(
        home.local.join("src/moving.txt"),
        home.local.join("dst/moving.txt"),
    )?;
    ctx.wait_exists(&home.cloud.join("dst/moving.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("src/moving.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn tree_removal_and_type_flip(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("tree/one.txt"), "one\n")?;
    write_file(&home.local.join("tree/deeper/two.txt"), "two\n")?;
    write_file(&home.local.join("tree/deeper/deepest/three.txt"), "three\n")?;
    ctx.converge_from(&mark, 3, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(
        &home.cloud.join("tree/deeper/deepest/three.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    fs::remove_dir_all(home.local.join("tree"))?;
    ctx.wait_absent(
        &home.cloud.join("tree/deeper/deepest/three.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.wait_absent(&home.cloud.join("tree/one.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // The same name comes back as a file.
    write_file(&home.local.join("tree"), "now a file\n")?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the cloud 'tree' entry to become a file",
        || home.cloud.join("tree").is_file(),
    )?;
    ensure!(
        read_string(&home.cloud.join("tree"))? == "now a file\n",
        "the cloud file 'tree' carries the wrong content"
    );
    ctx.settle(Duration::from_secs(40))?;
    Ok(())
}

fn directory_trees(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    // Local nested creation flows up.
    let mark = ctx.mark();
    write_file(&home.local.join("a/b/c/leaf.txt"), "leaf\n")?;
    write_file(&home.local.join("a/sibling.txt"), "sibling\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("a/b/c/leaf.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("a/sibling.txt"), CONVERGE_TIMEOUT)?;

    // Cloud nested creation flows down through reconcile.
    write_file(&home.cloud.join("x/y/z/remote-leaf.txt"), "remote leaf\n")?;
    ctx.cli().reconcile()?;
    ctx.wait_exists(&home.local.join("x/y/z/remote-leaf.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Emptying a directory removes its files on the other side.
    fs::remove_file(home.local.join("a/b/c/leaf.txt"))?;
    ctx.wait_absent(&home.cloud.join("a/b/c/leaf.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        home.cloud.join("a/sibling.txt").is_file(),
        "removing one file took its sibling with it"
    );
    Ok(())
}

#[cfg(unix)]
pub(crate) fn inode_of(path: &std::path::Path) -> Result<u64, Failure> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.ino())
}

#[cfg(not(unix))]
pub(crate) fn inode_of(_path: &std::path::Path) -> Result<u64, Failure> {
    Ok(0)
}

/// The names in `directory` as the filesystem stores them, without
/// Vapor's own files.
pub(crate) fn stored_names(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with(".vapor"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What a change in the spelling of a name must never leave behind.
fn ensure_nothing_but_a_rename(ctx: &Ctx) -> Result<(), Failure> {
    let trash = ctx.cli().json(&["trash", "list", "--json"])?;
    ensure!(
        trash["entries"].as_array().is_some_and(Vec::is_empty),
        "a rename must not put anything in the trash: {trash}"
    );
    let decisions = ctx.cli().json(&["decisions", "list", "--json"])?;
    ensure!(
        decisions["decisions"].as_array().is_some_and(Vec::is_empty),
        "a rename must not ask a question: {decisions}"
    );
    Ok(())
}

fn local_case_rename(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("report.txt"), "quarterly numbers\n")?;
    write_file(&home.local.join("other.txt"), "untouched\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("report.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let cloud_inode = inode_of(&home.cloud.join("report.txt"))?;

    fs::rename(home.local.join("report.txt"), home.local.join("Report.txt"))?;
    let cloud = home.cloud.clone();
    ctx.wait_until(
        Duration::from_secs(60),
        "the cloud folder spells the name Report.txt",
        || stored_names(&cloud) == ["Report.txt", "other.txt"],
    )?;
    ctx.settle(Duration::from_secs(60))?;

    ensure!(
        stored_names(&home.local) == ["Report.txt", "other.txt"],
        "the local folder must hold exactly the renamed file: {:?}",
        stored_names(&home.local)
    );
    ensure!(
        stored_names(&home.cloud) == ["Report.txt", "other.txt"],
        "the cloud folder must hold exactly the renamed file: {:?}",
        stored_names(&home.cloud)
    );
    ensure!(
        inode_of(&home.cloud.join("Report.txt"))? == cloud_inode,
        "the cloud object must be the same one moved, not a re-upload"
    );
    ensure!(
        read_string(&home.cloud.join("Report.txt"))? == "quarterly numbers\n",
        "payload intact"
    );
    ensure_nothing_but_a_rename(ctx)
}

fn cloud_case_rename(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("notes.txt"), "meeting minutes\n")?;
    write_file(&home.local.join("other.txt"), "untouched\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("notes.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let local_inode = inode_of(&home.local.join("notes.txt"))?;

    fs::rename(home.cloud.join("notes.txt"), home.cloud.join("Notes.txt"))?;
    let local = home.local.clone();
    ctx.wait_until(
        Duration::from_secs(90),
        "the local folder spells the name Notes.txt",
        || stored_names(&local) == ["Notes.txt", "other.txt"],
    )?;
    ctx.settle(Duration::from_secs(60))?;

    ensure!(
        stored_names(&home.cloud) == ["Notes.txt", "other.txt"],
        "the cloud folder must hold exactly the renamed file: {:?}",
        stored_names(&home.cloud)
    );
    ensure!(
        stored_names(&home.local) == ["Notes.txt", "other.txt"],
        "the local folder must hold exactly the renamed file: {:?}",
        stored_names(&home.local)
    );
    ensure!(
        inode_of(&home.local.join("Notes.txt"))? == local_inode,
        "the local file must be the same one renamed, not a download"
    );
    ensure!(
        read_string(&home.local.join("Notes.txt"))? == "meeting minutes\n",
        "payload intact"
    );
    ensure_nothing_but_a_rename(ctx)
}

fn remove_and_put_back(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("keep.txt"), "do not lose me\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("keep.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Out for longer than the debounce window, back before the delete
    // has settled: an undo in the Finder a few seconds after a delete.
    let elsewhere = home.dir.join("keep.txt.elsewhere");
    let db = ctx.db();
    let enqueued_before = db.enqueue_high_water().unwrap_or(0);
    fs::rename(home.local.join("keep.txt"), &elsewhere)?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the removal became a queued deletion",
        || db.enqueue_high_water().unwrap_or(0) > enqueued_before,
    )?;
    fs::rename(&elsewhere, home.local.join("keep.txt"))?;
    // Long enough for the delete's settle window and its probe.
    std::thread::sleep(Duration::from_secs(12));
    ctx.settle(Duration::from_secs(60))?;

    ensure!(
        read_string(&home.cloud.join("keep.txt"))? == "do not lose me\n",
        "the cloud copy must survive a removal that was undone"
    );
    ensure!(
        read_string(&home.local.join("keep.txt"))? == "do not lose me\n",
        "the local copy is back"
    );
    Ok(())
}

fn large_payload() -> Vec<u8> {
    (0..2_000_000u32).map(|i| (i % 249) as u8).collect()
}

/// Waits until the daemon holds no event for the file of a rename
/// scenario. The names end in `.bin` and `.raw`, which debounce on the
/// longest window (4 s), and the daemon reports a file of a sync
/// folder it has just made a second time. That event must be through
/// before the rename: an upload that is planned in the moment of a
/// cloud rename finds the old name gone and removes the local file,
/// and the rename is then a download.
fn settle_past_the_longest_debounce(ctx: &Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.settle_home(&home, Duration::from_secs(6), CONVERGE_TIMEOUT)
}

fn local_rename_is_a_move(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let before = home.local.join("dataset-v1.bin");
    let mark = ctx.mark();
    fs::write(&before, large_payload())?;
    ctx.converge_from(&mark, 1, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("dataset-v1.bin"), CONVERGE_TIMEOUT)?;
    settle_past_the_longest_debounce(ctx)?;
    let cloud_inode = inode_of(&home.cloud.join("dataset-v1.bin"))?;

    let after = home.local.join("archive/dataset-final.bin");
    fs::create_dir_all(after.parent().unwrap())?;
    fs::rename(&before, &after)?;
    ctx.wait_exists(
        &home.cloud.join("archive/dataset-final.bin"),
        Duration::from_secs(60),
    )?;
    ctx.wait_absent(&home.cloud.join("dataset-v1.bin"), Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        inode_of(&home.cloud.join("archive/dataset-final.bin"))? == cloud_inode,
        "the cloud object must be the same one moved, not a re-upload"
    );
    ensure!(
        read_string(&after).is_err() || fs::read(&after)? == large_payload(),
        "payload intact"
    );
    let log = fs::read_to_string(home.daemon_log()).unwrap_or_default();
    ensure!(
        log.contains("Moved the cloud object instead of re-uploading"),
        "the daemon log must record the move"
    );
    Ok(())
}

fn cloud_folder_rename_leaves_no_empty_folder(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("albums/first.txt"), "the first photo\n")?;
    write_file(
        &home.local.join("albums/second.txt"),
        "the second photo, a longer one\n",
    )?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("albums/second.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Another device renames the folder while Vapor does not run. No
    // event says that the old folder went away: the scan of the next
    // start finds the files under the new name.
    ctx.stop_daemon(first)?;
    fs::rename(home.cloud.join("albums"), home.cloud.join("archive"))?;
    ctx.start_daemon()?;
    ctx.wait_exists(
        &home.local.join("archive/first.txt"),
        Duration::from_secs(90),
    )?;
    ctx.wait_exists(
        &home.local.join("archive/second.txt"),
        Duration::from_secs(90),
    )?;
    // The cloud has no `albums` any more, and the local one is empty.
    ctx.wait_absent(&home.local.join("albums"), Duration::from_secs(90))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        read_string(&home.local.join("archive/first.txt"))? == "the first photo\n",
        "the file under the new folder name has another content"
    );
    Ok(())
}

fn cloud_folder_rename_leftover_folder(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("albums/first.txt"), "the first photo\n")?;
    write_file(
        &home.local.join("albums/second.txt"),
        "the second photo, a longer one\n",
    )?;
    write_file(&home.local.join("projects/app/main.txt"), "the source\n")?;
    ctx.converge_from(&mark, 3, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("albums/second.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("projects/app/main.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // While Vapor does not run: the file managers of two systems write
    // their view settings into one folder, the other folder gets a
    // build tree that an ignore rule hides, and another device gives
    // both folders another name. The scan of the next start moves the
    // synced files, and each old folder is left behind.
    ctx.stop_daemon(first)?;
    write_file(&home.local.join("albums/.DS_Store"), "a view\n")?;
    write_file(&home.local.join("albums/Thumbs.db"), "thumbnails\n")?;
    write_file(&home.local.join("projects/app/.DS_Store"), "a view\n")?;
    write_file(
        &home.local.join("projects/app/node_modules/pkg/index.js"),
        "a dependency\n",
    )?;
    fs::rename(home.cloud.join("albums"), home.cloud.join("archive"))?;
    fs::rename(
        home.cloud.join("projects/app"),
        home.cloud.join("projects/tool"),
    )?;
    ctx.start_daemon()?;
    ctx.wait_exists(
        &home.local.join("archive/second.txt"),
        Duration::from_secs(90),
    )?;
    ctx.wait_exists(
        &home.local.join("projects/tool/main.txt"),
        Duration::from_secs(90),
    )?;
    // Nothing of the user is left in `albums`: the folder goes.
    ctx.wait_absent(&home.local.join("albums"), Duration::from_secs(90))?;
    ctx.settle(Duration::from_secs(60))?;

    let trash = ctx.cli().json(&["trash", "list", "--json"])?;
    let mut trashed: Vec<String> = trash["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["originalPath"].as_str())
                .filter_map(|path| path.rsplit(['/', '\\']).next())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    trashed.sort();
    ensure!(
        trashed == [".DS_Store", "Thumbs.db"],
        "the two files of the OS are in the trash, and no synced file is: {trash}"
    );
    // The build tree is the user's: its folder stays, with everything
    // that was left in it.
    ensure!(
        read_string(&home.local.join("projects/app/node_modules/pkg/index.js"))?
            == "a dependency\n",
        "the ignored build tree was touched"
    );
    ensure!(
        read_string(&home.local.join("projects/app/.DS_Store"))? == "a view\n",
        "a folder that holds anything else must stay as it is"
    );
    ensure!(
        !home.local.join("projects/app/main.txt").exists(),
        "the synced file moved to the new folder name"
    );
    let decisions = ctx.cli().json(&["decisions", "list", "--json"])?;
    ensure!(
        decisions["decisions"].as_array().is_some_and(Vec::is_empty),
        "a leftover folder must not ask a question: {decisions}"
    );
    Ok(())
}

fn both_sides_case_rename_keeps_this_device(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("draft.txt"), "chapter one\n")?;
    write_file(&home.local.join("photos/a.txt"), "the first photo\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("photos/a.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Each side gives the file and the folder another spelling before
    // either syncs. No time decides: in two-way the spelling of this
    // device has priority.
    ctx.stop_daemon(first)?;
    fs::rename(home.local.join("draft.txt"), home.local.join("DRAFT.txt"))?;
    fs::rename(home.local.join("photos"), home.local.join("PHOTOS"))?;
    fs::rename(home.cloud.join("draft.txt"), home.cloud.join("Draft.txt"))?;
    fs::rename(home.cloud.join("photos"), home.cloud.join("Photos"))?;
    ctx.start_daemon()?;
    let cloud = home.cloud.clone();
    ctx.wait_until(
        Duration::from_secs(90),
        "the cloud folder spells the names as this device does",
        || stored_names(&cloud) == ["DRAFT.txt", "PHOTOS"],
    )?;
    ctx.settle(Duration::from_secs(60))?;

    ensure!(
        stored_names(&home.local) == ["DRAFT.txt", "PHOTOS"],
        "this device must keep its spelling: {:?}",
        stored_names(&home.local)
    );
    ensure!(
        stored_names(&home.cloud) == ["DRAFT.txt", "PHOTOS"],
        "the cloud must take the spelling of this device: {:?}",
        stored_names(&home.cloud)
    );
    ensure!(
        read_string(&home.cloud.join("DRAFT.txt"))? == "chapter one\n"
            && read_string(&home.cloud.join("PHOTOS/a.txt"))? == "the first photo\n",
        "payload intact"
    );
    ensure_nothing_but_a_rename(ctx)
}

fn cloud_rename_is_a_move(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let local = home.local.join("footage.raw");
    let mark = ctx.mark();
    fs::write(&local, large_payload())?;
    ctx.converge_from(&mark, 1, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("footage.raw"), CONVERGE_TIMEOUT)?;
    settle_past_the_longest_debounce(ctx)?;
    let local_inode = inode_of(&local)?;

    let cloud_after = home.cloud.join("2026/footage-renamed.raw");
    fs::create_dir_all(cloud_after.parent().unwrap())?;
    fs::rename(home.cloud.join("footage.raw"), &cloud_after)?;
    let local_after = home.local.join("2026/footage-renamed.raw");
    ctx.wait_exists(&local_after, Duration::from_secs(90))?;
    ctx.wait_absent(&local, Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        inode_of(&local_after)? == local_inode,
        "the local file must be the same one renamed, not a download"
    );
    let trash = ctx.cli().json(&["trash", "list", "--json"])?;
    ensure!(
        trash["entries"].as_array().is_some_and(Vec::is_empty),
        "a rename must not leave the old name in the trash: {trash}"
    );
    let log = fs::read_to_string(home.daemon_log()).unwrap_or_default();
    ensure!(
        log.contains("Renamed the local file instead of downloading"),
        "the daemon log must record the move"
    );
    Ok(())
}
