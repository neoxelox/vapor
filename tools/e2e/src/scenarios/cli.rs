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
            id: "S65",
            name: "report-hides-names",
            proves: "vapor report replaces every folder path and file name by a code in the config, the logs, and the live state, and says so; --keep-paths keeps them and says that the report is not for a public issue",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: report_hides_names,
        },
        Scenario {
            id: "S66",
            name: "ignored-paths-are-shown",
            proves: "vapor status counts the paths that the ignore rules keep out of sync, and vapor ignored lists each one with its rule, a folder one time and never a file that only shares a folder rule's name",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: ignored_paths_are_shown,
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
            id: "S79",
            name: "replaced-version-is-a-question",
            proves: "a version that an upload replaced without having seen it, which the provider leaves next to the file as a copy named ~conflict-replaced, comes to this device and is one conflict decision whose option says what the copy is; the answer keeps it under the name on both sides",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: replaced_version_is_a_question,
        },
        Scenario {
            id: "S51",
            name: "sync-under-lasting-conditions",
            proves: "with a user at the keyboard, on battery, and Low Power Mode on, the startup scan runs with no request and an offline divergence resolves as a keep-both copy; under critical thermal pressure a new file waits and status says why; it uploads when the pressure clears",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: sync_under_lasting_conditions,
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

/// Every text file of a report, joined.
fn report_text(report_dir: &std::path::Path) -> Result<String, Failure> {
    let mut text = String::new();
    let mut pending = vec![report_dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                text.push_str(&String::from_utf8_lossy(&fs::read(&path)?));
                text.push('\n');
            }
        }
    }
    Ok(text)
}

