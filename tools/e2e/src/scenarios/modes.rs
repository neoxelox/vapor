//! Sync modes, deletion semantics, profiles, decisions, and name
//! collisions. Several scenarios here pin behavior the product has
//! today so a change of policy is a deliberate edit, not an accident.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::diskimage::{DiskImage, ImageFs};
use crate::host::Need;
use crate::scenario::{
    CONVERGE_TIMEOUT, Ctx, Expect, OracleMode, Scenario, conflict_copy_exists, read_string,
    tree_contains_content, write_file,
};
use crate::scenarios::sync::start_primary;
use crate::wait;
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S27",
            name: "offline-local-delete",
            proves: "a file deleted locally while the daemon was down is deleted in the cloud on restart when the cloud copy is unchanged, and restored when the cloud copy changed meanwhile",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: offline_local_delete,
        },
        Scenario {
            id: "S28",
            name: "offline-cloud-delete",
            proves: "a file deleted in the cloud while the daemon was down is removed here into the trash on restart when the local copy is unchanged, and re-uploaded when the local copy changed meanwhile",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: offline_cloud_delete,
        },
        Scenario {
            id: "S68",
            name: "left-both-sides-then-put-back",
            proves: "a file that left both folders while the daemon was down, and that is put back on this device with the same bytes, is uploaded and never moved to the trash",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: left_both_sides_then_put_back,
        },
        Scenario {
            id: "S69",
            name: "moved-in-folder-keeps-its-rules",
            proves: "a folder that moves into the sync root with a .gitignore of its own syncs by those rules: the files the rules hide never reach the cloud",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: moved_in_folder_keeps_its_rules,
        },
        Scenario {
            id: "S29",
            name: "push-only-mirror",
            proves: "push-only uploads local content, overwrites a divergent cloud edit, and removes a cloud-only file without ever downloading",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: push_only_mirror,
        },
        Scenario {
            id: "S62",
            name: "one-way-mode-asks-first",
            proves: "setting pull-only prints what the mode does; the daemon then syncs nothing, in either direction, and status says the mode needs a confirmation; after the decision is answered enable the mirror runs",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: one_way_mode_asks_first,
        },
        Scenario {
            id: "S67",
            name: "push-only-keeps-its-spelling",
            proves: "in push-only, a file whose name the cloud respells so that only the case changes gets this device's spelling back in the cloud, with no restart and no reconcile request",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::Unix,
                Need::CaseInsensitiveFs,
            ],
            expect: Expect::Pass,
            run: push_only_keeps_its_spelling,
        },
        Scenario {
            id: "S52",
            name: "push-only-cloud-only-folder",
            proves: "push-only removes a cloud-only folder and everything under it, one entry at a time, and never leaves the delete retrying",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: push_only_cloud_only_folder,
        },
        Scenario {
            id: "S37",
            name: "push-only-same-size-divergence",
            proves: "push-only overwrites a cloud edit that kept the byte count even when the daemon has no index row for the pair",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: push_only_same_size_divergence,
        },
        Scenario {
            id: "S30",
            name: "two-profiles-isolated",
            proves: "two profiles in one daemon sync their own roots with their own durable state and never cross",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: two_profiles_isolated,
        },
        Scenario {
            id: "S80",
            name: "two-profiles-edit-one-file",
            proves: "two profiles on one cloud folder that both change one file before either upload lands keep both edits: each local folder and the cloud folder end with the file and one conflict copy, whichever upload reached the cloud first",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: two_profiles_edit_one_file,
        },
        Scenario {
            id: "S74",
            name: "nested-profile-does-not-run",
            proves: "a profile in vapor.json whose local folder is inside the one of an earlier profile does not run: vapor status lists it as suspended with the reason, neither of its folders is created, and the earlier profile syncs",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: nested_profile_does_not_run,
        },
        Scenario {
            id: "S71",
            name: "pause-one-profile",
            proves: "vapor pause --profile stops one profile while the other keeps syncing, resume --profile lets its file through, reconcile --profile is accepted, and a name with no profile behind it is refused with the names there are",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: pause_one_profile,
        },
        Scenario {
            id: "S82",
            name: "profile-changes-need-no-restart",
            proves: "a profile that vapor profiles adds, disables, enables, or renames while the daemon runs is started, stopped, or renamed within seconds; the other profile keeps syncing and status asks for no restart",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: profile_changes_need_no_restart,
        },
        Scenario {
            id: "S46",
            name: "type-mismatch-decision",
            proves: "a name that is a file here and a folder in the cloud opens a type-mismatch decision and touches nothing; keep-both moves the file to a conflict name and brings the folder down",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: type_mismatch_decision,
        },
        Scenario {
            id: "S50",
            name: "unsyncable-name-decision",
            proves: "a local file whose name is not valid UTF-8 opens an unsyncable-name decision, never reaches the cloud, and the question is withdrawn once the file is renamed; skip stops the asking",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::Unix,
                Need::NonUtf8Names,
            ],
            expect: Expect::Pass,
            run: unsyncable_name_decision,
        },
        Scenario {
            id: "S83",
            name: "unsyncable-cloud-name-decision",
            proves: "a file in the cloud folder whose name has a backslash opens an unsyncable-name decision that says where the file is, never reaches this device, and the question is withdrawn once the file is renamed in the cloud",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::Unix],
            expect: Expect::Pass,
            run: unsyncable_cloud_name_decision,
        },
        Scenario {
            id: "S42",
            name: "trash-keeps-cloud-deletions",
            proves: "a file removed on this device because the cloud deleted it lands in the trash; vapor trash list shows it and vapor trash restore brings it back and re-uploads it",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: trash_keeps_cloud_deletions,
        },
        Scenario {
            id: "S72",
            name: "sync-now-after-a-full-cloud",
            proves: "an upload that finds the cloud folder's volume full is not failed: the profile reports that uploads wait; once there is room, vapor reconcile tries the waiting upload at once, with no wait for the next automatic try",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::DiskImage],
            expect: Expect::Pass,
            run: sync_now_after_a_full_cloud,
        },
        Scenario {
            id: "S49",
            name: "trash-on-the-sync-root-volume",
            proves: "a sync root that is a whole volume of its own gets its trash at <volume>/.vapor/trash/: a cloud deletion is a rename on that volume (same inode, no copy onto the runtime directory's volume), vapor trash list shows it, restore puts it back, and the .vapor directory inside the root never syncs",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::DiskImage],
            expect: Expect::Pass,
            run: trash_on_the_sync_root_volume,
        },
        Scenario {
            id: "S33",
            name: "case-collision-conflict-copy",
            proves: "two cloud files whose names differ only by case both materialize on a case-insensitive local filesystem, the second as a conflict copy",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::CaseInsensitiveFs,
                Need::DiskImage,
            ],
            expect: Expect::Pass,
            run: case_collision,
        },
        Scenario {
            id: "S38",
            name: "case-collision-stable",
            proves: "a cloud name that would alias a differently-cased local file never rewrites either cloud object, materializes once as a conflict copy, and a second reconcile adds nothing",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::CaseInsensitiveFs,
                Need::DiskImage,
            ],
            expect: Expect::Pass,
            run: case_collision_untouched,
        },
    ]
}

