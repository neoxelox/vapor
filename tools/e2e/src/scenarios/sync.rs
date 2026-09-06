//! Bidirectional replication through the real filesystem provider:
//! bytes in both directions, keep-both conflicts, ignore symmetry,
//! deletions, special files, and the offline edit the watcher cannot
//! see.

use std::fs;
use std::time::Duration;

use crate::host::Need;
use crate::scenario::{
    CONVERGE_TIMEOUT, Ctx, Expect, Scenario, conflict_copy_exists, read_string,
    tree_contains_content, write_file,
};
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S10",
            name: "bidirectional-round-trip",
            proves: "uploads land in the cloud root byte for byte, a cloud-born file downloads through reconcile, POSIX modes survive both ways",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::PosixMode],
            expect: Expect::Pass,
            run: bidirectional_round_trip,
        },
        Scenario {
            id: "S11",
            name: "keep-both-conflict",
            proves: "a path that diverged on both sides while the daemon was down keeps both payloads via a ~conflict copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: keep_both_conflict,
        },
        Scenario {
            id: "S12",
            name: "pull-only-mirror",
            proves: "pull-only materializes cloud content locally and removes a local-only file without ever uploading it",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: pull_only_mirror,
        },
        Scenario {
            id: "S14",
            name: "symmetric-ignore",
            proves: "ignored names never sync in either direction and never manufacture a conflict copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: symmetric_ignore,
        },
        Scenario {
            id: "S16",
            name: "local-delete-propagates",
            proves: "a plain local rm removes the cloud copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: local_delete_propagates,
        },
        Scenario {
            id: "S17",
            name: "special-files-inert",
            proves: "a FIFO in the watched root never becomes a remote object and never wedges the queue",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Fifo],
            expect: Expect::Pass,
            run: special_files_inert,
        },
        Scenario {
            id: "S19",
            name: "feed-driven-cloud-delete",
            proves: "a cloud-side delete of an uploaded file propagates through the live changes feed with no reconcile",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: feed_driven_cloud_delete,
        },
        Scenario {
            id: "S20",
            name: "offline-same-size-edit",
            proves: "an edit made while the daemon was down that keeps the byte count is found by the startup reconcile and uploaded",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: offline_same_size_edit,
        },
        Scenario {
            id: "S41",
            name: "offline-same-size-cloud-edit",
            proves: "a cloud edit made while the daemon was down that keeps the byte count is found by the startup reconcile and downloaded, with no conflict copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: offline_same_size_cloud_edit,
        },
    ]
}

/// Starts the primary daemon with the scope configured. Most sync
/// scenarios begin this way.
pub fn start_primary(ctx: &mut Ctx) -> Result<usize, Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()
}

fn bidirectional_round_trip(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("up.txt"), "vapor e2e payload\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.local.join("up.txt"),
        &home.cloud.join("up.txt"),
        CONVERGE_TIMEOUT,
    )?;

    // External cloud writes bypass the provider's changes feed, so a
    // reconcile is the designed discovery path for them.
    write_file(&home.cloud.join("from-cloud.txt"), "born in the cloud\n")?;
    ctx.cli().reconcile()?;
    ctx.wait_same_content(
        &home.cloud.join("from-cloud.txt"),
        &home.local.join("from-cloud.txt"),
        CONVERGE_TIMEOUT,
    )?;

    // An executable script must stay executable on the other replica.
    let script = home.local.join("run.sh");
    let mark = ctx.mark();
    write_file(&script, "#!/bin/sh\necho ok\n")?;
    set_mode(&script, 0o755)?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    let uploaded = home.cloud.join("run.sh");
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "uploaded script to carry mode 755",
        || mode_of(&uploaded) == Some(0o755),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn keep_both_conflict(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("doc.txt"), "conflict v1\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("doc.txt"), CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;

    write_file(&home.local.join("doc.txt"), "edited locally while down\n")?;
    write_file(&home.cloud.join("doc.txt"), "edited in cloud while down\n")?;
    ctx.start_daemon()?;
    ctx.cli().reconcile()?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "a keep-both conflict copy to appear",
        || conflict_copy_exists(&home.local, "doc") || conflict_copy_exists(&home.cloud, "doc"),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        tree_contains_content(&home.local, b"edited locally while down\n")
            || tree_contains_content(&home.cloud, b"edited locally while down\n"),
        "the local edit was lost"
    );
    ensure!(
        tree_contains_content(&home.local, b"edited in cloud while down\n")
            || tree_contains_content(&home.cloud, b"edited in cloud while down\n"),
        "the cloud edit was lost"
    );
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");
    Ok(())
}

