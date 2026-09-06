//! Crash and recovery: the daemon dies mid-transfer, its durable state
//! disappears, the cloud root vanishes under it, and the shipped
//! `vapord` binary does the same job as `vapor run`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use vapor_shared::constants;

use crate::daemon::DaemonKind;
use crate::host::Need;
use crate::logs;
use crate::oracle::sha256_of;
use crate::scenario::{CONVERGE_TIMEOUT, Ctx, Expect, Scenario, write_file};
use crate::scenarios::sync::start_primary;
use crate::wait;
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S23",
            name: "sigkill-mid-upload",
            proves: "SIGKILL while a large upload is in flight, then restart: the upload completes, the trees match, nothing is duplicated",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: sigkill_mid_upload,
        },
        Scenario {
            id: "S24",
            name: "sigkill-mid-download",
            proves: "SIGKILL while a large download is in flight, then restart: the download completes and the local copy matches the cloud",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: sigkill_mid_download,
        },
        Scenario {
            id: "S32",
            name: "state-db-lost",
            proves: "with the state DB deleted, a restart rebuilds the index from both trees without losing a file or inventing a conflict copy for identical content",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: state_db_lost,
        },
        Scenario {
            id: "S39",
            name: "shutdown-flushes-debounce",
            proves: "a write reported moments before SIGTERM becomes a durable intent at shutdown and uploads right after the restart",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: shutdown_flushes_debounce,
        },
        Scenario {
            id: "S34",
            name: "cloud-root-vanishes",
            proves: "when the cloud root disappears mid-run sync blocks with an Error state, and when it returns work resumes and converges",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: cloud_root_vanishes,
        },
        Scenario {
            id: "S35",
            name: "vapord-binary",
            proves: "the shipped vapord binary starts, syncs, and shuts down cleanly like vapor run",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: vapord_binary,
        },
    ]
}

/// Bytes of the payload used to keep a transfer in flight long enough
/// to crash the daemon in the middle of it. With `bandwidthPercent: 5`
/// of the assumed link capacity the transfer takes several seconds.
const LARGE_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;

fn large_payload(seed: u8) -> Vec<u8> {
    (0..LARGE_PAYLOAD_BYTES)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn temp_file_present(directory: &Path) -> bool {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(constants::provider::TEMP_FILE_PREFIX)
        })
}

