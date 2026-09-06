//! Sync modes, deletion semantics, profiles, safeguards, and name
//! collisions. Several scenarios here pin behavior the product has
//! today so a change of policy is a deliberate edit, not an accident.

use std::fs;
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
            proves: "a file deleted locally while the daemon was down is restored from the cloud on restart (ambiguous evidence keeps data)",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: offline_local_delete,
        },
        Scenario {
            id: "S28",
            name: "offline-cloud-delete",
            proves: "a file deleted in the cloud while the daemon was down is re-uploaded from the local copy on restart (ambiguous evidence keeps data)",
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
            // Same root cause as the index-rebuild gap: an equal-size
            // pair with no index row is taken as converged, so a strict
            // mirror silently leaves the divergence in place.
            expect: Expect::KnownGap(
                "a strict mirror takes an index-less equal-size pair as converged and never overwrites it",
            ),
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
            name: "mass-delete-guard",
            proves: "a burst of local deletions above the threshold pauses sync with a reason naming vapor resume; resume re-arms and the deletions then propagate",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: mass_delete_guard,
        },
        Scenario {
            id: "S33",
            name: "case-collision",
            proves: "two cloud files whose names differ only by case both survive on a case-insensitive local filesystem, one as a conflict copy",
            needs: &[
                Need::NativeWatcher,
                Need::Filesystem,
                Need::CaseInsensitiveFs,
                Need::DiskImage,
            ],
            expect: Expect::KnownGap(
                "the engine has no rule for names that collide on a case-insensitive filesystem",
            ),
            run: case_collision,
        },
    ]
}

fn offline_local_delete(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("keep.txt"), "kept\n")?;
    write_file(&home.local.join("gone.txt"), "deleted offline\n")?;
    ctx.converge_from(&mark, 2, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("gone.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    fs::remove_file(home.local.join("gone.txt"))?;
    ctx.start_daemon()?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        home.local.join("gone.txt").is_file() && home.cloud.join("gone.txt").is_file(),
        "expected the offline-deleted file to be restored on both sides; local: {}, cloud: {}",
        home.local.join("gone.txt").exists(),
        home.cloud.join("gone.txt").exists()
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
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("gone.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    fs::remove_file(home.cloud.join("gone.txt"))?;
    ctx.start_daemon()?;
    ctx.settle(Duration::from_secs(60))?;
    ensure!(
        home.local.join("gone.txt").is_file() && home.cloud.join("gone.txt").is_file(),
        "expected the cloud-deleted file to be re-uploaded; local: {}, cloud: {}",
        home.local.join("gone.txt").exists(),
        home.cloud.join("gone.txt").exists()
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

fn mass_delete_guard(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    // The floors are 10 deletions in 5 seconds; use the floor so the
    // burst stays small.
    ctx.cli().config_set(
        "safeguards",
        r#"{"massDeleteThreshold": 10, "massDeleteWindowSeconds": 5}"#,
    )?;
    ctx.start_daemon()?;
    let mark = ctx.mark();
    for index in 0..14 {
        write_file(
            &home.local.join(format!("victim-{index:02}.txt")),
            format!("victim {index}\n"),
        )?;
    }
    ctx.converge_from(&mark, 14, Duration::from_secs(60))?;
    ctx.wait_exists(&home.cloud.join("victim-13.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    for index in 0..14 {
        fs::remove_file(home.local.join(format!("victim-{index:02}.txt")))?;
    }
    let cli = ctx.cli();
    wait::wait_until(
        Duration::from_secs(30),
        "the mass-deletion guard to pause the daemon",
        || cli.run_state_is("Paused"),
    )?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    let reason = status
        .profiles
        .first()
        .map(|profile| profile.reason.clone())
        .unwrap_or_default();
    ensure!(
        reason.contains("vapor resume"),
        "pause reason does not name the way out: {reason:?}"
    );
    let survivors = fs::read_dir(&home.cloud)?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("victim-"))
        .count();
    ensure!(
        survivors > 0,
        "the guard paused but every cloud copy is already gone"
    );
    let timeline = cli.json(&["timeline", "--json"])?;
    ensure!(
        timeline.to_string().contains("\"guard\""),
        "the guard trip did not land on the timeline"
    );
    cli.resume()?;
    cli.wait_run_state("Running", Duration::from_secs(10))?;
    ctx.wait_until(
        Duration::from_secs(60),
        "the held deletions to propagate after resume",
        || {
            fs::read_dir(&home.cloud)
                .map(|entries| {
                    !entries
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy().starts_with("victim-"))
                })
                .unwrap_or(false)
        },
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.allow_warning("mass");
    ctx.allow_warning("Mass");
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
    ensure!(
        read_string(&home.cloud.join("Readme.md"))? == "upper-case readme\n"
            && read_string(&home.cloud.join("readme.md"))? == "lower-case readme\n",
        "the cloud objects were rewritten by the collision"
    );
    ensure!(
        conflict_copy_exists(&home.local, "Readme") || conflict_copy_exists(&home.local, "readme"),
        "the colliding payload did not become a conflict copy"
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
