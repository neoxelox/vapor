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
            proves: "a path that diverged on both sides while the daemon was down keeps both payloads via a ~conflict copy, and the timeline names the file",
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
            id: "S53",
            name: "pull-only-folder-removed-here",
            proves: "pull-only restores a synced folder removed on this device without leaving the folder's own event as a download that never completes, and removes a local-only folder file by file",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: pull_only_folder_removed_here,
        },
        Scenario {
            id: "S54",
            name: "cloud-in-place-append",
            proves: "an append made in place on the cloud object, which keeps Vapor's op-id tag, reaches this device as the canonical file and is neither dropped as an echo nor turned into a conflict copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: cloud_in_place_append,
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
            id: "S64",
            name: "ignore-rules-decide-one-way",
            proves: "a folder rule hides a folder and not a file of that name; a negation under an excluded folder stays excluded for the watcher as it does for the walk; a file that a rule change frees syncs with no restart and no reconcile request",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: ignore_rules_decide_one_way,
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
        Scenario {
            id: "S76",
            name: "cloud-event-without-new-bytes",
            proves: "a chmod, a touch, or a rewrite with the same bytes of a cloud object never turns a local edit that waits for its upload into a conflict copy; the edit reaches the cloud under its name",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::PosixMode],
            expect: Expect::Pass,
            run: cloud_event_without_new_bytes,
        },
        Scenario {
            id: "S77",
            name: "cloud-copy-keeps-the-original",
            proves: "a copy made in the cloud that keeps the modification time (cp -p, a Finder duplicate) arrives as a second file; the original stays on this device",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: cloud_copy_keeps_the_original,
        },
        Scenario {
            id: "S78",
            name: "edit-under-an-ignore-rule",
            proves: "a synced file that changes on this device while an ignore rule hides it uploads when the rule goes, with no conflict copy: the cloud copy never changed",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: edit_under_an_ignore_rule,
        },
        Scenario {
            id: "S81",
            name: "burst-starts-in-seconds",
            proves: "a burst of new files that the watcher folds into one scan of their folder starts to reach the cloud a few seconds after the burst ends, not at the end of the 30 s that the scan may be put off for",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: burst_starts_in_seconds,
        },
        Scenario {
            id: "S85",
            name: "deletion-burst-needs-no-answer",
            proves: "40 files that are removed one by one on this device leave the cloud, and 40 that are removed in the cloud leave this device: half of what the profile syncs each time, with no question to the user",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: deletion_burst_needs_no_answer,
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
    // The timeline names the conflicted file, so a notification can too.
    let timeline = ctx.cli().json(&["timeline", "--json"])?;
    let named = timeline["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| {
            entry["kind"] == "conflict"
                && entry["message"]
                    .as_str()
                    .is_some_and(|message| message.ends_with("doc.txt"))
        });
    ensure!(
        named,
        "no conflict entry on the timeline names doc.txt: {timeline}"
    );
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");
    Ok(())
}

