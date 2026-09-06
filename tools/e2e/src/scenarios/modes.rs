//! Sync modes, deletion semantics, profiles, safeguards, and name
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
            id: "S29",
            name: "push-only-mirror",
            proves: "push-only uploads local content, overwrites a divergent cloud edit, and removes a cloud-only file without ever downloading",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: push_only_mirror,
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
            id: "S31",
            name: "mass-delete-decision-apply",
            proves: "a burst of local deletions is held whole behind a mass-deletion decision while other work continues; vapor decisions resolve --choose apply releases it",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: mass_delete_guard,
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
            id: "S42",
            name: "trash-keeps-cloud-deletions",
            proves: "a file removed on this device because the cloud deleted it lands in the trash; vapor trash list shows it and vapor trash restore brings it back and re-uploads it",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: trash_keeps_cloud_deletions,
        },
        Scenario {
            id: "S40",
            name: "mass-delete-decision-discard",
            proves: "a burst of cloud deletions is held before it touches this device; --choose discard restores the cloud copies from the local ones",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: mass_delete_discard,
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
    ctx.start_daemon()?;
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

fn push_only_same_size_divergence(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    fs::create_dir_all(&home.cloud)?;
    fs::create_dir_all(&home.local)?;
    write_file(&home.cloud.join("shared.txt"), "cloud version\n")?;
    write_file(&home.local.join("shared.txt"), "local version\n")?;
    ctx.configure_scope(&home)?;
    ctx.cli().config_set("syncMode", "push-only")?;
    ctx.start_daemon()?;
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
            "localSyncDirectory": alpha_local.to_string_lossy(),
            "cloudSyncDirectory": alpha_cloud.to_string_lossy(),
        },
        {
            "id": "beta",
            "name": "Beta",
            "enabled": true,
            "localSyncDirectory": beta_local.to_string_lossy(),
            "cloudSyncDirectory": beta_cloud.to_string_lossy(),
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

/// The one open mass-deletion decision, once the daemon has opened it.
fn wait_open_mass_deletion(cli: &crate::cli::Cli, timeout: Duration) -> Result<i64, Failure> {
    let mut found = None;
    wait::wait_until(
        timeout,
        "the mass-deletion guard to open a decision",
        || {
            let Ok(report) = cli.json(&["decisions", "list", "--json"]) else {
                return false;
            };
            found = report["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|decision| {
                    decision["kind"] == "mass-deletion" && decision["choice"].is_null()
                })
                .and_then(|decision| decision["id"].as_i64());
            found.is_some()
        },
    )?;
    found.ok_or_else(|| Failure::new("no open mass-deletion decision"))
}

fn victims_in(root: &Path) -> usize {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("victim-"))
                .count()
        })
        .unwrap_or(0)
}

/// Twenty synced files; twelve deleted at once. Under the default
/// settings that is a burst above the ratio floor, so the guard holds
/// it behind a decision.
fn seed_victims(ctx: &mut Ctx, home: &crate::sandbox::Home) -> Result<(), Failure> {
    let mark = ctx.mark();
    for index in 0..20 {
        write_file(
            &home.local.join(format!("victim-{index:02}.txt")),
            format!("victim {index}\n"),
        )?;
    }
    ctx.converge_from(&mark, 20, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("victim-19.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    Ok(())
}

fn mass_delete_guard(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    seed_victims(ctx, &home)?;

    for index in 0..12 {
        fs::remove_file(home.local.join(format!("victim-{index:02}.txt")))?;
    }
    let cli = ctx.cli();
    let id = wait_open_mass_deletion(&cli, Duration::from_secs(30))?;
    let decision = cli.json(&["decisions", "show", &id.to_string(), "--json"])?;
    ensure!(
        decision["heldIntents"].as_u64() == Some(12),
        "the whole burst must be held before any of it lands: {decision}"
    );
    ensure!(
        decision["evidence"]["direction"] == "local-to-cloud",
        "wrong direction in the evidence: {decision}"
    );
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.run_state == "Running",
        "a held batch must not pause the daemon (run_state {})",
        status.run_state
    );
    ensure!(
        status.decisions_pending == 1,
        "status must count the open decision, got {}",
        status.decisions_pending
    );
    ensure!(
        victims_in(&home.cloud) == 20,
        "held deletions must not reach the cloud; {} of 20 remain",
        victims_in(&home.cloud)
    );
    // Other work keeps flowing while the question is open.
    let mark = ctx.mark();
    write_file(&home.local.join("meanwhile.txt"), "still syncing\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("meanwhile.txt"), CONVERGE_TIMEOUT)?;
    let timeline = cli.json(&["timeline", "--json"])?;
    ensure!(
        timeline.to_string().contains("\"decision\""),
        "the decision did not land on the timeline"
    );

    cli.ok(&["decisions", "resolve", &id.to_string(), "--choose", "apply"])?;
    ctx.wait_until(
        Duration::from_secs(60),
        "the held deletions to propagate after apply",
        || victims_in(&home.cloud) == 8,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let open = cli.json(&["decisions", "list", "--json"])?;
    ensure!(
        open["decisions"].as_array().is_some_and(Vec::is_empty),
        "the answered decision must leave the open list: {open}"
    );
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.decisions_pending == 0,
        "status still counts a decision"
    );
    ctx.allow_warning("Mass-deletion guard tripped");
    Ok(())
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
        entries[0]["reason"] == "deleted-in-cloud"
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

fn mass_delete_discard(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    seed_victims(ctx, &home)?;

    // Another device empties most of the cloud folder.
    for index in 0..12 {
        fs::remove_file(home.cloud.join(format!("victim-{index:02}.txt")))?;
    }
    let cli = ctx.cli();
    let id = wait_open_mass_deletion(&cli, Duration::from_secs(60))?;
    let decision = cli.json(&["decisions", "show", &id.to_string(), "--json"])?;
    ensure!(
        decision["evidence"]["direction"] == "cloud-to-local",
        "wrong direction in the evidence: {decision}"
    );
    ensure!(
        victims_in(&home.local) == 20,
        "held deletions must not touch this device; {} of 20 remain",
        victims_in(&home.local)
    );
    cli.ok(&[
        "decisions",
        "resolve",
        &id.to_string(),
        "--choose",
        "discard",
    ])?;
    ctx.wait_until(
        Duration::from_secs(60),
        "the discarded deletions to be undone by re-uploading",
        || victims_in(&home.cloud) == 20,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ensure!(victims_in(&home.local) == 20, "a local copy went missing");
    ctx.allow_warning("Mass-deletion guard tripped");
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
