//! Startup, configuration, process discipline, and observability
//! basics. Every later scenario relies on what these prove.

use std::fs;
use std::time::Duration;

use crate::daemon::DaemonKind;
use crate::host::Need;
use crate::logs;
use crate::scenario::{CONVERGE_TIMEOUT, Ctx, Expect, Scenario, write_file};
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "S01",
            name: "config-round-trip",
            proves: "config set/get round-trips through vapor.json; structured keys are stored as JSON",
            needs: &[],
            expect: Expect::Pass,
            run: config_round_trip,
        },
        Scenario {
            id: "S02",
            name: "daemon-startup",
            proves: "daemon reaches Running, creates the missing local sync root, binds the IPC socket",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: daemon_startup,
        },
        Scenario {
            id: "S03",
            name: "local-ingest-converges",
            proves: "local writes become durable intents and drain; the cloud root matches byte for byte",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: local_ingest_converges,
        },
        Scenario {
            id: "S04",
            name: "pause-resume",
            proves: "pause flips run_state to Paused over IPC, resume restores Running, the paused backlog drains",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: pause_resume,
        },
        Scenario {
            id: "S05",
            name: "singleton-lock",
            proves: "a second daemon on the same VAPOR_DIR exits non-zero saying one is already running",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: singleton_lock,
        },
        Scenario {
            id: "S06",
            name: "doctor",
            proves: "vapor doctor reports no failures inside the sandbox, in text and --json",
            needs: &[Need::NativeWatcher],
            expect: Expect::Pass,
            run: doctor,
        },
        Scenario {
            id: "S07",
            name: "restart-recovery",
            proves: "clean SIGTERM shutdown, restart on the same state DB, post-restart writes converge",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: restart_recovery,
        },
        Scenario {
            id: "S08",
            name: "log-hygiene",
            proves: "a healthy run across two restarts emits no ERROR line and no warning",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: log_hygiene,
        },
        Scenario {
            id: "S61",
            name: "fresh-install-is-unconfigured",
            proves: "a home with no sync scope starts a daemon that syncs nothing and creates no folder, and status says unconfigured and names the command; setting the two roots, one config set each, starts the profile with no restart",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: fresh_install_is_unconfigured,
        },
        Scenario {
            id: "S63",
            name: "profiles-command-sets-an-install-up",
            proves: "vapor profiles add sets a fresh install up with one command and no restart, and refuses when no terminal can ask and a folder is left out; it refuses a cloud folder inside the local folder, a local folder inside the one of another profile, and a pull-only profile on the folder of another profile; remove refuses while the daemon runs, then forgets the sync state and leaves the files on both sides",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: profiles_command_sets_an_install_up,
        },
        Scenario {
            id: "S70",
            name: "init-sets-an-install-up",
            proves: "vapor init takes a fresh install to a running sync with one command and no terminal, refuses when a folder is left out, and a second run changes nothing and says initialized",
            needs: &[Need::NativeWatcher, Need::Filesystem],
            expect: Expect::Pass,
            run: init_sets_an_install_up,
        },
        Scenario {
            id: "S09",
            name: "socket-relocation",
            proves: "an over-budget VAPOR_DIR relocates the IPC socket under the OS temp dir; status and doctor still work",
            needs: &[Need::NativeWatcher, Need::Unix],
            expect: Expect::Pass,
            run: socket_relocation,
        },
    ]
}

