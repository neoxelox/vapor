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
            proves: "diagnostics answers over IPC and vapor report exports config, logs, and live captures with a manifest",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: observability,
        },
        Scenario {
            id: "S15",
            name: "conflict-decisions",
            proves: "every version of a file edited in three places is one conflict decision naming each device, decisions resolve --choose <file> keeps that version at once and trashes the others, the resolution syncs, and the question closes",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: conflict_tooling,
        },
        Scenario {
            id: "S51",
            name: "reconcile-under-user-activity",
            proves: "a startup scan held by the throttle says so in status; vapor reconcile runs it under user activity and an offline divergence resolves as a keep-both copy",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: reconcile_under_user_activity,
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
            id: "S55",
            name: "sign-in-required-holds-the-queue",
            proves: "a gdrive profile with no stored sign-in holds its queue instead of failing it, and status names the profile and the command that ends the hold",
            needs: &[Need::NativeWatcher, Need::Filesystem, Need::SecretStore],
            expect: Expect::Pass,
            run: sign_in_required_holds_the_queue,
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
        diagnostics.get("schemaVersion").is_some(),
        "diagnostics --json lacks schemaVersion: {diagnostics}"
    );
    let output_dir = ctx.sandbox.root.join("reports");
    let report = cli.json(&[
        "report",
        "--output",
        &output_dir.to_string_lossy(),
        "--json",
    ])?;
    ensure!(
        report.get("daemonReachable") == Some(&serde_json::Value::Bool(true)),
        "the support report did not capture the live daemon: {report}"
    );
    let report_dir = fs::read_dir(&output_dir)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("vapor-report-"))
        })
        .ok_or_else(|| Failure::new("support report directory missing"))?;
    for required in ["manifest.json", "status.json"] {
        ensure!(
            report_dir.join(required).is_file(),
            "the support report lacks {required}"
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
    // Three edits of one file while no daemon watched: this device's,
    // the one that reached the cloud first, and a third device's that
    // already lost its race and sits in the cloud as its copy.
    write_file(&home.local.join("doc.txt"), "local divergence\n")?;
    write_file(&home.cloud.join("doc.txt"), "cloud divergence\n")?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_millis())
        .unwrap_or_default();
    let third = format!("doc~conflict-third-device-{now_ms}.txt");
    write_file(&home.cloud.join(&third), "third device\n")?;
    ctx.start_daemon()?;
    ctx.cli().reconcile()?;
    ctx.wait_until(CONVERGE_TIMEOUT, "both conflict copies to be here", || {
        conflict_copies(&home.local, "doc").count() == 2
    })?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");

    let cli = ctx.cli();
    // The three versions are one question, within the daemon's
    // matching cadence.
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "one conflict decision listing the three versions",
        || {
            open_conflicts(&cli).is_some_and(|open| {
                open.len() == 1 && open[0]["options"].as_array().is_some_and(|o| o.len() == 3)
            })
        },
    )?;
    let decision = open_conflicts(&cli)
        .unwrap_or_default()
        .into_iter()
        .next()
        .ok_or_else(|| Failure::new("no conflict decision to answer"))?;
    ensure!(
        decision["path"].as_str() == home.local.join("doc.txt").to_str(),
        "the decision is not about doc.txt: {decision}"
    );
    let third_option = decision["options"]
        .as_array()
        .and_then(|options| {
            options
                .iter()
                .find(|option| option["key"] == third.as_str())
        })
        .cloned()
        .ok_or_else(|| {
            Failure::new(format!(
                "the third device's version is not offered: {decision}"
            ))
        })?;
    ensure!(
        third_option["label"] == "Keep the edit made on third-device",
        "the option does not name the device: {third_option}"
    );
    ensure!(
        decision["evidence"]["versions"]
            .as_array()
            .is_some_and(
                |versions| versions.iter().filter(|v| v["device"].is_string()).count() == 2
            ),
        "the evidence does not name a device per copy: {decision}"
    );
    // The status endpoint counts it with the other decisions, so
    // surfaces can flag it without listing.
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to count the conflict decision",
        || cli.status().is_some_and(|status| status.decisions == 1),
    )?;

    let mark = ctx.mark();
    let answered = cli.json(&[
        "decisions",
        "resolve",
        &decision["id"].to_string(),
        "--choose",
        &third,
        "--json",
    ])?;
    ensure!(
        answered["appliedAtMs"].is_number(),
        "a conflict answer must apply at once: {answered}"
    );
    ensure!(
        read_string(&home.local.join("doc.txt"))? == "third device\n",
        "the chosen version did not take the name"
    );
    ensure!(
        !conflict_copy_exists(&home.local, "doc"),
        "a conflict copy is still here after the answer"
    );
    let trash = cli.json(&["trash", "list", "--json"])?;
    let trashed = trash["entries"]
        .as_array()
        .map(|entries| entries.iter().filter(|e| e["reason"] == "conflict").count())
        .unwrap_or_default();
    ensure!(
        trashed == 2,
        "the two versions not kept must be in the trash: {trash}"
    );
    // At least the rename and the removals produced intents.
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the conflict copies to disappear from the cloud root",
        || !conflict_copy_exists(&home.cloud, "doc"),
    )?;
    ctx.wait_same_content(
        &home.local.join("doc.txt"),
        &home.cloud.join("doc.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "no conflict question to remain open",
        || open_conflicts(&cli).is_some_and(|open| open.is_empty()),
    )?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to count no open decision after resolution",
        || cli.status().is_some_and(|status| status.decisions == 0),
    )?;
    Ok(())
}