fn pull_only_mirror(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.cloud.join("doc.txt"), "cloud canonical\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "pull-only")?;
    ctx.start_daemon()?;
    ctx.wait_exists(&home.local.join("doc.txt"), CONVERGE_TIMEOUT)?;
    write_file(&home.local.join("extra.txt"), "local intruder\n")?;
    ctx.cli().reconcile()?;
    ctx.wait_absent(&home.local.join("extra.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        !home.cloud.join("extra.txt").exists(),
        "pull-only mode uploaded a local file"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn symmetric_ignore(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    write_file(&home.local.join(".DS_Store"), "local finder state\n")?;
    write_file(
        &home.cloud.join(".DS_Store"),
        "divergent cloud finder state\n",
    )?;
    write_file(&home.cloud.join("residue.tmp"), "cloud temp residue\n")?;
    let mark = ctx.mark();
    write_file(&home.local.join("control.txt"), "s14 control\n")?;
    ctx.cli().reconcile()?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("control.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        read_string(&home.local.join(".DS_Store"))? == "local finder state\n",
        "local .DS_Store was overwritten from the cloud side"
    );
    ensure!(
        read_string(&home.cloud.join(".DS_Store"))? == "divergent cloud finder state\n",
        "cloud .DS_Store was overwritten from the local side"
    );
    ensure!(
        !home.local.join("residue.tmp").exists(),
        "an ignored cloud-side name downloaded into the local root"
    );
    ensure!(
        !conflict_copy_exists(&home.local, ".DS_Store")
            && !conflict_copy_exists(&home.cloud, ".DS_Store"),
        "ignored divergence manufactured a ~conflict copy"
    );
    Ok(())
}

fn local_delete_propagates(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("gone.txt"), "to be deleted\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("gone.txt"), CONVERGE_TIMEOUT)?;
    fs::remove_file(home.local.join("gone.txt"))?;
    ctx.wait_absent(&home.cloud.join("gone.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn special_files_inert(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let fifo = home.local.join("pipe.fifo");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    ensure!(status.success(), "mkfifo failed");
    let mark = ctx.mark();
    write_file(&home.local.join("control.txt"), "s17 control\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("control.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        !home.cloud.join("pipe.fifo").exists(),
        "a special file produced a remote object"
    );
    Ok(())
}

fn feed_driven_cloud_delete(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(
        &home.local.join("feed-delete.txt"),
        "uploaded then deleted in the cloud\n",
    )?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("feed-delete.txt"), CONVERGE_TIMEOUT)?;
    fs::remove_file(home.cloud.join("feed-delete.txt"))?;
    // A loaded host throttles the daemon to a slow poll cadence. Nudge an
    // immediate poll on every probe, the same nudge a user gets from
    // `vapor flush-now`: the deletion still travels through the live
    // feed; no reconcile is requested.
    let cli = ctx.cli();
    let local = home.local.join("feed-delete.txt");
    ctx.wait_until(
        Duration::from_secs(45),
        "cloud deletion to propagate through the changes feed",
        || {
            cli.flush_now();
            !local.exists()
        },
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn offline_same_size_edit(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let target = home.local.join("offline.txt");
    let mark = ctx.mark();
    write_file(&target, "offline-edit-AAAA")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(&target, &home.cloud.join("offline.txt"), CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    // Same byte count, mtime pushed clearly past what the index recorded.
    write_file(&target, "offline-edit-BBBB")?;
    let future = std::time::SystemTime::now() + Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&target)?
        .set_modified(future)?;
    ctx.start_daemon()?;
    ctx.wait_same_content(
        &target,
        &home.cloud.join("offline.txt"),
        Duration::from_secs(60),
    )?;
    ensure!(
        read_string(&home.cloud.join("offline.txt"))? == "offline-edit-BBBB",
        "the cloud copy does not carry the offline edit"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn offline_same_size_cloud_edit(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let target = home.local.join("offline.txt");
    let cloud = home.cloud.join("offline.txt");
    let mark = ctx.mark();
    write_file(&target, "offline-edit-AAAA")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(&target, &cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    // The cloud side rewrites the file in place (the op-id tag survives),
    // same byte count, mtime clearly past what the index recorded.
    write_file(&cloud, "offline-edit-CCCC")?;
    let future = std::time::SystemTime::now() + Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&cloud)?
        .set_modified(future)?;
    ctx.start_daemon()?;
    ctx.wait_same_content(&target, &cloud, Duration::from_secs(60))?;
    ensure!(
        read_string(&target)? == "offline-edit-CCCC",
        "the local copy does not carry the cloud edit"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        !conflict_copy_exists(&home.local, "offline")
            && !conflict_copy_exists(&home.cloud, "offline"),
        "an unchanged local copy must not become a conflict copy"
    );
    Ok(())
}

pub fn set_mode(path: &std::path::Path, mode: u32) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

pub fn mode_of(path: &std::path::Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
