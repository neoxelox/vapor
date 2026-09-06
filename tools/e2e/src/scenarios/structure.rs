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