/// The open `conflict` decisions `vapor decisions list --json` reports.
fn open_conflicts(cli: &crate::cli::Cli) -> Option<Vec<serde_json::Value>> {
    let listed = cli.json(&["decisions", "list", "--json"]).ok()?;
    Some(
        listed["decisions"]
            .as_array()?
            .iter()
            .filter(|decision| decision["kind"] == "conflict" && decision["choice"].is_null())
            .cloned()
            .collect(),
    )
}

fn reconcile_under_user_activity(ctx: &mut Ctx) -> Result<(), Failure> {
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
                .is_some_and(|status| status.reconcile.state == "waiting")
        },
    )?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.throttle.state == "throttled",
        "the file inputs must hold the daemon at Throttled, got {}",
        status.throttle.state
    );
    ensure!(
        status.reconcile.detail.contains("user activity is active"),
        "the waiting scan must name what it waits for: {:?}",
        status.reconcile.detail
    );
    ensure!(
        read_string(&home.local.join("alex.txt"))? == "local\n"
            && read_string(&home.cloud.join("alex.txt"))? == "cloud\n",
        "nothing may move while the scan waits"
    );

    // The user asks for it: the scan runs although the user is active.
    cli.ok(&["reconcile", "--json"])?;
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
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to count the copy as a decision and stop reporting a wait",
        || {
            cli.status()
                .is_some_and(|status| status.reconcile.state != "waiting" && status.decisions >= 1)
        },
    )?;
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
    cli.config_set("localRoot", &home.local.to_string_lossy())?;
    cli.config_set("cloudRoot", "/VaporBad")?;
    cli.config_set("provider", "gdrive")?;
    let daemon = ctx.start_daemon_in(&home, DaemonKind::CliRun, false)?;
    wait::wait_until(
        CONVERGE_TIMEOUT,
        "misconfigured daemon to report its suspension reason",
        || {
            cli.status().is_some_and(|status| {
                status.profiles.iter().any(|profile| {
                    profile.suspended && profile.run.reason.contains("VAPOR_GDRIVE_CLIENT_ID")
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

/// A profile id no `vapor auth login` on a developer machine uses, so
/// the daemon's keychain lookup finds nothing and no request is made.
const SIGNED_OUT_PROFILE: &str = "e2e-signed-out";

fn sign_in_required_holds_the_queue(ctx: &mut Ctx) -> Result<(), Failure> {
    let mut home = ctx.primary.clone();
    // A client id is enough to build the provider; with no token stored
    // for the profile every call fails before it reaches the network.
    home.extra_env.insert(
        constants::env::VAPOR_GDRIVE_CLIENT_ID.to_string(),
        "e2e-placeholder.apps.googleusercontent.com".to_string(),
    );
    home.removed_env
        .push(constants::env::VAPOR_GDRIVE_CLIENT_SECRET.to_string());
    ctx.register_home(home.clone());
    let cli = ctx.cli_for(&home);
    let profiles = serde_json::json!([{
        constants::profile::KEY_ID: SIGNED_OUT_PROFILE,
        constants::profile::KEY_NAME: "Signed out",
        constants::profile::KEY_ENABLED: true,
        constants::profile::KEY_PROVIDER: constants::provider::GDRIVE,
        constants::profile::KEY_LOCAL_ROOT: home.local.to_string_lossy(),
        constants::profile::KEY_CLOUD_ROOT: "/VaporE2E",
    }]);
    cli.config_set("profiles", &profiles.to_string())?;
    ctx.start_daemon_in(&home, DaemonKind::CliRun, false)?;

    let command = format!("vapor auth login --profile {SIGNED_OUT_PROFILE}");
    wait::wait_until(
        CONVERGE_TIMEOUT,
        "the profile to report that it needs a sign-in",
        || {
            cli.status().is_some_and(|status| {
                status.profiles.iter().any(|profile| {
                    profile.id == SIGNED_OUT_PROFILE
                        && profile.sign_in_required
                        && profile.run.state == constants::run_state::ERROR
                        && profile.run.reason.contains(&command)
                })
            })
        },
    )?;

    // A change made while signed out waits in the queue.
    write_file(&home.local.join("draft.txt"), "written while signed out\n")?;
    let db = crate::db::StateDb::at(&home.profile_state_db(SIGNED_OUT_PROFILE));
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the change to reach the durable queue",
        || db.pending_intents().is_some_and(|pending| pending >= 1),
    )?;
    wait::hold_for(
        Duration::from_secs(3),
        "the change stays queued and nothing fails",
        || {
            db.failed_intents() == Some(0)
                && db.pending_intents().is_some_and(|pending| pending >= 1)
        },
    )?;

    let text = cli.ok(&["status"])?;
    ensure!(
        text.contains(&format!(
            "Sign-in required: profile {SIGNED_OUT_PROFILE} is on hold ({command})"
        )),
        "vapor status does not name the sign-in: {text}"
    );

    ctx.allow_warning(
        "The provider refused the sign-in; sync is on hold until the user signs in again",
    );
    ctx.set_oracle(OracleMode::Skip(
        "the profile syncs to Google Drive and is held before any transfer".to_string(),
    ));
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
                .and_then(|status| status.resources)
                .is_some_and(|resources| resources.cpu.limit_percent == 7)
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
                .and_then(|status| status.restart_required)
                .is_some_and(|keys| keys.contains("syncMode"))
        },
    )?;
    ctx.allow_warning("Configuration keys changed that take effect on the next daemon start");
    Ok(())
}
