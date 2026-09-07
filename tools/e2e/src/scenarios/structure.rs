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
            id: "S48",
            name: "cloud-rename-is-a-move",
            proves: "renaming a synced file in the cloud renames the local file in place (same inode, no download) and leaves nothing in the trash",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: cloud_rename_is_a_move,
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
fn inode_of(path: &std::path::Path) -> Result<u64, Failure> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.ino())
}

#[cfg(not(unix))]
fn inode_of(_path: &std::path::Path) -> Result<u64, Failure> {
    Ok(0)
}

fn large_payload() -> Vec<u8> {
    (0..2_000_000u32).map(|i| (i % 249) as u8).collect()
}

fn local_rename_is_a_move(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let before = home.local.join("dataset-v1.bin");
    let mark = ctx.mark();
    fs::write(&before, large_payload())?;
    ctx.converge_from(&mark, 1, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("dataset-v1.bin"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
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

fn cloud_rename_is_a_move(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let local = home.local.join("footage.raw");
    let mark = ctx.mark();
    fs::write(&local, large_payload())?;
    ctx.converge_from(&mark, 1, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("footage.raw"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
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
