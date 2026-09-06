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
    ctx.configure_scope(&home)?;
    let local = cli.config_get("localSyncDirectory")?;
    ensure!(
        local == home.local.to_string_lossy(),
        "config get did not round-trip localSyncDirectory: got {local:?}"
    );
    ensure!(
        home.config_path().is_file(),
        "vapor.json was not written under VAPOR_DIR"
    );
    // Structured keys are stored as JSON (a string containing JSON would
    // make the daemon ignore the group) and unset keys read as defaults.
    cli.config_set("safeguards", r#"{"massDeleteThreshold": 500}"#)?;
    let raw = fs::read_to_string(home.config_path())?;
    ensure!(
        raw.contains("\"massDeleteThreshold\": 500"),
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
        status.provider_name.to_lowercase().contains("filesystem"),
        "provider_name should name the filesystem provider, got {:?}",
        status.provider_name
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
    cli.wait_run_state("Paused", Duration::from_secs(10))?;
    let mark = ctx.mark();
    write_file(&home.local.join("e2e-paused.txt"), "written while paused\n")?;
    cli.resume()?;
    cli.wait_run_state("Running", Duration::from_secs(10))?;
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
        report.get("worst_status").is_some(),
        "doctor --json lacks worst_status"
    );
    let checks = report
        .get("checks")
        .and_then(|value| value.as_array())
        .ok_or_else(|| Failure::new("doctor --json lacks the checks array"))?;
    let names: Vec<&str> = checks
        .iter()
        .filter_map(|check| check.get("name").and_then(|name| name.as_str()))
        .collect();
    for required in ["vapord_binary", "secret_store"] {
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