fn throttle_bandwidth(ctx: &Ctx) -> Result<(), Failure> {
    ctx.cli()
        .config_set("resourceLimits", r#"{"bandwidthPercent": 5}"#)
        .map(|_| ())
}

fn sigkill_mid_upload(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    throttle_bandwidth(ctx)?;
    let daemon = ctx.start_daemon()?;
    let payload = large_payload(1);
    let target = home.local.join("big.bin");
    write_file(&target, &payload)?;
    // The staging temp in the cloud root is the product's own signal
    // that the upload is in flight.
    wait::wait_until(
        Duration::from_secs(60),
        "the upload staging temp to appear in the cloud root",
        || temp_file_present(&home.cloud),
    )?;
    let mid_transfer = temp_file_present(&home.cloud) && !home.cloud.join("big.bin").exists();
    ctx.kill_daemon(daemon)?;
    ctx.note(format!("kill landed mid-transfer: {mid_transfer}"));
    // Restore full speed so the recovery upload finishes quickly.
    ctx.cli()
        .config_set("resourceLimits", r#"{"bandwidthPercent": 25}"#)?;
    ctx.start_daemon()?;
    let cloud_copy = home.cloud.join("big.bin");
    wait::wait_until(
        Duration::from_secs(90),
        "the upload to complete after the restart",
        || sha256_of(&cloud_copy).ok() == sha256_of(&target).ok(),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let uploads: Vec<_> = fs::read_dir(&home.cloud)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("big"))
        .collect();
    ensure!(
        uploads == ["big.bin"],
        "the cloud root holds something other than the one upload: {uploads:?}"
    );
    if temp_file_present(&home.cloud) {
        ctx.note(
            "an orphaned staging temp from the crash remains until the stale-temp reaper runs",
        );
    }
    Ok(())
}

fn sigkill_mid_download(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    let payload = large_payload(2);
    let source = home.cloud.join("big.bin");
    write_file(&source, &payload)?;
    ctx.configure_scope(&home)?;
    throttle_bandwidth(ctx)?;
    let daemon = ctx.start_daemon()?;
    // The startup reconcile finds the cloud-only file and downloads it.
    wait::wait_until(
        Duration::from_secs(60),
        "the download staging temp to appear in the local root",
        || temp_file_present(&home.local),
    )?;
    let mid_transfer = temp_file_present(&home.local) && !home.local.join("big.bin").exists();
    ctx.kill_daemon(daemon)?;
    ctx.note(format!("kill landed mid-transfer: {mid_transfer}"));
    ctx.cli()
        .config_set("resourceLimits", r#"{"bandwidthPercent": 25}"#)?;
    ctx.start_daemon()?;
    let local_copy = home.local.join("big.bin");
    wait::wait_until(
        Duration::from_secs(90),
        "the download to complete after the restart",
        || sha256_of(&local_copy).ok() == sha256_of(&source).ok(),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        fs::read(&source)? == payload,
        "the cloud original changed during the download recovery"
    );
    Ok(())
}

fn state_db_lost(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    for index in 1..=3 {
        write_file(
            &home.local.join(format!("doc-{index}.txt")),
            format!("payload {index}\n"),
        )?;
    }
    write_file(&home.local.join("nested/deep.txt"), "nested payload\n")?;
    ctx.converge_from(&mark, 4, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("nested/deep.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;

    // Every durable byte the daemon had is gone; both trees survive.
    let state_dir = home.dir.join(constants::runtime::STATE_DIRECTORY_NAME);
    for entry in fs::read_dir(&state_dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(constants::runtime::SQLITE_DATABASE_FILE_NAME) {
            fs::remove_file(entry.path())?;
        }
    }
    ensure!(
        !home.state_db().exists(),
        "state DB still present after deletion"
    );

    ctx.start_daemon()?;
    ctx.settle(Duration::from_secs(60))?;
    let db = ctx.db();
    ensure!(
        db.sync_index_count().unwrap_or(0) >= 4,
        "the rebuilt index does not cover the synced files: {:?}",
        db.sync_index_count()
    );
    let conflicts: Vec<_> = fs::read_dir(&home.local)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("~conflict-"))
        .collect();
    ensure!(
        conflicts.is_empty(),
        "identical content on both sides produced conflict copies: {conflicts:?}"
    );
    Ok(())
}

fn shutdown_flushes_debounce(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    // Let the startup reconcile finish so the queue is idle first.
    ctx.settle(CONVERGE_TIMEOUT)?;
    let db = ctx.db();
    let before = db.enqueue_high_water().unwrap_or(0);
    // A name without an extension debounces on the longest window
    // (several seconds). Give the watcher a moment to deliver the
    // event, then stop the daemon well inside that window.
    write_file(
        &home.local.join("late-note"),
        "written just before shutdown\n",
    )?;
    std::thread::sleep(Duration::from_millis(500));
    ctx.stop_daemon(first)?;
    let after = db.enqueue_high_water().unwrap_or(0);
    ensure!(
        after > before,
        "the shutdown flush did not enqueue the pending write (high water {before} -> {after})"
    );
    ensure!(
        logs::contains(&home.daemon_log(), "intents_flushed=1"),
        "the shutdown log line does not report one flushed intent"
    );
    // The intent is durable; the next daemon executes it before any
    // reconcile could have found the file.
    ctx.start_daemon()?;
    ctx.wait_same_content(
        &home.local.join("late-note"),
        &home.cloud.join("late-note"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle_home(&home, Duration::from_secs(6), CONVERGE_TIMEOUT)?;
    Ok(())
}

fn cloud_root_vanishes(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("before.txt"), "before the outage\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("before.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // The cloud root goes away (an unmounted volume, a renamed folder).
    let parked = ctx.sandbox.root.join("parked-cloud");
    fs::rename(&home.cloud, &parked)?;
    write_file(
        &home.local.join("during.txt"),
        "written during the outage\n",
    )?;
    let cli = ctx.cli();
    cli.wait_run_state("Error", Duration::from_secs(60))?;
    ensure!(
        !home.cloud.exists(),
        "the daemon recreated the cloud root while it was parked"
    );

    // The volume comes back with its content intact.
    fs::rename(&parked, &home.cloud)?;
    cli.wait_run_state("Running", Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("during.txt"), Duration::from_secs(60))?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.allow_warning("Cloud sync directory became unavailable");
    ctx.allow_warning("cloud sync directory");
    Ok(())
}

fn vapord_binary(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    let daemon = ctx.start_daemon_in(&home, DaemonKind::Vapord, true)?;
    let version = ctx.cli().ok(&["version"])?;
    ensure!(!version.trim().is_empty(), "vapor version printed nothing");
    let mark = ctx.mark();
    write_file(&home.local.join("via-vapord.txt"), "served by vapord\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("via-vapord.txt"), CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(daemon)?;
    Ok(())
}
