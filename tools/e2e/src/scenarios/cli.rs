//! CLI surfaces: observability commands, conflict tooling, pipeline
//! friendliness, a misconfigured daemon, and live configuration reload.

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use vapor_shared::constants;

use crate::daemon::DaemonKind;
use crate::host::Need;
use crate::logs;
use crate::scenario::{
    CONVERGE_TIMEOUT, Ctx, Expect, OracleMode, Scenario, conflict_copies, conflict_copy_exists,
    read_string, write_file,
};
use crate::scenarios::sync::start_primary;
use crate::wait;
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S13",
            name: "observability",
            proves: "diagnostics answers over IPC and the support bundle exports config, logs, and live captures with a manifest",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: observability,
        },
        Scenario {
            id: "S15",
            name: "conflict-tooling",
            proves: "conflicts list finds a keep-both copy from the files, resolve --keep copy promotes it, the resolution syncs, the list drains",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: conflict_tooling,
        },
        Scenario {
            id: "S51",
            name: "sync-now-under-user-activity",
            proves: "a startup scan held by the throttle says so in status; vapor sync-now runs it under user activity and an offline divergence resolves as a keep-both copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: sync_now_under_user_activity,
        },
        Scenario {
            id: "S18",
            name: "sigpipe",
            proves: "a downstream reader that closes the pipe early does not make the CLI panic",
            needs: &[Need::NativeWatcher, Need::Unix],
            expect: Expect::Pass,
            run: sigpipe,
        },
        Scenario {
            id: "S21",
            name: "misconfigured-daemon-serves-status",
            proves: "a daemon whose only profile cannot be composed stays up and names the reason in status",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: misconfigured_daemon,
        },
        Scenario {
            id: "S22",
            name: "live-config-reload",
            proves: "a resource ceiling set with vapor config reaches the running daemon; a restart-required key is reported in status",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: live_config_reload,
        },
    ]
}

fn observability(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let cli = ctx.cli();
    let diagnostics = cli.json(&["diagnostics", "--json"])?;
    ensure!(
        diagnostics.get("schema_version").is_some(),
        "diagnostics --json lacks schema_version: {diagnostics}"
    );
    let output_dir = ctx.sandbox.root.join("support");
    let bundle = cli.json(&[
        "support-bundle",
        "--output",
        &output_dir.to_string_lossy(),
        "--json",
    ])?;
    ensure!(
        bundle.get("daemonReachable") == Some(&serde_json::Value::Bool(true)),
        "support bundle did not capture the live daemon: {bundle}"
    );
    let bundle_dir = fs::read_dir(&output_dir)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("vapor-support-"))
        })
        .ok_or_else(|| Failure::new("support bundle directory missing"))?;
    for required in ["manifest.json", "status.json"] {
        ensure!(
            bundle_dir.join(required).is_file(),
            "support bundle lacks {required}"
        );
    }
    let _ = home;
    Ok(())
}