fn config_round_trip(ctx: &mut Ctx) -> Result<(), Failure> {
    ctx.skip_oracle("no daemon runs; the scenario only exercises vapor config");
    let cli = ctx.cli();
    let home = ctx.primary.clone();
    // Before anything is set, neither root has a value: Vapor syncs
    // only folders the user chose, whatever the provider.
    for provider in ["filesystem", "gdrive", "filesystem"] {
        cli.config_set("provider", provider)?;
        for root in ["localRoot", "cloudRoot"] {
            let value = cli.config_get(root)?;
            ensure!(
                value.is_empty(),
                "an unset {root} must have no default under {provider}, got {value:?}"
            );
        }
    }
    ctx.configure_scope(&home)?;
    let local = cli.config_get("localRoot")?;
    ensure!(
        local == home.local.to_string_lossy(),
        "config get did not round-trip localRoot: got {local:?}"
    );
    ensure!(
        home.config_path().is_file(),
        "vapor.json was not written under VAPOR_DIR"
    );
    // Structured keys are stored as JSON (a string containing JSON would
    // make the daemon ignore the group) and unset keys read as defaults.
    cli.config_set("safeguards", r#"{"massDelete": {"threshold": 500}}"#)?;
    let raw = fs::read_to_string(home.config_path())?;
    ensure!(
        raw.contains("\"threshold\": 500"),
        "safeguards was not stored as a JSON object: {raw}"
    );
    ensure!(
        cli.config_get("syncMode")? == "two-way",
        "config get did not render the default for an unset key"
    );
    let rejected = cli.run(&["config", "set", "resourceLimits", "cpu=5"])?;
    ensure!(
        !rejected.success(),
        "config set accepted a non-JSON value for resourceLimits"
    );
    Ok(())
}

fn fresh_install_is_unconfigured(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let cli = ctx.cli();
    // No scope is written: the home holds what a fresh install holds.
    let kind = ctx.paths.daemon_kind;
    ctx.start_daemon_in(&home, kind, false)?;
    cli.wait_run_state(
        vapor_shared::constants::run_state::UNCONFIGURED,
        crate::scenario::STARTUP_TIMEOUT,
    )?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status --json did not answer"))?;
    ensure!(
        status.run.reason.contains("vapor init"),
        "status must name the command that sets a profile up: {:?}",
        status.run.reason
    );
    ensure!(
        status.profiles.is_empty(),
        "an unconfigured daemon has no profile"
    );
    ensure!(
        !home.local.exists() && !home.cloud.exists(),
        "an unconfigured daemon creates no sync folder"
    );
    let state = home.dir.join("state");
    let databases: Vec<_> = walk_files(&state)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "sqlite"))
        .collect();
    ensure!(
        databases.is_empty(),
        "no profile, so no sync state: {databases:?}"
    );

    // One root: the profile says what it still needs; still no folder.
    cli.config_set("localRoot", &home.local.to_string_lossy())?;
    ctx.wait_until(
        CONVERGE_TIMEOUT,
        "status to name the root that is not set",
        || {
            cli.status().is_some_and(|status| {
                status
                    .profiles
                    .first()
                    .is_some_and(|profile| profile.run.reason.contains("cloudRoot"))
            })
        },
    )?;
    ensure!(!home.local.exists(), "half a scope creates no folder");
    // The daemon says once that the profile waits for its second root.
    ctx.allow_warning("Suspending a profile that cannot run as configured");
    ctx.allow_warning("Every profile is suspended by its configuration");

    // The second root: the profile runs, with no restart asked for.
    cli.config_set("cloudRoot", &home.cloud.to_string_lossy())?;
    cli.wait_run_state("running", CONVERGE_TIMEOUT)?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status --json did not answer"))?;
    ensure!(
        status.restart_required.is_none(),
        "the first scope must not ask for a restart: {:?}",
        status.restart_required
    );
    ensure!(home.local.is_dir(), "the profile adopted its local root");
    let mark = ctx.mark();
    write_file(&home.local.join("first.txt"), "the first file\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.local.join("first.txt"),
        &home.cloud.join("first.txt"),
        CONVERGE_TIMEOUT,
    )?;
    Ok(())
}