fn offline_local_delete(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("keep.txt"), "kept\n")?;
    write_file(&home.local.join("gone.txt"), "deleted offline\n")?;
    write_file(
        &home.local.join("outran.txt"),
        "deleted offline, edited in the cloud\n",
    )?;
    ctx.converge_from(&mark, 3, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("outran.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    fs::remove_file(home.local.join("gone.txt"))?;
    fs::remove_file(home.local.join("outran.txt"))?;
    // The cloud side edits one of them meanwhile.
    write_file(
        &home.cloud.join("outran.txt"),
        "the cloud edited this one after the device deleted it\n",
    )?;
    ctx.start_daemon()?;
    ctx.wait_absent(&home.cloud.join("gone.txt"), Duration::from_secs(60))?;
    ctx.wait_exists(&home.local.join("outran.txt"), Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        !home.local.join("gone.txt").exists() && !home.cloud.join("gone.txt").exists(),
        "an offline deletion of an unchanged file propagates instead of resurrecting it"
    );
    ensure!(
        read_string(&home.local.join("outran.txt"))?
            == "the cloud edited this one after the device deleted it\n",
        "a cloud copy that changed since the last sync is kept and restored"
    );
    ensure!(
        home.cloud.join("keep.txt").is_file(),
        "untouched files stay"
    );
    Ok(())
}

fn offline_cloud_delete(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(
        &home.local.join("gone.txt"),
        "deleted in the cloud offline\n",
    )?;
    write_file(
        &home.local.join("outran.txt"),
        "deleted in the cloud, edited here\n",
    )?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("outran.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    fs::remove_file(home.cloud.join("gone.txt"))?;
    fs::remove_file(home.cloud.join("outran.txt"))?;
    write_file(
        &home.local.join("outran.txt"),
        "this device edited it after the cloud deleted it\n",
    )?;
    ctx.start_daemon()?;
    ctx.wait_absent(&home.local.join("gone.txt"), Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("outran.txt"), Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        !home.cloud.join("gone.txt").exists(),
        "an offline cloud deletion is not undone by a re-upload"
    );
    let trash = ctx.cli().json(&["trash", "list", "--json"])?;
    ensure!(
        trash["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["originalPath"]
                .as_str()
                .is_some_and(|path| path.ends_with("gone.txt")))),
        "the removed file is kept in the trash: {trash}"
    );
    ensure!(
        read_string(&home.cloud.join("outran.txt"))?
            == "this device edited it after the cloud deleted it\n",
        "a local copy that changed since the last sync is kept and re-uploaded"
    );
    Ok(())
}

fn left_both_sides_then_put_back(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("keep.txt"), "kept\n")?;
    write_file(&home.local.join("report.txt"), "the report\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("report.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    // The file leaves both folders while no daemon watches.
    fs::remove_file(home.local.join("report.txt"))?;
    fs::remove_file(home.cloud.join("report.txt"))?;
    ctx.start_daemon()?;
    ctx.settle(Duration::from_secs(60))?;

    // The user puts it back from a copy: the same bytes as the last
    // version that synced. The sync index must not read that as "the
    // cloud deleted it".
    write_file(&home.local.join("report.txt"), "the report\n")?;
    ctx.wait_exists(&home.cloud.join("report.txt"), Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        read_string(&home.local.join("report.txt"))? == "the report\n",
        "the file that was put back stays on this device"
    );
    let trash = ctx.cli().json(&["trash", "list", "--json"])?;
    ensure!(
        trash["entries"]
            .as_array()
            .is_some_and(|entries| entries.is_empty()),
        "nothing went to the trash: {trash}"
    );
    Ok(())
}

fn moved_in_folder_keeps_its_rules(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    // A project made next to the sync root, then moved in whole: the
    // watcher reports the folder and nothing for the files under it.
    let staged = home
        .local
        .parent()
        .ok_or_else(|| Failure::new("the local root has no parent"))?
        .join("staged-project");
    write_file(&staged.join(".gitignore"), "secret.env\nbuild-output/\n")?;
    write_file(&staged.join("secret.env"), "TOKEN=not-for-the-cloud\n")?;
    write_file(&staged.join("src/main.rs"), "fn main() {}\n")?;
    write_file(&staged.join("build-output/app.bin"), "binary\n")?;
    fs::rename(&staged, home.local.join("project"))?;

    ctx.wait_exists(
        &home.cloud.join("project/src/main.rs"),
        Duration::from_secs(60),
    )?;
    ctx.wait_exists(
        &home.cloud.join("project/.gitignore"),
        Duration::from_secs(60),
    )?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        !home.cloud.join("project/secret.env").exists()
            && !home.cloud.join("project/build-output").exists(),
        "what the folder's own rules hide never reaches the cloud"
    );
    // The two hidden entries are on this device only, by the rules.
    ctx.oracle_ignore("secret.env");
    ctx.oracle_ignore("build-output/");
    Ok(())
}