fn conflict_tooling(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let first = start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("doc.txt"), "v1\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("doc.txt"), CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    write_file(&home.local.join("doc.txt"), "local divergence\n")?;
    write_file(&home.cloud.join("doc.txt"), "cloud divergence\n")?;
    ctx.start_daemon()?;
    ctx.cli().reconcile()?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "a keep-both conflict copy to appear locally",
        || conflict_copy_exists(&home.local, "doc"),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");

    let cli = ctx.cli();
    let listed = cli.json(&["conflicts", "list", "--json"])?;
    let rendered = listed.to_string();
    ensure!(
        rendered.contains("doc~conflict-"),
        "conflicts list did not find the conflict copy: {rendered}"
    );
    ensure!(
        rendered.contains("\"deviceId\""),
        "conflict record is missing the origin device id"
    );
    // The status endpoint carries the unresolved count so surfaces can
    // flag the copy without walking the tree themselves.
    let listed_count = listed["conflicts"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default() as u64;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.conflicts_unresolved == listed_count,
        "status counts {} unresolved conflicts while the list holds {listed_count}",
        status.conflicts_unresolved
    );

    let copies: Vec<_> = conflict_copies(&home.local, "doc").collect();
    let promoted = copies
        .first()
        .cloned()
        .ok_or_else(|| Failure::new("local conflict copy missing"))?;
    let kept_payload = read_string(&promoted)?;
    let mark = ctx.mark();
    cli.ok(&[
        "conflicts",
        "resolve",
        &promoted.to_string_lossy(),
        "--keep",
        "copy",
    ])?;
    ensure!(
        read_string(&home.local.join("doc.txt"))? == kept_payload,
        "the kept copy's payload did not become the canonical content"
    );
    ensure!(
        !promoted.exists(),
        "resolved conflict copy still exists locally"
    );
    // Divergence on both sides can preserve one copy per side; discard
    // the rest so the scope ends conflict-free.
    for leftover in conflict_copies(&home.local, "doc") {
        cli.ok(&[
            "conflicts",
            "resolve",
            &leftover.to_string_lossy(),
            "--keep",
            "canonical",
        ])?;
    }
    // At least the promotion (a rename) produced intents.
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "resolved conflict copies to disappear from the cloud root",
        || !conflict_copy_exists(&home.cloud, "doc"),
    )?;
    ctx.wait_same_content(
        &home.local.join("doc.txt"),
        &home.cloud.join("doc.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    let after = cli.json(&["conflicts", "list", "--json"])?;
    let empty = after
        .get("conflicts")
        .and_then(|value| value.as_array())
        .is_some_and(|list| list.is_empty());
    ensure!(
        empty,
        "conflicts list is not empty after resolution: {after}"
    );
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.conflicts_unresolved == 0,
        "status still counts {} unresolved conflicts after resolution",
        status.conflicts_unresolved
    );
    Ok(())
}

fn sync_now_under_user_activity(ctx: &mut Ctx) -> Result<(), Failure> {
    // Throttle inputs from a file that says the user is typing, so the
    // daemon sits in Throttled and the startup scan waits (the shape a
    // developer machine produces with the host sampler).
    let inputs_path = ctx.sandbox.root.join("throttle-inputs.json");
    let inputs = vapor_shared::ThrottleInputs {
        user_active: true,
        ..vapor_shared::ThrottleInputs::default()
    };
    let document = serde_json::json!({ "inputs": inputs, "idle_seconds": 0 });
    fs::write(&inputs_path, serde_json::to_string_pretty(&document)?)?;
    let mut home = ctx.primary.clone();
    home.extra_env.insert(
        constants::env::VAPOR_THROTTLE_INPUTS.to_string(),
        format!(
            "{}{}",
            constants::engine::THROTTLE_INPUTS_FILE_PREFIX,
            inputs_path.display()
        ),
    );
    ctx.register_home(home.clone());
    ctx.configure_scope(&home)?;

    // The same name written on both sides while no daemon watched.
    fs::create_dir_all(&home.local)?;
    fs::create_dir_all(&home.cloud)?;
    write_file(&home.local.join("alex.txt"), "local\n")?;
    write_file(&home.cloud.join("alex.txt"), "cloud\n")?;
    let kind = ctx.paths.daemon_kind;
    ctx.start_daemon_in(&home, kind, true)?;
    let cli = ctx.cli_for(&home);

    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to report the startup scan waiting on the throttle",
        || {
            cli.status()
                .is_some_and(|status| status.reconcile_state == "waiting")
        },
    )?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.throttle_state == "Throttled",
        "the file inputs must hold the daemon at Throttled, got {}",
        status.throttle_state
    );
    ensure!(
        status.reconcile_detail.contains("user activity is active"),
        "the waiting scan must name what it waits for: {:?}",
        status.reconcile_detail
    );
    ensure!(
        read_string(&home.local.join("alex.txt"))? == "local\n"
            && read_string(&home.cloud.join("alex.txt"))? == "cloud\n",
        "nothing may move while the scan waits"
    );

    // The user asks for it: the scan runs although the user is active.
    cli.ok(&["sync-now", "--json"])?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the offline divergence to resolve as a keep-both copy on both sides",
        || conflict_copy_exists(&home.local, "alex") && conflict_copy_exists(&home.cloud, "alex"),
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    for root in [&home.local, &home.cloud] {
        let mut payloads: Vec<String> = fs::read_dir(root)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("alex"))
            })
            .map(|path| read_string(&path))
            .collect::<Result<_, _>>()?;
        payloads.sort();
        ensure!(
            payloads == ["cloud\n", "local\n"],
            "both versions must survive under {}: {payloads:?}",
            root.display()
        );
    }
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.reconcile_state != "waiting" && status.conflicts_unresolved >= 1,
        "after the requested scan status must count the copy and stop reporting a wait: state {} unresolved {}",
        status.reconcile_state,
        status.conflicts_unresolved
    );
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");
    Ok(())
}