fn init_sets_an_install_up(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let cli = ctx.cli();
    let kind = ctx.paths.daemon_kind;
    ctx.start_daemon_in(&home, kind, false)?;
    cli.wait_run_state(
        vapor_shared::constants::run_state::UNCONFIGURED,
        crate::scenario::STARTUP_TIMEOUT,
    )?;
    // `--auto-launch off` in every call: a sandbox never registers a
    // service with the host, and the daemon here is the harness's own.

    // No terminal and no folders: nobody chose them, so nothing is
    // written.
    let no_folders = cli.run(&["init", "--auto-launch", "off", "--json"])?;
    ensure!(
        !no_folders.success() && no_folders.stderr.contains("--local <path> --cloud <path>"),
        "init with no folders and no terminal must refuse: {}",
        no_folders.stderr
    );
    ensure!(
        cli.json(&["profiles", "list", "--json"])?["profiles"] == serde_json::json!([]),
        "a refused init leaves no profile"
    );

    // One command, every answer an option.
    let local = home.local.to_string_lossy().into_owned();
    let cloud = home.cloud.to_string_lossy().into_owned();
    let first = cli.json(&[
        "init",
        "--provider",
        "filesystem",
        "--local",
        &local,
        "--cloud",
        &cloud,
        "--auto-launch",
        "off",
        "--json",
    ])?;
    let states = |report: &serde_json::Value| -> Vec<(String, String)> {
        report["steps"]
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .map(|step| {
                        (
                            step["step"].as_str().unwrap_or_default().to_string(),
                            step["state"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let settled = [
        ("profile".to_string(), "done".to_string()),
        ("sign-in".to_string(), "not-needed".to_string()),
        ("service".to_string(), "off".to_string()),
    ];
    ensure!(
        first["initialized"] == true && states(&first) == settled,
        "one init run sets the install up: {first}"
    );
    ensure!(
        first["profiles"][0]["id"] == "default",
        "the first profile is the default one: {first}"
    );
    // The daemon was up before the profile existed. The run waits for
    // it to read the configuration again, so its last line is not "no
    // sync profile yet" about the profile it has just made.
    ensure!(
        first["daemon"]["state"].is_string()
            && first["daemon"]["state"] != vapor_shared::constants::run_state::UNCONFIGURED,
        "init ends with a daemon that does not know the profile: {first}"
    );

    // The running daemon picks the profile up and syncs.
    cli.wait_run_state("running", CONVERGE_TIMEOUT)?;
    let mark = ctx.mark();
    write_file(&home.local.join("first.txt"), "the first file\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.local.join("first.txt"),
        &home.cloud.join("first.txt"),
        CONVERGE_TIMEOUT,
    )?;

    // A second run has nothing left to do: it changes nothing, and it
    // says so with the state of the daemon.
    let before = fs::read_to_string(home.config_path())?;
    let second = cli.json(&["init", "--json"])?;
    ensure!(
        second["initialized"] == true && states(&second) == settled,
        "a second init run finds every step settled: {second}"
    );
    ensure!(
        second["daemon"]["state"] == "running",
        "an initialized install reports the daemon: {second}"
    );
    ensure!(
        fs::read_to_string(home.config_path())? == before,
        "a second init run must not change vapor.json"
    );
    ensure!(
        cli.json(&["profiles", "list", "--json"])?["profiles"]
            .as_array()
            .is_some_and(|profiles| profiles.len() == 1),
        "a second init run adds no second profile"
    );
    Ok(())
}

fn profiles_command_sets_an_install_up(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    let cli = ctx.cli();
    let unconfigured = vapor_shared::constants::run_state::UNCONFIGURED;
    let kind = ctx.paths.daemon_kind;
    let daemon = ctx.start_daemon_in(&home, kind, false)?;
    cli.wait_run_state(unconfigured, crate::scenario::STARTUP_TIMEOUT)?;
    let listed = cli.json(&["profiles", "list", "--json"])?;
    ensure!(
        listed["profiles"] == serde_json::json!([]) && listed["problems"] == serde_json::json!([]),
        "a fresh install lists no profile and no problem: {listed}"
    );

    // With no terminal there is nobody to ask for the folders, and a
    // suggestion that nobody saw is no choice: the command refuses, and
    // the install stays as it was.
    let no_folders = cli.run(&["profiles", "add", "--json"])?;
    ensure!(
        !no_folders.success() && no_folders.stderr.contains("--local <path> --cloud <path>"),
        "profiles add with no folders and no terminal must refuse: {}",
        no_folders.stderr
    );
    let listed = cli.json(&["profiles", "list", "--json"])?;
    ensure!(
        listed["profiles"] == serde_json::json!([]),
        "a refused add leaves no profile: {listed}"
    );

    // One command sets the install up; the running daemon picks it up.
    let local = home.local.to_string_lossy().into_owned();
    let cloud = home.cloud.to_string_lossy().into_owned();
    let added = cli.json(&[
        "profiles",
        "add",
        "--provider",
        "filesystem",
        "--local",
        &local,
        "--cloud",
        &cloud,
        "--json",
    ])?;
    ensure!(
        added["profile"]["id"] == "default" && added["applies"] == "live",
        "the first profile is the default one and applies live: {added}"
    );
    cli.wait_run_state("running", CONVERGE_TIMEOUT)?;
    let status = cli
        .status()
        .ok_or_else(|| Failure::new("status --json did not answer"))?;
    ensure!(
        status.restart_required.is_none(),
        "the first profile must not ask for a restart: {:?}",
        status.restart_required
    );
    let mark = ctx.mark();
    write_file(&home.local.join("first.txt"), "the first file\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_same_content(
        &home.local.join("first.txt"),
        &home.cloud.join("first.txt"),
        CONVERGE_TIMEOUT,
    )?;

    // A profile the daemon would refuse is refused here, and not written.
    let second_local = home.dir.join("second");
    let second_cloud = second_local.join("cloud");
    let before = fs::read_to_string(home.config_path())?;
    let overlapping = cli.run(&[
        "profiles",
        "add",
        "--id",
        "second",
        "--provider",
        "filesystem",
        "--local",
        &second_local.to_string_lossy(),
        "--cloud",
        &second_cloud.to_string_lossy(),
    ])?;
    ensure!(
        !overlapping.success() && overlapping.stderr.contains("invalid sync directories"),
        "a cloud folder inside the local folder must be refused with the reason: {}",
        overlapping.combined()
    );
    ensure!(
        fs::read_to_string(home.config_path())? == before,
        "a refused profile must not change vapor.json"
    );

    // So is a local folder inside the folder of another profile: the
    // outer profile would sync the files of the inner one a second time.
    let nested_local = home.local.join("inner");
    let nested_cloud = home.dir.join("nested-cloud");
    let nested = cli.run(&[
        "profiles",
        "add",
        "--id",
        "nested",
        "--provider",
        "filesystem",
        "--local",
        &nested_local.to_string_lossy(),
        "--cloud",
        &nested_cloud.to_string_lossy(),
    ])?;
    ensure!(
        !nested.success()
            && nested
                .stderr
                .contains("overlaps the one of profile `default`"),
        "a local folder inside the one of another profile must be refused with the reason: {}",
        nested.combined()
    );
    ensure!(
        fs::read_to_string(home.config_path())? == before,
        "a refused profile must not change vapor.json"
    );
    ensure!(
        !nested_local.exists() && !nested_cloud.exists(),
        "a refused profile must create no folder"
    );

    // So is a pull-only profile on the folder of another profile: it
    // would remove every file that its own cloud folder does not hold.
    let mirror_cloud = home.dir.join("mirror-cloud");
    let mirror = cli.run(&[
        "profiles",
        "add",
        "--id",
        "mirror",
        "--provider",
        "filesystem",
        "--local",
        &home.local.to_string_lossy(),
        "--cloud",
        &mirror_cloud.to_string_lossy(),
        "--mode",
        "pull-only",
    ])?;
    ensure!(
        !mirror.success() && mirror.stderr.contains("`pull-only`"),
        "a pull-only profile on a shared local folder must be refused with the reason: {}",
        mirror.combined()
    );
    ensure!(
        fs::read_to_string(home.config_path())? == before && !mirror_cloud.exists(),
        "a refused profile must change nothing"
    );

    // The daemon holds the profile's state: remove waits for a stop.
    let refused = cli.run(&["profiles", "remove", "default", "--yes"])?;
    ensure!(
        !refused.success() && refused.stderr.contains("vapor service stop"),
        "remove must refuse while the daemon runs: {}",
        refused.combined()
    );
    ctx.stop_daemon(daemon)?;
    let removed = cli.json(&["profiles", "remove", "default", "--yes", "--json"])?;
    ensure!(
        removed["removed"] == "default" && removed["localRoot"] == local.as_str(),
        "remove reports the profile and its folders: {removed}"
    );
    for root in [&home.local, &home.cloud] {
        ensure!(
            root.join("first.txt").is_file(),
            "removing a profile must leave the files in {}",
            root.display()
        );
    }
    let databases: Vec<_> = walk_files(&home.dir.join("state"))
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("sqlite"))
        })
        .collect();
    ensure!(
        databases.is_empty(),
        "the removed profile's sync state must be gone: {databases:?}"
    );

    // The install is unconfigured again, and the folders stay as they are.
    ctx.start_daemon_in(&home, kind, false)?;
    cli.wait_run_state(unconfigured, crate::scenario::STARTUP_TIMEOUT)?;
    ensure!(
        home.local.join("first.txt").is_file() && home.cloud.join("first.txt").is_file(),
        "a daemon with no profile touches no folder"
    );
    Ok(())
}

/// Every file under `root`, recursively.
fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn daemon_startup(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ensure!(
        !home.local.exists(),
        "local root must not exist before the daemon starts"
    );
    ctx.start_daemon()?;
    ensure!(
        home.local.is_dir(),
        "daemon did not create the missing local sync root"
    );
    // Canonical socket placement is only asserted when the sandbox path
    // fits the socket-address budget; deeper checkouts relocate (S09).
    let socket = home.socket_path();
    if socket.as_os_str().len() <= 100 {
        ensure!(
            is_socket(&socket),
            "IPC socket not present at {}",
            socket.display()
        );
    } else {
        ctx.note("socket placement not asserted: sandbox path over the address budget");
    }
    let status = ctx
        .cli()
        .status()
        .ok_or_else(|| Failure::new("status --json did not answer"))?;
    ensure!(
        status.provider == vapor_shared::constants::provider::FILESYSTEM,
        "provider should name the filesystem provider, got {:?}",
        status.provider
    );
    Ok(())
}

fn local_ingest_converges(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    let db = ctx.db();
    ensure!(
        db.enqueue_high_water().is_some(),
        "cannot read the state DB at {}",
        db.path.display()
    );
    let mark = ctx.mark();
    for index in 1..=3 {
        write_file(
            &home.local.join(format!("e2e-file-{index}.txt")),
            format!("vapor e2e payload {index}\n"),
        )?;
    }
    ctx.converge_from(&mark, 3, CONVERGE_TIMEOUT)?;
    for index in 1..=3 {
        let name = format!("e2e-file-{index}.txt");
        ctx.wait_same_content(
            &home.local.join(&name),
            &home.cloud.join(&name),
            CONVERGE_TIMEOUT,
        )?;
    }
    Ok(())
}

fn pause_resume(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    let cli = ctx.cli();
    cli.pause()?;
    cli.wait_run_state("paused", Duration::from_secs(10))?;
    let mark = ctx.mark();
    write_file(&home.local.join("e2e-paused.txt"), "written while paused\n")?;
    cli.resume()?;
    cli.wait_run_state("running", Duration::from_secs(10))?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("e2e-paused.txt"), CONVERGE_TIMEOUT)?;
    Ok(())
}

fn singleton_lock(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    // Bounded: if the lock regresses the second daemon enters the tick
    // loop and never returns, so we poll for exit instead of waiting.
    let second = ctx.start_daemon_in(&home, DaemonKind::CliRun, false)?;
    let status = ctx
        .daemon(second)
        .wait_exit(Duration::from_secs(15))
        .ok_or_else(|| {
            Failure::new("second daemon did not exit within 15s (singleton-lock regression?)")
        })?;
    ensure!(!status.success(), "second daemon did not exit non-zero");
    let output = fs::read_to_string(&ctx.daemon(second).output_path)?;
    ensure!(
        output.to_lowercase().contains("already running"),
        "second daemon refusal message missing; output: {output}"
    );
    // Both daemons wrote to the same capture file; the refusal is
    // logged at ERROR by design.
    ctx.allow_error("Refusing to start: another daemon already serves this vapor directory");
    Ok(())
}

fn doctor(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    ctx.start_daemon()?;
    let cli = ctx.cli();
    cli.ok(&["doctor"])?;
    let report = cli.json(&["doctor", "--json"])?;
    ensure!(
        report.get("worstStatus").is_some(),
        "doctor --json lacks worstStatus"
    );
    let checks = report
        .get("checks")
        .and_then(|value| value.as_array())
        .ok_or_else(|| Failure::new("doctor --json lacks the checks array"))?;
    let names: Vec<&str> = checks
        .iter()
        .filter_map(|check| check.get("name").and_then(|name| name.as_str()))
        .collect();
    for required in ["vapord-binary", "secret-store"] {
        ensure!(
            names.contains(&required),
            "doctor --json lacks the {required} row; rows: {names:?}"
        );
    }
    Ok(())
}

fn restart_recovery(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    let first = ctx.start_daemon()?;
    let mark = ctx.mark();
    write_file(&home.local.join("before-restart.txt"), "before\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.stop_daemon(first)?;
    ctx.start_daemon()?;
    let mark = ctx.mark();
    write_file(&home.local.join("after-restart.txt"), "after\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("after-restart.txt"), CONVERGE_TIMEOUT)?;
    Ok(())
}

fn log_hygiene(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    for round in 0..2 {
        let daemon = ctx.start_daemon()?;
        let mark = ctx.mark();
        write_file(
            &home.local.join(format!("round-{round}.txt")),
            format!("round {round}\n"),
        )?;
        ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
        ctx.stop_daemon(daemon)?;
    }
    let report = logs::scan(&home.daemon_log(), &[], &[]);
    ensure!(
        report.line_count > 0,
        "daemon log is empty at {}",
        home.daemon_log().display()
    );
    ensure!(
        report.errors.is_empty(),
        "daemon log contains ERROR lines: {:?}",
        report.errors
    );
    ensure!(
        report.unexpected_warnings.is_empty(),
        "daemon log contains warnings: {:?}",
        report.unexpected_warnings
    );
    ctx.note(format!("{} log lines, no warnings", report.line_count));
    Ok(())
}

fn socket_relocation(ctx: &mut Ctx) -> Result<(), Failure> {
    let deep = ctx.sandbox.deep_home()?;
    ctx.register_home(deep.clone());
    ctx.configure_scope(&deep)?;
    ensure!(
        deep.socket_path().as_os_str().len() > 104,
        "deep home is not over the socket-address budget: {}",
        deep.socket_path().display()
    );
    let daemon = ctx.start_daemon_in(&deep, DaemonKind::CliRun, true)?;
    ensure!(
        !is_socket(&deep.socket_path()),
        "socket bound at the canonical over-budget path instead of relocating"
    );
    let cli = ctx.cli_for(&deep);
    let doctor = cli.ok(&["doctor"])?;
    ensure!(
        doctor.contains("rendezvous"),
        "vapor doctor does not explain the socket relocation: {doctor}"
    );
    let line =
        logs::last_line_containing(&deep.daemon_log(), "relocated under the OS temp directory")
            .ok_or_else(|| Failure::new("daemon log does not record the relocation"))?;
    let relocated = line
        .split("socket_path=")
        .nth(1)
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .unwrap_or_default();
    ensure!(
        !relocated.is_empty(),
        "cannot parse socket_path from: {line}"
    );
    ctx.stop_daemon(daemon)?;
    // Only remove a positively recognised relocation directory.
    let socket = std::path::Path::new(&relocated);
    let parent = socket.parent().unwrap_or(socket);
    let parent_name = parent
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if socket.file_name().is_some_and(|name| name == "vapord.sock")
        && parent_name.starts_with("vapor-")
    {
        let _ = fs::remove_dir_all(parent);
    } else {
        let _ = fs::remove_file(socket);
        ctx.note(format!(
            "relocated dir shape unexpected ({}); removed only the socket file",
            parent.display()
        ));
    }
    ensure!(
        !socket.exists(),
        "relocated socket residue left at {relocated}"
    );
    // The primary home never started a daemon; only the deep home has trees.
    ctx.set_oracle(crate::scenario::OracleMode::Homes(vec!["deep".to_string()]));
    Ok(())
}

fn is_socket(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_socket())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}