fn one_way_mode_asks_first(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.local)?;
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.local.join("only-here.txt"), "a mirror removes me\n")?;
    write_file(&home.cloud.join("from-cloud.txt"), "cloud canonical\n")?;
    ctx.configure_scope(&home)?;
    let said = ctx.cli().config_set("syncMode", "pull-only")?;
    ensure!(
        said.contains("removes files that are only here") && said.contains("vapor decisions list"),
        "config set must say what pull-only does and that the daemon asks first: {said}"
    );

    let kind = ctx.paths.daemon_kind;
    ctx.start_daemon_in(&home, kind, false)?;
    let cli = ctx.cli();
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to say that the mode needs a confirmation",
        || {
            cli.status().is_some_and(|status| {
                status.run.state == "error"
                    && status
                        .run
                        .reason
                        .contains("pull-only needs your confirmation")
            })
        },
    )?;
    // Long enough for a startup scan and a mirror removal to happen,
    // if anything let them.
    std::thread::sleep(Duration::from_secs(6));
    ensure!(
        home.local.join("only-here.txt").exists(),
        "an unconfirmed mirror removed a local-only file"
    );
    ensure!(
        !home.local.join("from-cloud.txt").exists(),
        "an unconfirmed profile synced in the safe direction"
    );

    ctx.confirm_one_way(&home)?;
    cli.wait_run_state("running", CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.local.join("only-here.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.cloud.join("from-cloud.txt"),
        &home.local.join("from-cloud.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn push_only_mirror(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.cloud.join("cloud-only.txt"), "must go\n")?;
    write_file(
        &home.cloud.join("shared.txt"),
        "the cloud version of shared\n",
    )?;
    fs::create_dir_all(&home.local)?;
    write_file(&home.local.join("shared.txt"), "local version\n")?;
    write_file(&home.local.join("local-only.txt"), "must upload\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "push-only")?;
    ctx.start_daemon_confirming_one_way()?;
    ctx.wait_exists(&home.cloud.join("local-only.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("cloud-only.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.local.join("shared.txt"),
        &home.cloud.join("shared.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ensure!(
        read_string(&home.cloud.join("shared.txt"))? == "local version\n",
        "push-only did not overwrite the divergent cloud edit"
    );
    ensure!(
        !home.local.join("cloud-only.txt").exists(),
        "push-only downloaded a cloud-only file"
    );
    ensure!(
        !tree_contains_content(&home.local, b"the cloud version of shared\n")
            && !tree_contains_content(&home.cloud, b"the cloud version of shared\n"),
        "push-only kept the cloud version somewhere (strict mirror must not)"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn push_only_keeps_its_spelling(ctx: &mut Ctx) -> Result<(), Failure> {
    // The device is the source of truth, and a name's spelling is part
    // of it. On a folder that folds case the cloud's `Report.txt` and
    // the local `report.txt` are one name, so nothing here looks
    // missing: the daemon has to notice the spelling itself.
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    fs::create_dir_all(&home.local)?;
    write_file(&home.local.join("report.txt"), "the local version\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "push-only")?;
    ctx.start_daemon_confirming_one_way()?;
    ctx.wait_exists(&home.cloud.join("report.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    fs::rename(home.cloud.join("report.txt"), home.cloud.join("Report.txt"))?;
    let cloud = home.cloud.clone();
    ctx.wait_until(
        Duration::from_secs(90),
        "the cloud folder spells the name report.txt again",
        || crate::scenarios::structure::stored_names(&cloud) == ["report.txt"],
    )?;
    ctx.settle(Duration::from_secs(60))?;

    ensure!(
        read_string(&home.cloud.join("report.txt"))? == "the local version\n",
        "the cloud file must hold the local content"
    );
    ensure!(
        crate::scenarios::structure::stored_names(&home.local) == ["report.txt"],
        "push-only must not change the local name: {:?}",
        crate::scenarios::structure::stored_names(&home.local)
    );
    Ok(())
}

fn push_only_cloud_only_folder(ctx: &mut Ctx) -> Result<(), Failure> {
    // No provider deletes a tree in one call, so a cloud-only folder
    // is only ever removed by expanding it into its entries. The
    // folder itself, sent to the provider, is refused as non-empty
    // for ever.
    let home = ctx.primary.clone();
    fs::create_dir_all(home.cloud.join("stale/deep"))?;
    write_file(&home.cloud.join("stale/one.txt"), "cloud only\n")?;
    write_file(&home.cloud.join("stale/deep/two.txt"), "cloud only too\n")?;
    fs::create_dir_all(&home.local)?;
    write_file(&home.local.join("keep.txt"), "local canonical\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "push-only")?;
    ctx.start_daemon_confirming_one_way()?;
    ctx.wait_exists(&home.cloud.join("keep.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("stale/deep/two.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("stale/one.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_absent(&home.cloud.join("stale"), CONVERGE_TIMEOUT)?;
    ensure!(
        !home.local.join("stale").exists(),
        "push-only brought a cloud-only folder down"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn push_only_same_size_divergence(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    fs::create_dir_all(&home.local)?;
    write_file(&home.cloud.join("shared.txt"), "cloud version\n")?;
    write_file(&home.local.join("shared.txt"), "local version\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "push-only")?;
    ctx.start_daemon_confirming_one_way()?;
    ctx.wait_same_content(
        &home.local.join("shared.txt"),
        &home.cloud.join("shared.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ensure!(
        read_string(&home.cloud.join("shared.txt"))? == "local version\n",
        "push-only did not overwrite the same-size cloud edit"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn two_profiles_isolated(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let alpha_local = ctx.sandbox.root.join("alpha-local");
    let alpha_cloud = ctx.sandbox.root.join("cloud").join("Alpha");
    let beta_local = ctx.sandbox.root.join("beta-local");
    let beta_cloud = ctx.sandbox.root.join("cloud").join("Beta");
    let profiles = serde_json::json!([
        {
            "id": "alpha",
            "name": "Alpha",
            "enabled": true,
            "localRoot": alpha_local.to_string_lossy(),
            "cloudRoot": alpha_cloud.to_string_lossy(),
        },
        {
            "id": "beta",
            "name": "Beta",
            "enabled": true,
            "localRoot": beta_local.to_string_lossy(),
            "cloudRoot": beta_cloud.to_string_lossy(),
        }
    ]);
    ctx.cli().config_set("profiles", &profiles.to_string())?;
    ctx.start_daemon()?;
    let status = ctx
        .cli()
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    let ids: Vec<&str> = status
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect();
    ensure!(
        ids.contains(&"alpha") && ids.contains(&"beta"),
        "status does not list both profiles: {ids:?}"
    );
    write_file(&alpha_local.join("a.txt"), "alpha payload\n")?;
    write_file(&beta_local.join("b.txt"), "beta payload\n")?;
    ctx.wait_exists(&alpha_cloud.join("a.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&beta_cloud.join("b.txt"), CONVERGE_TIMEOUT)?;
    // Each profile has its own durable state; each stays in its lane.
    let alpha_db = crate::db::StateDb::at(&home.profile_state_db("alpha"));
    let beta_db = crate::db::StateDb::at(&home.profile_state_db("beta"));
    ctx.wait_until(CONVERGE_TIMEOUT, "both profile queues to drain", || {
        alpha_db.queue_drained() && beta_db.queue_drained()
    })?;
    ensure!(
        alpha_db.exists() && beta_db.exists(),
        "per-profile state DBs missing"
    );
    ensure!(
        !alpha_cloud.join("b.txt").exists() && !beta_cloud.join("a.txt").exists(),
        "a profile's file crossed into the other profile's cloud root"
    );
    ensure!(
        !alpha_local.join("b.txt").exists() && !beta_local.join("a.txt").exists(),
        "a profile's file crossed into the other profile's local root"
    );
    let oracle = crate::oracle::TreeOracle::new(&crate::oracle::OracleOptions {
        extra_ignore_rules: Vec::new(),
        compare_mode: cfg!(unix),
    })?;
    for (label, local, cloud) in [
        ("alpha", &alpha_local, &alpha_cloud),
        ("beta", &beta_local, &beta_cloud),
    ] {
        let report = oracle.compare(local, cloud)?;
        ensure!(report.is_clean(), "profile {label}: {}", report.summary(10));
    }
    ctx.set_oracle(OracleMode::Skip(
        "profiles use their own roots; compared explicitly above".to_string(),
    ));
    Ok(())
}

fn two_profiles_edit_one_file(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let alpha_local = ctx.sandbox.root.join("alpha-local");
    let beta_local = ctx.sandbox.root.join("beta-local");
    let shared_cloud = ctx.sandbox.root.join("cloud").join("Shared");
    let profiles = serde_json::json!([
        {
            "id": "alpha",
            "name": "Alpha",
            "enabled": true,
            "localRoot": alpha_local.to_string_lossy(),
            "cloudRoot": shared_cloud.to_string_lossy(),
        },
        {
            "id": "beta",
            "name": "Beta",
            "enabled": true,
            "localRoot": beta_local.to_string_lossy(),
            "cloudRoot": shared_cloud.to_string_lossy(),
        }
    ]);
    ctx.cli().config_set("profiles", &profiles.to_string())?;
    ctx.start_daemon()?;
    let alpha_db = crate::db::StateDb::at(&home.profile_state_db("alpha"));
    let beta_db = crate::db::StateDb::at(&home.profile_state_db("beta"));
    let oracle = crate::oracle::TreeOracle::new(&crate::oracle::OracleOptions {
        extra_ignore_rules: Vec::new(),
        compare_mode: cfg!(unix),
    })?;
    let converged = |ctx: &Ctx, what: &str| {
        ctx.wait_until(Duration::from_secs(90), what, || {
            alpha_db.queue_drained()
                && beta_db.queue_drained()
                && [&alpha_local, &beta_local].iter().all(|local| {
                    oracle
                        .compare(local, &shared_cloud)
                        .is_ok_and(|report| report.is_clean())
                })
        })
    };

    write_file(
        &alpha_local.join("doc.txt"),
        "the version both start from\n",
    )?;
    ctx.wait_exists(&beta_local.join("doc.txt"), CONVERGE_TIMEOUT)?;
    converged(ctx, "the first version on both devices")?;

    // Both devices save while nothing is released, so both uploads
    // start in the same tick.
    ctx.cli().pause()?;
    write_file(&alpha_local.join("doc.txt"), "the edit made on alpha\n")?;
    write_file(&beta_local.join("doc.txt"), "the edit made on beta\n")?;
    std::thread::sleep(Duration::from_secs(3));
    ctx.cli().resume()?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "a conflict copy in the cloud folder",
        || conflict_copy_exists(&shared_cloud, "doc"),
    )?;
    converged(ctx, "both devices to hold what the cloud holds")?;

    for (label, tree) in [
        ("alpha", &alpha_local),
        ("beta", &beta_local),
        ("the cloud folder", &shared_cloud),
    ] {
        for edit in ["the edit made on alpha\n", "the edit made on beta\n"] {
            ensure!(
                tree_contains_content(tree, edit.as_bytes()),
                "{label} lost {edit:?}"
            );
        }
        let copies = crate::scenario::conflict_copies(tree, "doc").count();
        ensure!(
            copies == 1,
            "{label} holds {copies} conflict copies of doc.txt, not one"
        );
    }
    // Which of the two the daemon logs depends on which upload reached
    // the cloud first, and on whether the other one saw it in time.
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");
    ctx.allow_warning("An upload replaced a version another writer had just put in the cloud");
    ctx.set_oracle(OracleMode::Skip(
        "profiles use their own roots; compared explicitly above".to_string(),
    ));
    Ok(())
}

fn nested_profile_does_not_run(ctx: &mut Ctx) -> Result<(), Failure> {
    let outer_local = ctx.sandbox.root.join("outer-local");
    let outer_cloud = ctx.sandbox.root.join("cloud").join("Outer");
    let inner_local = outer_local.join("inner");
    let inner_cloud = ctx.sandbox.root.join("cloud").join("Inner");
    // A file edited by hand: `vapor profiles add` refuses this pair.
    let profiles = serde_json::json!([
        {
            "id": "outer",
            "enabled": true,
            "localRoot": outer_local.to_string_lossy(),
            "cloudRoot": outer_cloud.to_string_lossy(),
        },
        {
            "id": "inner",
            "enabled": true,
            "localRoot": inner_local.to_string_lossy(),
            "cloudRoot": inner_cloud.to_string_lossy(),
        }
    ]);
    ctx.cli().config_set("profiles", &profiles.to_string())?;
    // The state of the whole daemon is the worst one of its profiles,
    // so it does not say `running` here.
    let home = ctx.primary.clone();
    let kind = ctx.paths.daemon_kind;
    ctx.start_daemon_in(&home, kind, false)?;
    ctx.allow_warning("Suspending a profile that cannot run as configured");
    let cli = ctx.cli();
    let profile = |id: &str| {
        cli.status()?
            .profiles
            .into_iter()
            .find(|profile| profile.id == id)
    };
    ctx.wait_until(CONVERGE_TIMEOUT, "outer to run", || {
        profile("outer").is_some_and(|outer| outer.run.state == "running")
    })?;
    let inner = profile("inner").ok_or_else(|| Failure::new("status does not list inner"))?;
    ensure!(
        inner.suspended,
        "the nested profile must not run: {:?}",
        inner.run
    );
    ensure!(
        inner
            .run
            .reason
            .contains("overlaps the one of profile `outer`"),
        "the reason does not name the other profile: {}",
        inner.run.reason
    );

    write_file(&outer_local.join("note.txt"), "outer payload\n")?;
    ctx.wait_exists(&outer_cloud.join("note.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        !inner_local.exists() && !inner_cloud.exists(),
        "a profile that does not run made one of its folders"
    );

    let oracle = crate::oracle::TreeOracle::new(&crate::oracle::OracleOptions {
        extra_ignore_rules: Vec::new(),
        compare_mode: cfg!(unix),
    })?;
    let report = oracle.compare(&outer_local, &outer_cloud)?;
    ensure!(report.is_clean(), "profile outer: {}", report.summary(10));
    ctx.set_oracle(OracleMode::Skip(
        "the profiles use their own roots; the one that runs is compared above".to_string(),
    ));
    Ok(())
}

fn pause_one_profile(ctx: &mut Ctx) -> Result<(), Failure> {
    let alpha_local = ctx.sandbox.root.join("alpha-local");
    let alpha_cloud = ctx.sandbox.root.join("cloud").join("Alpha");
    let beta_local = ctx.sandbox.root.join("beta-local");
    let beta_cloud = ctx.sandbox.root.join("cloud").join("Beta");
    let profiles = serde_json::json!([
        {
            "id": "alpha",
            "name": "Alpha",
            "enabled": true,
            "localRoot": alpha_local.to_string_lossy(),
            "cloudRoot": alpha_cloud.to_string_lossy(),
        },
        {
            "id": "beta",
            "name": "Beta",
            "enabled": true,
            "localRoot": beta_local.to_string_lossy(),
            "cloudRoot": beta_cloud.to_string_lossy(),
        }
    ]);
    ctx.cli().config_set("profiles", &profiles.to_string())?;
    ctx.start_daemon()?;
    let cli = ctx.cli();
    let run_state_of = |id: &str| -> Option<String> {
        cli.status()?
            .profiles
            .into_iter()
            .find(|profile| profile.id == id)
            .map(|profile| profile.run.state)
    };

    // A name with no profile behind it changes nothing, and the answer
    // lists the names there are.
    let unknown = cli.run(&["pause", "--profile", "gamma", "--json"])?;
    ensure!(
        !unknown.success(),
        "a pause for a profile that does not exist was accepted"
    );
    ensure!(
        unknown.stdout.contains("alpha") && unknown.stdout.contains("beta"),
        "the refusal does not list the profiles: {}",
        unknown.combined()
    );

    cli.ok(&["pause", "--profile", "alpha"])?;
    ctx.wait_until(CONVERGE_TIMEOUT, "alpha to report paused", || {
        run_state_of("alpha").as_deref() == Some("paused")
    })?;
    ensure!(
        run_state_of("beta").as_deref() == Some("running"),
        "the pause of alpha reached beta: {:?}",
        run_state_of("beta")
    );

    write_file(&alpha_local.join("a.txt"), "alpha payload\n")?;
    write_file(&beta_local.join("b.txt"), "beta payload\n")?;
    ctx.wait_exists(&beta_cloud.join("b.txt"), CONVERGE_TIMEOUT)?;
    // The comparison of beta's folders is beta's alone.
    cli.ok(&["reconcile", "--profile", "beta"])?;
    ensure!(
        !alpha_cloud.join("a.txt").exists(),
        "the paused profile synced a file"
    );

    cli.ok(&["resume", "--profile", "alpha"])?;
    ctx.wait_exists(&alpha_cloud.join("a.txt"), CONVERGE_TIMEOUT)?;

    let oracle = crate::oracle::TreeOracle::new(&crate::oracle::OracleOptions {
        extra_ignore_rules: Vec::new(),
        compare_mode: cfg!(unix),
    })?;
    for (label, local, cloud) in [
        ("alpha", &alpha_local, &alpha_cloud),
        ("beta", &beta_local, &beta_cloud),
    ] {
        let report = oracle.compare(local, cloud)?;
        ensure!(report.is_clean(), "profile {label}: {}", report.summary(10));
    }
    ctx.set_oracle(OracleMode::Skip(
        "profiles use their own roots; compared explicitly above".to_string(),
    ));
    Ok(())
}

fn profile_changes_need_no_restart(ctx: &mut Ctx) -> Result<(), Failure> {
    let alpha_local = ctx.sandbox.root.join("alpha-local");
    let alpha_cloud = ctx.sandbox.root.join("cloud").join("Alpha");
    let beta_local = ctx.sandbox.root.join("beta-local");
    let beta_cloud = ctx.sandbox.root.join("cloud").join("Beta");
    let cli = ctx.cli();
    let add = |id: &str, local: &Path, cloud: &Path| {
        cli.json(&[
            "profiles",
            "add",
            "--id",
            id,
            "--provider",
            "filesystem",
            "--local",
            &local.to_string_lossy(),
            "--cloud",
            &cloud.to_string_lossy(),
            "--json",
        ])
    };
    add("alpha", &alpha_local, &alpha_cloud)?;
    ctx.start_daemon()?;
    let profile = |id: &str| {
        cli.status()?
            .profiles
            .into_iter()
            .find(|profile| profile.id == id)
    };
    let runs = |id: &str| profile(id).is_some_and(|profile| profile.run.state == "running");
    let no_restart = |after: &str| -> Result<(), Failure> {
        // A status that did not come is no answer.
        let Some(status) = cli.status() else {
            return Err(Failure::new(format!("no status after {after}")));
        };
        ensure!(
            status.restart_required.is_none(),
            "status asks for a restart after {after}: {:?}",
            status.restart_required
        );
        Ok(())
    };
    write_file(&alpha_local.join("a1.txt"), "alpha one\n")?;
    ctx.wait_exists(&alpha_cloud.join("a1.txt"), CONVERGE_TIMEOUT)?;

    // A second profile, added next to one that runs.
    add("beta", &beta_local, &beta_cloud)?;
    ctx.wait_until(CONVERGE_TIMEOUT, "beta to run", || runs("beta"))?;
    no_restart("profiles add")?;
    write_file(&beta_local.join("b1.txt"), "beta one\n")?;
    ctx.wait_exists(&beta_cloud.join("b1.txt"), CONVERGE_TIMEOUT)?;

    // Disabled: beta leaves status and syncs nothing; alpha goes on.
    let disabled = cli.json(&["profiles", "disable", "beta", "--json"])?;
    ensure!(
        disabled["changed"] == true && disabled["profile"]["enabled"] == false,
        "disable reports the profile as it is now: {disabled}"
    );
    ctx.wait_until(CONVERGE_TIMEOUT, "beta to leave status", || {
        cli.status().is_some() && profile("beta").is_none()
    })?;
    no_restart("profiles disable")?;
    ensure!(runs("alpha"), "alpha stopped with beta");
    write_file(&beta_local.join("b2.txt"), "beta two\n")?;
    write_file(&alpha_local.join("a2.txt"), "alpha two\n")?;
    ctx.wait_exists(&alpha_cloud.join("a2.txt"), CONVERGE_TIMEOUT)?;
    ensure!(
        !beta_cloud.join("b2.txt").exists(),
        "the disabled profile synced a file"
    );

    // A new name, taken while the profile runs.
    cli.ok(&["profiles", "rename", "alpha", "Main files"])?;
    ctx.wait_until(CONVERGE_TIMEOUT, "alpha to show its new name", || {
        profile("alpha").is_some_and(|profile| profile.name == "Main files")
    })?;
    no_restart("profiles rename")?;

    // Enabled again: beta starts on the state it left and syncs what
    // changed while it was off.
    cli.ok(&["profiles", "enable", "beta"])?;
    ctx.wait_until(CONVERGE_TIMEOUT, "beta to run again", || runs("beta"))?;
    no_restart("profiles enable")?;
    ctx.wait_exists(&beta_cloud.join("b2.txt"), CONVERGE_TIMEOUT)?;
    write_file(&beta_local.join("b3.txt"), "beta three\n")?;
    ctx.wait_exists(&beta_cloud.join("b3.txt"), CONVERGE_TIMEOUT)?;

    let oracle = crate::oracle::TreeOracle::new(&crate::oracle::OracleOptions {
        extra_ignore_rules: Vec::new(),
        compare_mode: cfg!(unix),
    })?;
    for (label, local, cloud) in [
        ("alpha", &alpha_local, &alpha_cloud),
        ("beta", &beta_local, &beta_cloud),
    ] {
        let report = oracle.compare(local, cloud)?;
        ensure!(report.is_clean(), "profile {label}: {}", report.summary(10));
    }
    ctx.set_oracle(OracleMode::Skip(
        "profiles use their own roots; compared explicitly above".to_string(),
    ));
    Ok(())
}

#[cfg(unix)]
fn unsyncable_name_decision(ctx: &mut Ctx) -> Result<(), Failure> {
    use std::os::unix::ffi::OsStrExt;
    let home = ctx.primary.clone();
    ctx.allow_warning("cannot be synced");
    fs::create_dir_all(&home.local)?;
    let bad = home
        .local
        .join(std::ffi::OsStr::from_bytes(b"report-\xe9.txt"));
    fs::write(&bad, b"latin-1 name")?;
    let skipped = home
        .local
        .join(std::ffi::OsStr::from_bytes(b"keep-\xff.bin"));
    fs::write(&skipped, b"stays here")?;
    write_file(&home.local.join("fine.txt"), "fine\n")?;
    ctx.configure_scope(&home)?;
    let first = ctx.start_daemon()?;
    ctx.wait_exists(&home.cloud.join("fine.txt"), CONVERGE_TIMEOUT)?;
    let cli = ctx.cli();
    let mut open = Vec::new();
    wait::wait_until(
        Duration::from_secs(60),
        "two unsyncable-name decisions",
        || {
            let Ok(report) = cli.json(&["decisions", "list", "--json"]) else {
                return false;
            };
            open = report["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|decision| {
                    decision["kind"] == "unsyncable-name" && decision["choice"].is_null()
                })
                .cloned()
                .collect();
            open.len() == 2
        },
    )?;
    ensure!(
        open.iter().all(|decision| decision["question"]
            .as_str()
            .is_some_and(|question| question.contains("not valid UTF-8"))),
        "the question names the reason: {open:?}"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    let cloud_names: Vec<String> = fs::read_dir(&home.cloud)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ensure!(
        !cloud_names
            .iter()
            .any(|name| name.starts_with("report-") || name.starts_with("keep-")),
        "a name the model cannot carry never reaches the cloud: {cloud_names:?}"
    );

    // Skip one, rename the other; a whole-scope reconcile (a restart
    // runs one) withdraws the renamed one's question.
    let to_skip = open
        .iter()
        .find(|decision| {
            decision["path"]
                .as_str()
                .is_some_and(|path| path.contains("keep-"))
        })
        .ok_or_else(|| Failure::new("no decision for keep-"))?;
    let id = to_skip["id"].as_i64().unwrap_or_default().to_string();
    cli.ok(&["decisions", "resolve", &id, "--choose", "skip"])?;
    fs::rename(&bad, home.local.join("report.txt"))?;
    ctx.wait_exists(&home.cloud.join("report.txt"), CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    ctx.start_daemon()?;
    wait::wait_until(
        Duration::from_secs(60),
        "no open unsyncable-name decision",
        || {
            cli.json(&["decisions", "list", "--json"])
                .ok()
                .and_then(|report| report["decisions"].as_array().cloned())
                .is_some_and(|decisions| {
                    decisions.iter().all(|decision| {
                        decision["kind"] != "unsyncable-name" || !decision["choice"].is_null()
                    })
                })
        },
    )?;
    let history = cli.json(&["decisions", "list", "--all", "--json"])?;
    let choices: Vec<String> = history["decisions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|decision| decision["kind"] == "unsyncable-name")
        .map(|decision| decision["choice"].as_str().unwrap_or_default().to_string())
        .collect();
    ensure!(
        choices.len() == 2
            && choices.contains(&"skip".to_string())
            && choices.contains(&"withdrawn".to_string()),
        "one skipped, one withdrawn, none re-asked: {choices:?}"
    );
    ensure!(skipped.is_file(), "the skipped file stays on this device");
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn unsyncable_cloud_name_decision(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.allow_warning("cannot be synced");
    fs::create_dir_all(&home.cloud)?;
    let bad = home.cloud.join("back\\slash.txt");
    fs::write(&bad, b"stays in the cloud")?;
    write_file(&home.cloud.join("fine.txt"), "fine\n")?;
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    ctx.wait_exists(&home.local.join("fine.txt"), CONVERGE_TIMEOUT)?;
    let cli = ctx.cli();
    // `None` when the command failed: a list that did not come is not
    // an empty list.
    let open_questions = || -> Option<Vec<serde_json::Value>> {
        let report = cli.json(&["decisions", "list", "--json"]).ok()?;
        Some(
            report["decisions"]
                .as_array()?
                .iter()
                .filter(|decision| {
                    decision["kind"] == "unsyncable-name" && decision["choice"].is_null()
                })
                .cloned()
                .collect(),
        )
    };
    wait::wait_until(
        Duration::from_secs(60),
        "an unsyncable-name decision for the cloud file",
        || open_questions().is_some_and(|open| open.len() == 1),
    )?;
    let question = open_questions()
        .and_then(|open| {
            open.first()
                .and_then(|decision| decision["question"].as_str().map(str::to_string))
        })
        .unwrap_or_default();
    ensure!(
        question.starts_with("back\\slash.txt in the cloud folder cannot be synced")
            && question.contains("backslash"),
        "the question says where the file is and why: {question}"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        !home.local.join("back\\slash.txt").exists(),
        "a name the model cannot carry never reaches this device"
    );
    ensure!(bad.is_file(), "the file stays in the cloud");

    // Renamed in the cloud: the file syncs, and the next comparison of
    // both folders finds the name gone and withdraws the question.
    fs::rename(&bad, home.cloud.join("slash.txt"))?;
    ctx.wait_exists(&home.local.join("slash.txt"), CONVERGE_TIMEOUT)?;
    cli.ok(&["reconcile"])?;
    wait::wait_until(
        Duration::from_secs(60),
        "no open unsyncable-name decision",
        || open_questions().is_some_and(|open| open.is_empty()),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

#[cfg(not(unix))]
fn unsyncable_name_decision(_ctx: &mut Ctx) -> Result<(), Failure> {
    Err(Failure::new("non-UTF-8 names need a Unix host"))
}

fn type_mismatch_decision(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.local)?;
    fs::create_dir_all(home.cloud.join("notes"))?;
    write_file(&home.local.join("notes"), "the local file\n")?;
    write_file(
        &home.cloud.join("notes/inner.txt"),
        "inside the cloud folder\n",
    )?;
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    let cli = ctx.cli();
    let mut found = None;
    wait::wait_until(Duration::from_secs(60), "a type-mismatch decision", || {
        let Ok(report) = cli.json(&["decisions", "list", "--json"]) else {
            return false;
        };
        found = report["decisions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|decision| decision["kind"] == "type-mismatch" && decision["choice"].is_null())
            .cloned();
        found.is_some()
    })?;
    let decision = found.ok_or_else(|| Failure::new("no decision"))?;
    ensure!(
        decision["evidence"]["local"] == "file" && decision["evidence"]["cloud"] == "directory",
        "unexpected evidence: {decision}"
    );
    ensure!(
        decision["options"]
            .as_array()
            .is_some_and(|options| options.len() == 3),
        "three answers are offered: {decision}"
    );
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(
        read_string(&home.local.join("notes"))? == "the local file\n"
            && home.cloud.join("notes/inner.txt").is_file(),
        "both sides stay untouched while the question is open"
    );

    let id = decision["id"].as_i64().unwrap_or_default().to_string();
    cli.ok(&["decisions", "resolve", &id, "--choose", "keep-both"])?;
    ctx.wait_exists(&home.local.join("notes/inner.txt"), Duration::from_secs(60))?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        conflict_copy_exists(&home.local, "notes") && conflict_copy_exists(&home.cloud, "notes"),
        "the local file lives on as a conflict copy on both sides"
    );
    ensure!(
        read_string(&home.local.join("notes/inner.txt"))? == "inside the cloud folder\n",
        "the cloud folder came down under the original name"
    );
    ctx.allow_warning("type mismatch");
    Ok(())
}

fn trash_keeps_cloud_deletions(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let local = home.local.join("docs/keep.txt");
    let cloud = home.cloud.join("docs/keep.txt");
    let mark = ctx.mark();
    write_file(&local, "worth keeping\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    fs::remove_file(&cloud)?;
    ctx.wait_absent(&local, Duration::from_secs(90))?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let cli = ctx.cli();
    let listed = cli.json(&["trash", "list", "--json"])?;
    let entries = listed["entries"].as_array().cloned().unwrap_or_default();
    ensure!(entries.len() == 1, "expected one trash entry, got {listed}");
    ensure!(
        entries[0]["reason"] == "cloud-delete"
            && entries[0]["originalPath"].as_str() == Some(local.to_str().unwrap_or_default()),
        "unexpected trash entry: {}",
        entries[0]
    );
    ensure!(
        home.dir.join("trash/default").is_dir(),
        "the managed trash lives under the profile's vapor_dir"
    );

    let id = entries[0]["id"].as_str().unwrap_or_default().to_string();
    let mark = ctx.mark();
    let restored = cli.json(&["trash", "restore", &id, "--json"])?;
    ensure!(
        restored["restoredTo"].as_str() == Some(local.to_str().unwrap_or_default()),
        "restored somewhere else: {restored}"
    );
    ensure!(
        read_string(&local)? == "worth keeping\n",
        "restored content differs"
    );
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let after = cli.json(&["trash", "list", "--json"])?;
    ensure!(
        after["entries"].as_array().is_some_and(Vec::is_empty),
        "a restored entry must leave the trash: {after}"
    );
    Ok(())
}

fn trash_on_the_sync_root_volume(ctx: &mut Ctx) -> Result<(), Failure> {
    let mut home = ctx.primary.clone();
    // The local root is a whole volume of its own, so its trash sits
    // inside the sync root; the runtime directory (and so the trash's
    // home location) stays on the sandbox's volume.
    let image = DiskImage::create(&ctx.sandbox.root, "local-volume", 64, ImageFs::Apfs)?;
    home.local = image.mount_point.clone();
    ctx.register_home(home.clone());
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;

    let local = home.local.join("docs/keep.txt");
    let cloud = home.cloud.join("docs/keep.txt");
    let mark = ctx.mark();
    write_file(&local, "kept on the external volume\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let inode_before = crate::scenarios::structure::inode_of(&local)?;

    fs::remove_file(&cloud)?;
    ctx.wait_absent(&local, Duration::from_secs(90))?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let cli = ctx.cli();
    let listed = cli.json(&["trash", "list", "--json"])?;
    let entries = listed["entries"].as_array().cloned().unwrap_or_default();
    ensure!(entries.len() == 1, "expected one trash entry, got {listed}");
    let id = entries[0]["id"].as_str().unwrap_or_default().to_string();
    let volume_trash = image.mount_point.join(".vapor/trash/default");
    let payload = volume_trash.join(&id).join("keep.txt");
    ensure!(
        payload.is_file(),
        "the entry must live on the sync root's volume at {}",
        payload.display()
    );
    ensure!(
        crate::scenarios::structure::inode_of(&payload)? == inode_before,
        "the discard must be a rename on that volume, not a copy"
    );
    ensure!(
        !home.dir.join("trash/default").join(&id).exists(),
        "nothing may be copied onto the runtime directory's volume"
    );
    ensure!(
        !home.cloud.join(".vapor").exists(),
        "the volume trash sits inside the sync root and must never sync"
    );

    let mark = ctx.mark();
    let restored = cli.json(&["trash", "restore", &id, "--json"])?;
    ensure!(
        restored["restoredTo"].as_str() == Some(local.to_str().unwrap_or_default()),
        "restored somewhere else: {restored}"
    );
    ensure!(
        crate::scenarios::structure::inode_of(&local)? == inode_before,
        "the restore must be a rename too"
    );
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&cloud, CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let after = cli.json(&["trash", "list", "--json"])?;
    ensure!(
        after["entries"].as_array().is_some_and(Vec::is_empty),
        "a restored entry must leave the trash: {after}"
    );
    ctx.keep_alive(Box::new(image));
    Ok(())
}

fn sync_now_after_a_full_cloud(ctx: &mut Ctx) -> Result<(), Failure> {
    use std::io::Write;

    let mut home = ctx.primary.clone();
    // The cloud folder sits on a small volume of its own, next to a
    // filler file that is not part of the sync scope.
    let image = DiskImage::create(&ctx.sandbox.root, "cloud-volume", 64, ImageFs::Apfs)?;
    home.cloud = image.mount_point.join("cloud");
    ctx.register_home(home.clone());
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;
    ctx.allow_warning("The cloud has no room");

    let filler = image.mount_point.join("filler.bin");
    let mut file = fs::File::create(&filler)?;
    let chunk = vec![0xA5u8; 1024 * 1024];
    let mut filled = 0usize;
    while file.write_all(&chunk).is_ok() {
        filled += 1;
        ensure!(filled < 128, "a 64 MiB volume took more than 128 MiB");
    }
    let _ = file.sync_all();
    drop(file);

    let local = home.local.join("docs/big.bin");
    let cloud = home.cloud.join("docs/big.bin");
    write_file(&local, vec![0x5Au8; 2 * 1024 * 1024])?;
    let cli = ctx.cli();
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the profile to say that uploads wait",
        || {
            cli.status()
                .is_some_and(|status| status.run.reason.contains("uploads wait"))
        },
    )?;
    ensure!(
        !cloud.exists(),
        "an upload got through to a volume with no room"
    );
    ensure!(
        ctx.db().failed_intents().unwrap_or(0) == 0,
        "a full cloud must make the upload wait, not fail"
    );

    // Room comes back. The next automatic try is 15 minutes away; the
    // request is what brings the upload forward.
    fs::remove_file(&filler)?;
    cli.reconcile()?;
    ctx.wait_same_content(&local, &cloud, CONVERGE_TIMEOUT)?;
    cli.wait_run_state("running", CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.keep_alive(Box::new(image));
    Ok(())
}

fn case_collision_untouched(ctx: &mut Ctx) -> Result<(), Failure> {
    let mut home = ctx.primary.clone();
    let image = DiskImage::create(
        &ctx.sandbox.root,
        "cs-cloud",
        64,
        ImageFs::ApfsCaseSensitive,
    )?;
    home.cloud = image.mount_point.join("Vapor");
    ctx.register_home(home.clone());
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.cloud.join("Readme.md"), "upper-case readme\n")?;
    write_file(&home.cloud.join("readme.md"), "lower-case readme\n")?;
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;
    // A second reconcile must not manufacture anything new.
    ctx.cli().reconcile()?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;
    ensure!(
        read_string(&home.cloud.join("Readme.md"))? == "upper-case readme\n"
            && read_string(&home.cloud.join("readme.md"))? == "lower-case readme\n",
        "a cloud object was rewritten by the collision"
    );
    let cloud_names: Vec<String> = fs::read_dir(&home.cloud)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    ensure!(
        cloud_names.len() == 2,
        "the collision manufactured extra cloud objects: {cloud_names:?}"
    );
    let local_names: Vec<String> = fs::read_dir(&home.local)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    ensure!(
        local_names.len() == 2
            && local_names
                .iter()
                .filter(|name| name.contains("~conflict-"))
                .count()
                == 1,
        "expected the kept name plus one conflict copy, and nothing more after a second reconcile: {local_names:?}"
    );
    let timeline = ctx.cli().json(&["timeline", "--json"])?;
    ensure!(
        timeline.to_string().contains("\"collision\""),
        "the collision did not land on the timeline: {timeline}"
    );
    ctx.allow_warning("collides with a differently-cased local file");
    ctx.allow_warning("differ only by case");
    ctx.set_oracle(OracleMode::Skip(
        "cloud root is case-sensitive and the local root is not".to_string(),
    ));
    ctx.keep_alive(Box::new(image));
    Ok(())
}

fn case_collision(ctx: &mut Ctx) -> Result<(), Failure> {
    let mut home = ctx.primary.clone();
    // The cloud root lives on a case-sensitive volume, like a real
    // cloud; the local root stays on the sandbox's case-insensitive
    // filesystem.
    let image = DiskImage::create(
        &ctx.sandbox.root,
        "cs-cloud",
        64,
        ImageFs::ApfsCaseSensitive,
    )?;
    home.cloud = image.mount_point.join("Vapor");
    ctx.register_home(home.clone());
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.cloud.join("Readme.md"), "upper-case readme\n")?;
    write_file(&home.cloud.join("readme.md"), "lower-case readme\n")?;
    ensure!(
        home.cloud.join("Readme.md").exists() && home.cloud.join("readme.md").exists(),
        "the disk image is not case-sensitive"
    );
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;
    ensure!(
        tree_contains_content(&home.local, b"upper-case readme\n")
            && tree_contains_content(&home.local, b"lower-case readme\n"),
        "both colliding payloads must exist locally (one as a conflict copy); local has: {:?}",
        fs::read_dir(&home.local)
            .map(|entries| entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    let cloud_listing: Vec<String> = fs::read_dir(&home.cloud)?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let body = fs::read_to_string(entry.path()).unwrap_or_default();
            format!("{name}={body:?}")
        })
        .collect();
    ensure!(
        read_string(&home.cloud.join("Readme.md"))? == "upper-case readme\n"
            && read_string(&home.cloud.join("readme.md"))? == "lower-case readme\n",
        "the cloud objects were rewritten by the collision; cloud holds {cloud_listing:?}"
    );
    ensure!(
        conflict_copy_exists(&home.local, "Readme") || conflict_copy_exists(&home.local, "readme"),
        "the colliding payload did not become a conflict copy"
    );
    // The copy is aliased to its own cloud object: an edit to the copy
    // reaches the colliding cloud name, and the cloud gains no third file.
    let copy = crate::scenario::conflict_copies(&home.local, "Readme")
        .chain(crate::scenario::conflict_copies(&home.local, "readme"))
        .next()
        .ok_or_else(|| Failure::new("no conflict copy"))?;
    let aliased_cloud = if read_string(&copy)? == "lower-case readme\n" {
        home.cloud.join("readme.md")
    } else {
        home.cloud.join("Readme.md")
    };
    write_file(&copy, "edited on the device through the copy\n")?;
    ctx.wait_same_content(&copy, &aliased_cloud, Duration::from_secs(60))?;
    ctx.settle_home(&home, Duration::from_secs(3), Duration::from_secs(60))?;
    let cloud_names: Vec<String> = fs::read_dir(&home.cloud)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    ensure!(
        cloud_names.len() == 2,
        "the cloud keeps exactly its two objects, got {cloud_names:?}"
    );
    // The two trees cannot match by construction on this pair of
    // filesystems; the assertions above are the oracle.
    ctx.set_oracle(OracleMode::Skip(
        "cloud root is case-sensitive and the local root is not".to_string(),
    ));
    // Keep the image alive until the daemons are stopped by the
    // epilogue; then the drop detaches it.
    ctx.keep_alive(Box::new(image));
    Ok(())
}