fn sigpipe(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    // `vapor logs | head -n 1`: the reader closes after one line. The CLI
    // must die quietly like other Unix tools, never print a panic.
    let mut command = Command::new(&ctx.paths.cli_bin);
    command.arg("logs");
    home.apply_env(&mut command);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stdout = child.stdout.as_mut().expect("piped stdout");
        let mut one = [0u8; 1];
        let _ = stdout.read(&mut one);
    }
    // Dropping stdout closes the read end; the writer gets SIGPIPE.
    drop(child.stdout.take());
    let status = child.wait()?;
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    ensure!(
        !stderr.contains("panicked"),
        "vapor logs | head panicked on SIGPIPE: {stderr}"
    );
    ctx.note(format!(
        "vapor logs exited {status} after the reader closed the pipe"
    ));
    Ok(())
}

fn misconfigured_daemon(ctx: &mut Ctx) -> Result<(), Failure> {
    let mut home = ctx.primary.clone();
    home.removed_env
        .push(constants::env::VAPOR_GDRIVE_CLIENT_ID.to_string());
    ctx.register_home(home.clone());
    let cli = ctx.cli_for(&home);
    cli.config_set("localSyncDirectory", &home.local.to_string_lossy())?;
    cli.config_set("cloudSyncDirectory", "/VaporBad")?;
    cli.config_set("provider", "gdrive")?;
    let daemon = ctx.start_daemon_in(&home, DaemonKind::CliRun, false)?;
    wait::wait_until(
        CONVERGE_TIMEOUT,
        "misconfigured daemon to report its suspension reason",
        || {
            cli.status().is_some_and(|status| {
                status.profiles.iter().any(|profile| {
                    profile
                        .suspended_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("VAPOR_GDRIVE_CLIENT_ID"))
                })
            })
        },
    )?;
    // Still alive well past the first idle ticks, and still answering.
    wait::hold_for(
        Duration::from_secs(3),
        "misconfigured daemon stays up and answers status",
        || ctx.daemon(daemon).is_alive() && cli.status().is_some(),
    )?;
    ensure!(
        logs::contains(&home.daemon_log(), "serving status"),
        "daemon log does not say it is serving status only"
    );
    ctx.allow_error("Profile has an invalid provider; suspending it until the config is fixed");
    ctx.allow_error("Every profile is suspended by its configuration; serving status only");
    ctx.set_oracle(OracleMode::Skip("no sync scope was composed".to_string()));
    Ok(())
}

fn live_config_reload(ctx: &mut Ctx) -> Result<(), Failure> {
    start_primary(ctx)?;
    let cli = ctx.cli();
    let said = cli.config_set("resourceLimits", r#"{"cpuPercent": 7}"#)?;
    ensure!(
        said.contains("within a few seconds"),
        "config set did not say the key applies live: {said}"
    );
    wait::wait_until(
        Duration::from_secs(15),
        "the running daemon to adopt cpuPercent 7",
        || {
            cli.status()
                .and_then(|status| status.resource_budget)
                .is_some_and(|budget| budget.effective_cpu_percent == 7)
        },
    )?;
    let said = cli.config_set("syncMode", "push-only")?;
    ensure!(
        said.contains("restart the daemon"),
        "config set did not say syncMode needs a restart: {said}"
    );
    wait::wait_until(
        Duration::from_secs(15),
        "status to report the pending restart",
        || {
            cli.status()
                .and_then(|status| status.config_restart_required)
                .is_some_and(|keys| keys.contains("syncMode"))
        },
    )?;
    ctx.allow_warning("Configuration keys changed that take effect on the next daemon start");
    Ok(())
}