fn report_hides_names(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let cli = ctx.cli();
    // Names that appear nowhere else, one of them with spaces. No word
    // of them is one the daemon logs on its own: a long sandbox path
    // makes it log that the socket path is over its "budget".
    let mark = ctx.mark();
    write_file(
        &home.local.join("Quarterly Plans/zebra ledger draft.txt"),
        "figures\n",
    )?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(
        &home.cloud.join("Quarterly Plans/zebra ledger draft.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
    // The reports are judged by a name that the daemon's log holds. The
    // log lines that name the file come a moment after the transfer
    // (the cloud feed's own event, the second look at the file), so the
    // scenario waits for one: a report written before it would hold no
    // name to hide, and none to keep.
    let daemon_log = home.daemon_log();
    ctx.wait_until(CONVERGE_TIMEOUT, "the daemon log to name the file", || {
        read_string(&daemon_log).is_ok_and(|log| log.contains("zebra ledger draft.txt"))
    })?;
    let sandbox_name = ctx
        .sandbox
        .root
        .parent()
        .and_then(|run| run.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let write_report =
        |extra: &[&str], into: &str| -> Result<(serde_json::Value, String, String), Failure> {
            let output_dir = ctx.sandbox.root.join(into);
            let output = output_dir.to_string_lossy().into_owned();
            let mut args = vec!["report", "--output", output.as_str()];
            args.extend_from_slice(extra);
            let human = cli.ok(&args)?;
            let report_dir = fs::read_dir(&output_dir)?
                .flatten()
                .map(|entry| entry.path())
                .next()
                .ok_or_else(|| Failure::new("support report directory missing"))?;
            let manifest: serde_json::Value =
                serde_json::from_str(&read_string(&report_dir.join("manifest.json"))?)
                    .map_err(|error| Failure::new(format!("manifest is not JSON: {error}")))?;
            Ok((manifest, report_text(&report_dir)?, human))
        };

    let (manifest, text, human) = write_report(&[], "reports-redacted")?;
    ensure!(
        manifest["pathsRedacted"] == true,
        "the manifest must say that paths were redacted: {manifest}"
    );
    for name in [
        "zebra",
        "ledger",
        "Quarterly",
        "Plans",
        sandbox_name.as_str(),
    ] {
        ensure!(
            !text.contains(name),
            "the report still holds the name {name:?}"
        );
    }
    ensure!(
        text.contains("<local-root:default>") && text.contains("p-"),
        "the report must show where a path stood"
    );
    ensure!(
        human.contains("read the report before you share it"),
        "the command must say on the terminal what the report can still hold: {human}"
    );

    let (manifest, text, human) = write_report(&["--keep-paths"], "reports-kept")?;
    ensure!(
        manifest["pathsRedacted"] == false && text.contains("zebra ledger draft.txt"),
        "--keep-paths must keep the names"
    );
    ensure!(
        human.contains("public issue"),
        "the command must warn about a report with names in it: {human}"
    );
    Ok(())
}

fn ignored_paths_are_shown(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    // `build/` and `*.log` are default rules.
    write_file(&home.local.join("build/out/app.o"), "object\n")?;
    write_file(&home.local.join("build/out/lib.o"), "object\n")?;
    write_file(&home.local.join("notes/today.log"), "log\n")?;
    write_file(&home.local.join("notes/build"), "a file named build\n")?;
    write_file(&home.local.join("notes/plan.txt"), "plan\n")?;
    ctx.start_daemon()?;
    let cli = ctx.cli();
    ctx.wait_exists(&home.cloud.join("notes/plan.txt"), CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("notes/build"), CONVERGE_TIMEOUT)?;

    // The startup scan counted what the rules hid.
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to count the ignored paths",
        || {
            cli.json(&["status", "--json"])
                .is_ok_and(|status| status["profiles"][0]["ignoredPaths"] == 2)
        },
    )?;
    let text = cli.ok(&["status"])?;
    ensure!(
        text.contains("Ignored: 2 path(s)") && text.contains("vapor ignored"),
        "status must say how many paths are ignored and how to list them: {text}"
    );

    let listed = cli.json(&["ignored", "--json"])?;
    let mut entries: Vec<(String, String, String)> = listed["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap_or_default().to_string(),
                entry["kind"].as_str().unwrap_or_default().to_string(),
                entry["rule"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    entries.sort();
    let expected = vec![
        (
            home.local.join("build").to_string_lossy().into_owned(),
            "folder".to_string(),
            "build/".to_string(),
        ),
        (
            home.local
                .join("notes/today.log")
                .to_string_lossy()
                .into_owned(),
            "file".to_string(),
            "*.log".to_string(),
        ),
    ];
    ensure!(
        entries == expected,
        "vapor ignored must list the folder one time and the log file, each with its rule: {listed}"
    );
    ensure!(
        !home.cloud.join("build").exists() && !home.cloud.join("notes/today.log").exists(),
        "what the list names is what did not sync"
    );
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

fn replaced_version_is_a_question(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    start_primary(ctx)?;
    let mark = ctx.mark();
    write_file(&home.local.join("doc.txt"), "the edit that kept the name\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("doc.txt"), CONVERGE_TIMEOUT)?;
    ctx.settle(CONVERGE_TIMEOUT)?;

    // Two uploads of one file landed in the same moment, and the
    // second replaced the first without having compared it. The
    // provider keeps the first next to the file under this name.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_millis())
        .unwrap_or_default();
    let copy = format!("doc~conflict-replaced-{now_ms}.txt");
    write_file(&home.cloud.join(&copy), "the edit that was replaced\n")?;
    ctx.wait_exists(&home.local.join(&copy), CONVERGE_TIMEOUT)?;

    let cli = ctx.cli();
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "one conflict decision with the two versions",
        || {
            open_conflicts(&cli).is_some_and(|open| {
                open.len() == 1 && open[0]["options"].as_array().is_some_and(|o| o.len() == 2)
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
    let option = decision["options"]
        .as_array()
        .and_then(|options| options.iter().find(|option| option["key"] == copy.as_str()))
        .cloned()
        .ok_or_else(|| Failure::new(format!("the replaced version is not offered: {decision}")))?;
    ensure!(
        option["label"] == "Keep the version that another upload replaced",
        "the option does not say what the copy is: {option}"
    );

    let mark = ctx.mark();
    cli.json(&[
        "decisions",
        "resolve",
        &decision["id"].to_string(),
        "--choose",
        &copy,
        "--json",
    ])?;
    ensure!(
        read_string(&home.local.join("doc.txt"))? == "the edit that was replaced\n",
        "the chosen version did not take the name"
    );
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the copy to leave the cloud folder",
        || !conflict_copy_exists(&home.cloud, "doc"),
    )?;
    ctx.wait_same_content(
        &home.local.join("doc.txt"),
        &home.cloud.join("doc.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle(CONVERGE_TIMEOUT)?;
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

/// Writes the throttle inputs the daemon reads on its next sample.
fn write_throttle_inputs(
    path: &std::path::Path,
    inputs: vapor_shared::ThrottleInputs,
) -> Result<(), Failure> {
    let document = serde_json::json!({ "inputs": inputs, "idle_seconds": 0 });
    fs::write(path, serde_json::to_string_pretty(&document)?)?;
    Ok(())
}

fn sync_under_lasting_conditions(ctx: &mut Ctx) -> Result<(), Failure> {
    // Throttle inputs from a file: a user who types, on battery, with
    // Low Power Mode on. Each of the three can hold for hours, so none
    // of them may stop sync.
    let inputs_path = ctx.sandbox.root.join("throttle-inputs.json");
    let lasting = vapor_shared::ThrottleInputs {
        user_active: true,
        on_battery: true,
        low_power_mode: true,
        ..vapor_shared::ThrottleInputs::default()
    };
    write_throttle_inputs(&inputs_path, lasting)?;
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

    // Nobody asks for a reconcile: the startup scan runs at the
    // Throttled caps.
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "the offline divergence to resolve as a keep-both copy on both sides",
        || conflict_copy_exists(&home.local, "alex") && conflict_copy_exists(&home.cloud, "alex"),
    )?;
    ctx.settle_home(&home, crate::scenario::SETTLE_QUIET, CONVERGE_TIMEOUT)?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status.throttle.state == "throttled",
        "the file inputs must hold the daemon at Throttled, got {}",
        status.throttle.state
    );
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

    // A device about to overheat is the one condition that stops sync.
    write_throttle_inputs(
        &inputs_path,
        vapor_shared::ThrottleInputs {
            thermal_pressure: vapor_shared::ThermalPressure::Critical,
            ..lasting
        },
    )?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to report the suspended state",
        || {
            cli.status()
                .is_some_and(|status| status.throttle.state == "suspended")
        },
    )?;
    write_file(&home.local.join("while-hot.txt"), "waits\n")?;
    std::thread::sleep(Duration::from_secs(8));
    ensure!(
        !home.cloud.join("while-hot.txt").exists(),
        "nothing uploads while sync is suspended"
    );
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status did not answer"))?;
    ensure!(
        status
            .throttle
            .reason
            .contains("thermal pressure is critical"),
        "status must say why sync is stopped: {:?}",
        status.throttle.reason
    );

    // It cools down: the file goes up with no request.
    write_throttle_inputs(&inputs_path, lasting)?;
    ctx.wait_same_content(
        &home.local.join("while-hot.txt"),
        &home.cloud.join("while-hot.txt"),
        CONVERGE_TIMEOUT,
    )?;
    ctx.settle_home(&home, crate::scenario::SETTLE_QUIET, CONVERGE_TIMEOUT)?;
    ctx.allow_warning("Resolved concurrent divergence by keeping both versions");
    ctx.allow_warning("Updated throttle state");
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
    ctx.allow_warning("Every profile is suspended by its configuration; serving status only");
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