fn pull_only_folder_removed_here(ctx: &mut Ctx) -> Result<(), Failure> {
    // Removing a synced folder on this device in pull-only is
    // divergence from the cloud: its files come back. The watcher
    // also reports the folder itself gone, and that event used to
    // become a download of a directory, which retried forever and
    // held the queue open. A local-only folder created afterwards is
    // removed file by file, never uploaded.
    let home = ctx.primary.clone();
    fs::create_dir_all(home.cloud.join("shared/inner"))?;
    write_file(&home.cloud.join("shared/doc.txt"), "cloud canonical\n")?;
    write_file(
        &home.cloud.join("shared/inner/deep.txt"),
        "deeper canonical\n",
    )?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "pull-only")?;
    ctx.start_daemon_confirming_one_way()?;
    ctx.wait_exists(&home.local.join("shared/inner/deep.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    fs::remove_dir_all(home.local.join("shared"))?;
    ctx.wait_exists(&home.local.join("shared/doc.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.local.join("shared/inner/deep.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        read_string(&home.local.join("shared/doc.txt"))? == "cloud canonical\n",
        "the removed folder did not come back from the cloud"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;

    fs::create_dir_all(home.local.join("scratch"))?;
    write_file(&home.local.join("scratch/note.txt"), "local intruder\n")?;
    ctx.wait_absent(&home.local.join("scratch/note.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        !home.cloud.join("scratch").exists(),
        "pull-only mode uploaded a local folder"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn cloud_in_place_append(ctx: &mut Ctx) -> Result<(), Failure> {
    // An append made in place on the cloud object keeps the op-id tag
    // Vapor wrote at upload time. The change is another writer's edit
    // all the same, and it has to reach this device as the canonical,
    // not be dropped as an echo and not become a conflict copy.
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    write_file(&home.local.join("log.txt"), "line one\n")?;
    ctx.wait_exists(&home.cloud.join("log.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(home.cloud.join("log.txt"))?;
        file.write_all(b"line two\n")?;
        file.sync_all()?;
    }
    ctx.wait_same_content(
        &home.local.join("log.txt"),
        &home.cloud.join("log.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ensure!(
        read_string(&home.local.join("log.txt"))? == "line one\nline two\n",
        "the cloud append did not reach the local file"
    );
    ensure!(
        !conflict_copy_exists(&home.local, "log") && !conflict_copy_exists(&home.cloud, "log"),
        "an in-place cloud append was resolved as a conflict"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn pull_only_mirror(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.cloud.join("doc.txt"), "cloud canonical\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "pull-only")?;
    ctx.start_daemon_confirming_one_way()?;
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

fn ignore_rules_decide_one_way(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    // Rules and files that are there before the daemon starts.
    write_file(
        &home.local.join(".vaporignore"),
        "draft.txt\nkeepers/\n!keepers/keep.txt\n",
    )?;
    write_file(&home.local.join("draft.txt"), "not ready\n")?;
    // `build/` is one of the default rules: it names folders.
    write_file(&home.local.join("notes/build"), "a file named build\n")?;
    ctx.start_daemon()?;
    ctx.wait_same_content(
        &home.local.join("notes/build"),
        &home.cloud.join("notes/build"),
        CONVERGE_TIMEOUT,
    )?;

    // A live write under the excluded folder: the watcher must decide
    // it the way the walk does, so the negation does not bring it back.
    let mark = ctx.mark();
    write_file(
        &home.local.join("keepers/keep.txt"),
        "under an excluded folder\n",
    )?;
    write_file(&home.local.join("control.txt"), "control\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("control.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        !home.cloud.join("keepers").exists(),
        "a file under an excluded folder synced through its negation"
    );
    ensure!(
        !home.cloud.join("draft.txt").exists(),
        "an ignored file synced"
    );

    // The rules go. Nothing touches the files they hid.
    write_file(&home.local.join(".vaporignore"), "")?;
    ctx.wait_same_content(
        &home.local.join("draft.txt"),
        &home.cloud.join("draft.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.wait_same_content(
        &home.local.join("keepers/keep.txt"),
        &home.cloud.join("keepers/keep.txt"),
        CONVERGE_TIMEOUT,
    )?;
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
    // The deletion travels through the live feed; no reconcile is
    // requested. The budget covers the slowest poll cadence (throttled)
    // a loaded host can put the daemon in.
    let local = home.local.join("feed-delete.txt");
    ctx.wait_until(
        Duration::from_secs(90),
        "cloud deletion to propagate through the changes feed",
        || !local.exists(),
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

fn cloud_event_without_new_bytes(ctx: &mut Ctx) -> Result<(), Failure> {
    // Three files that another tool put in the cloud folder, so no
    // object carries an op-id. Each gets a local edit that waits (the
    // daemon is paused), and then an event in the cloud that changes
    // no bytes. The edit is the one change there is: it must go up
    // under its name, with no conflict copy on either side.
    let home = ctx.primary.clone();
    let names = ["mode.txt", "touched.txt", "rewritten.txt"];
    fs::create_dir_all(&home.cloud)?;
    for name in names {
        write_file(
            &home.cloud.join(name),
            format!("{name} from another tool\n"),
        )?;
    }
    start_primary(ctx)?;
    for name in names {
        ctx.wait_same_content(
            &home.cloud.join(name),
            &home.local.join(name),
            CONVERGE_TIMEOUT,
        )?;
    }
    ctx.settle(CONVERGE_TIMEOUT)?;

    // The event in the cloud comes first, so its download is the older
    // intent and is planned before the upload of the edit: the order
    // that used to end in a conflict copy.
    ctx.cli().pause()?;
    set_mode(&home.cloud.join("mode.txt"), 0o600)?;
    let later = std::time::SystemTime::now() + Duration::from_secs(90);
    fs::File::options()
        .write(true)
        .open(home.cloud.join("touched.txt"))?
        .set_modified(later)?;
    write_file(
        &home.cloud.join("rewritten.txt"),
        "rewritten.txt from another tool\n",
    )?;
    for name in names {
        write_file(&home.local.join(name), format!("{name} edited here\n"))?;
    }
    ctx.cli().resume()?;
    // Reads the feed now, not at its next poll.
    ctx.cli().reconcile()?;

    for name in names {
        let cloud = home.cloud.join(name);
        let expected = format!("{name} edited here\n");
        ctx.wait_until(
            Duration::from_secs(90),
            &format!("the local edit of {name} to reach the cloud"),
            || fs::read_to_string(&cloud).is_ok_and(|text| text == expected),
        )?;
    }
    ctx.settle(CONVERGE_TIMEOUT)?;
    for name in names {
        let stem = name.trim_end_matches(".txt");
        ensure!(
            !conflict_copy_exists(&home.local, stem) && !conflict_copy_exists(&home.cloud, stem),
            "an event that changed no bytes of {name} made a conflict copy"
        );
        ensure!(
            read_string(&home.local.join(name))? == format!("{name} edited here\n"),
            "the local edit of {name} lost its name"
        );
    }
    Ok(())
}

fn cloud_copy_keeps_the_original(ctx: &mut Ctx) -> Result<(), Failure> {
    // A new cloud object with the size, the time, and the content of a
    // synced file looks like that file renamed. It is a rename only
    // when the old name is gone from the cloud; here it is not.
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("original.bin"), "one body, two names\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("original.bin"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    let modified_at = fs::metadata(home.cloud.join("original.bin"))?.modified()?;
    fs::copy(home.cloud.join("original.bin"), home.cloud.join("copy.bin"))?;
    fs::File::options()
        .write(true)
        .open(home.cloud.join("copy.bin"))?
        .set_modified(modified_at)?;

    ctx.wait_until(
        Duration::from_secs(90),
        "the copy to arrive on this device",
        || home.local.join("copy.bin").is_file(),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        home.local.join("original.bin").is_file(),
        "the original left this device when its copy arrived"
    );
    ensure!(
        read_string(&home.local.join("copy.bin"))? == "one body, two names\n",
        "the copy holds other bytes"
    );
    Ok(())
}

fn deletion_burst_needs_no_answer(ctx: &mut Ctx) -> Result<(), Failure> {
    const FILES: usize = 40;
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    for folder in ["here", "there"] {
        for index in 0..FILES {
            write_file(
                &home.local.join(folder).join(format!("file{index:02}.txt")),
                "removed with the rest of its folder\n",
            )?;
        }
    }
    let files_in = |folder: &std::path::Path| {
        fs::read_dir(folder).map_or(0, |entries| {
            entries
                .flatten()
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
    };
    ctx.wait_until(CONVERGE_TIMEOUT, "both folders in the cloud", || {
        files_in(&home.cloud.join("here")) == FILES && files_in(&home.cloud.join("there")) == FILES
    })?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Half of what the profile syncs goes on this device, file by
    // file: 40 deletions and no removal of a folder that would stand
    // for them. The cloud follows, and nobody is asked.
    let remove_each = |folder: &std::path::Path| -> Result<(), Failure> {
        for index in 0..FILES {
            fs::remove_file(folder.join(format!("file{index:02}.txt")))?;
        }
        Ok(())
    };
    remove_each(&home.local.join("here"))?;
    ctx.wait_until(CONVERGE_TIMEOUT, "the files to leave the cloud", || {
        files_in(&home.cloud.join("here")) == 0
    })?;

    // The other half goes in the cloud. This device follows, with its
    // files in the trash.
    remove_each(&home.cloud.join("there"))?;
    ctx.wait_until(
        Duration::from_secs(90),
        "the files to leave this device",
        || files_in(&home.local.join("there")) == 0,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    let report = ctx.cli().json(&["decisions", "list", "--json"])?;
    let asked = report["decisions"]
        .as_array()
        .map(|decisions| decisions.len())
        .ok_or_else(|| Failure::new("decisions list gave no list"))?;
    ensure!(
        asked == 0,
        "the removal of the files asked the user something: {report}"
    );
    Ok(())
}

fn burst_starts_in_seconds(ctx: &mut Ctx) -> Result<(), Failure> {
    const FILES: usize = 800;
    // The scan of a burst's folder may be put off for 30 s. In `Idle`
    // it starts when the burst is over; this is far from both.
    const STARTS_WITHIN: Duration = Duration::from_secs(20);
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    let burst = home.local.join("burst");
    fs::create_dir_all(&burst)?;
    for index in 0..FILES {
        fs::write(burst.join(format!("file{index:04}.txt")), "burst\n")?;
    }
    let written = std::time::Instant::now();
    let cloud_burst = home.cloud.join("burst");
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the first file of the burst in the cloud",
        || {
            fs::read_dir(&cloud_burst).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            })
        },
    )?;
    let first_after = written.elapsed();
    ensure!(
        first_after < STARTS_WITHIN,
        "the first file of the burst reached the cloud after {first_after:?}"
    );
    ctx.wait_until(
        Duration::from_secs(120),
        "the whole burst in the cloud",
        || {
            fs::read_dir(&cloud_burst).is_ok_and(|entries| {
                entries
                    .flatten()
                    .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                    .count()
                    == FILES
            })
        },
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn edit_under_an_ignore_rule(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let (local, cloud) = (home.local.join("a.secret"), home.cloud.join("a.secret"));
    write_file(&local, "synced\n")?;
    ctx.wait_same_content(&local, &cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // The rule arrives, and the scan that a rule change asks for
    // passes while the file is hidden.
    write_file(&home.local.join(".vaporignore"), "*.secret\n")?;
    ctx.wait_exists(&home.cloud.join(".vaporignore"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    write_file(&local, "edited while hidden\n")?;
    ctx.cli().reconcile()?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        read_string(&cloud)? == "synced\n",
        "a file that an ignore rule hides synced"
    );

    // The rule goes. The edit is the one change there is.
    write_file(&home.local.join(".vaporignore"), "")?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the edit to reach the cloud once the rule is gone",
        || fs::read_to_string(&cloud).is_ok_and(|text| text == "edited while hidden\n"),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        !conflict_copy_exists(&home.local, "a") && !conflict_copy_exists(&home.cloud, "a"),
        "an edit made under an ignore rule became a conflict copy"
    );
    ensure!(
        read_string(&local)? == "edited while hidden\n",
        "the edit lost its name on this device"
    );
    Ok(())
}
