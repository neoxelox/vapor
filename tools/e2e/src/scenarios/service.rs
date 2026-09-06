//! The service lifecycle round-trip against the real macOS service
//! manager: install, start, status, crash-loop supervision through
//! backoff and pause, acknowledge, stop, uninstall. The one phase of
//! Tier E2E that mutates host state (a LaunchAgent plist and a launchd
//! registration), which is why it needs `--full`, refuses when a Vapor
//! LaunchAgent already exists, and removes the LaunchAgent on every
//! exit path.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::cli::Cli;
use crate::host::{Need, launch_agent_plist_path, launchd_domain_target};
use crate::scenario::{CONVERGE_TIMEOUT, Ctx, Expect, OracleMode, Scenario, write_file};
use crate::wait;
use crate::{Failure, ensure};

pub fn scenarios() -> Vec<Scenario> {
    vec![Scenario {
        id: "R01",
        name: "service-round-trip",
        proves: "install → start → status → crash-loop supervision through backoff and pause → acknowledge → stop → uninstall against real launchd",
        needs: &[
            Need::NativeWatcher,
            Need::Filesystem,
            Need::Full,
            Need::Launchd,
        ],
        expect: Expect::Pass,
        run: service_round_trip,
    }]
}

/// Removes the LaunchAgent and any straggler daemon on drop, so the
/// host is clean even when a step fails.
struct LaunchAgentGuard {
    cli: Cli,
    plist: PathBuf,
    vapord_bin: PathBuf,
}

impl Drop for LaunchAgentGuard {
    fn drop(&mut self) {
        let _ = self.cli.run(&["service", "uninstall"]);
        let _ = Command::new("launchctl")
            .args(["bootout", &launchd_domain_target()])
            .arg(&self.plist)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = fs::remove_file(&self.plist);
        kill_stragglers(&self.vapord_bin);
    }
}

/// Kills every `vapord` whose executable is this repository's build,
/// matched by path (never by a regex over the repo path).
fn kill_stragglers(vapord_bin: &std::path::Path) {
    let Ok(output) = Command::new("pgrep").args(["-x", "vapord"]).output() else {
        return;
    };
    for pid in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        let Ok(exe) = Command::new("ps").args(["-p", pid, "-o", "comm="]).output() else {
            continue;
        };
        let exe = String::from_utf8_lossy(&exe.stdout).trim().to_string();
        if exe == vapord_bin.to_string_lossy() {
            let _ = Command::new("kill").arg(pid).status();
        }
    }
}

fn daemon_pid_from_launchd() -> Option<String> {
    let output = Command::new("launchctl")
        .args([
            "print",
            &format!(
                "{}/{}",
                launchd_domain_target(),
                vapor_shared::constants::service::DAEMON_LABEL
            ),
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("pid = ")
                .map(|pid| pid.trim().to_string())
        })
}

struct Svc<'a> {
    cli: &'a Cli,
}

impl Svc<'_> {
    /// Runs `vapor service <args> --json`; a non-zero exit is always a
    /// failure worth diagnostics (deferred and paused outcomes exit 0).
    fn json(&self, step: &str, args: &[&str]) -> Result<serde_json::Value, Failure> {
        let mut full = vec!["service"];
        full.extend_from_slice(args);
        full.push("--json");
        self.cli
            .json(&full)
            .map_err(|error| Failure::new(format!("{step}: {error}")))
    }

    fn expect(
        &self,
        step: &str,
        args: &[&str],
        key: &str,
        value: &str,
    ) -> Result<serde_json::Value, Failure> {
        let out = self.json(step, args)?;
        let actual = out.get(key).cloned().unwrap_or(serde_json::Value::Null);
        ensure!(
            actual == serde_json::Value::String(value.to_string()),
            "{step}: expected {key} = {value:?} in {out}"
        );
        Ok(out)
    }

    fn status_is(&self, expected: &str) -> bool {
        self.json("status", &["status"])
            .ok()
            .and_then(|out| {
                out.get("status")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .as_deref()
            == Some(expected)
    }

    fn wait_status(&self, expected: &str, timeout: Duration, step: &str) -> Result<(), Failure> {
        wait::wait_until(
            timeout,
            &format!("{step}: service status to report {expected}"),
            || self.status_is(expected),
        )
    }

    /// Kills the launchd-managed daemon and waits for launchd to notice.
    fn crash(&self, step: &str) -> Result<(), Failure> {
        // `launchctl print` reports a pid while the job is still in its
        // spawn stage; wait for the daemon to answer over IPC so the
        // crash we simulate is the crash of a running daemon.
        self.cli
            .wait_run_state("Running", Duration::from_secs(30))
            .map_err(|error| Failure::new(format!("{step}: before the crash, {error}")))?;
        let pid = daemon_pid_from_launchd()
            .ok_or_else(|| Failure::new(format!("{step}: daemon pid not found via launchctl")))?;
        let killed = Command::new("kill").args(["-KILL", &pid]).status()?;
        ensure!(killed.success(), "{step}: could not SIGKILL pid {pid}");
        self.wait_status("stopped", Duration::from_secs(15), step)
    }

    /// Polls `service check` until its health equals `expected`.
    fn wait_health(&self, expected: &str, timeout: Duration, step: &str) -> Result<(), Failure> {
        wait::wait_until(
            timeout,
            &format!("{step}: service check to report health {expected}"),
            || {
                self.json(step, &["check"])
                    .ok()
                    .and_then(|out| {
                        out.get("health")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(expected)
            },
        )
    }
}

fn service_round_trip(ctx: &mut Ctx) -> Result<(), Failure> {
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    let cli = ctx.cli();
    let _guard = LaunchAgentGuard {
        cli: cli.clone(),
        plist: launch_agent_plist_path(),
        vapord_bin: ctx.paths.vapord_bin.clone(),
    };
    let svc = Svc { cli: &cli };

    // R1: a fresh runner reports not_installed.
    svc.expect("R1", &["status"], "status", "not_installed")?;

    // R2: install writes the plist, the daemon runs inside the sandbox.
    svc.expect("R2", &["install"], "result", "started")?;
    ensure!(
        launch_agent_plist_path().is_file(),
        "R2: LaunchAgent plist not written"
    );
    svc.wait_status("running", Duration::from_secs(30), "R2")?;
    cli.wait_run_state("Running", Duration::from_secs(30))?;
    svc.expect("R2", &["check"], "health", "running")?;
    // The launchd daemon syncs like any other.
    let mark = ctx.mark();
    write_file(&home.local.join("via-launchd.txt"), "served by launchd\n")?;
    ctx.converge_from(&mark, 1, CONVERGE_TIMEOUT)?;
    ctx.wait_exists(&home.cloud.join("via-launchd.txt"), CONVERGE_TIMEOUT)?;

    // R3: crash 1 restarts immediately.
    svc.crash("R3")?;
    svc.expect("R3", &["check"], "health", "restarted_after_crash")?;
    svc.wait_status("running", Duration::from_secs(30), "R3")?;

    // R4: crash 2 defers, a repeat check does not double-count, then
    // the restart happens once the backoff elapses.
    svc.crash("R4")?;
    svc.expect("R4", &["check"], "health", "restart_deferred")?;
    let status = svc.json("R4", &["status"])?;
    ensure!(
        status.pointer("/crash_loop/consecutive_crashes") == Some(&serde_json::json!(2)),
        "R4: expected consecutive_crashes 2 in {status}"
    );
    svc.expect("R4", &["check"], "health", "restart_deferred")?;
    let status = svc.json("R4", &["status"])?;
    ensure!(
        status.pointer("/crash_loop/consecutive_crashes") == Some(&serde_json::json!(2)),
        "R4: repeat check double-counted: {status}"
    );
    svc.wait_health("restarted_after_crash", Duration::from_secs(15), "R4")?;
    svc.wait_status("running", Duration::from_secs(30), "R4")?;

    // R5: crashes 3 and 4 walk the backoff schedule.
    svc.crash("R5")?;
    svc.expect("R5", &["check"], "health", "restart_deferred")?;
    svc.wait_health("restarted_after_crash", Duration::from_secs(20), "R5")?;
    svc.wait_status("running", Duration::from_secs(30), "R5")?;
    svc.crash("R5")?;
    svc.expect("R5", &["check"], "health", "restart_deferred")?;
    svc.wait_health("restarted_after_crash", Duration::from_secs(30), "R5")?;
    svc.wait_status("running", Duration::from_secs(30), "R5")?;

    // R6: crash 5 engages the durable pause; start refuses.
    svc.crash("R6")?;
    svc.expect("R6", &["check"], "health", "crash_loop_paused")?;
    let status = svc.expect("R6", &["status"], "status", "crash_loop_paused")?;
    ensure!(
        status.pointer("/crash_loop/paused") == Some(&serde_json::json!(true)),
        "R6: crash_loop.paused not true in {status}"
    );
    svc.expect("R6", &["start"], "result", "crash_loop_paused")?;
    let lifecycle = fs::read_to_string(home.lifecycle_state())?;
    ensure!(
        lifecycle.contains("\"paused_indefinitely\": true"),
        "R6: pause not persisted in lifecycle.json: {lifecycle}"
    );

    // R7: acknowledge, then start works again.
    svc.expect("R7", &["acknowledge"], "result", "acknowledged")?;
    svc.expect("R7", &["start"], "result", "started")?;
    svc.wait_status("running", Duration::from_secs(30), "R7")?;

    // R8: an expected stop is not a crash.
    svc.expect("R8", &["stop"], "result", "stopped")?;
    svc.wait_status("stopped", Duration::from_secs(15), "R8")?;
    svc.expect("R8", &["check"], "health", "stopped_expected")?;

    // R9: uninstall removes the plist.
    svc.expect("R9", &["uninstall"], "result", "unchanged")
        .or_else(|_| svc.json("R9", &["uninstall"]))?;
    ensure!(
        !launch_agent_plist_path().exists(),
        "R9: plist still present after uninstall"
    );
    svc.expect("R9", &["status"], "status", "not_installed")?;
    svc.expect("R9", &["check"], "health", "not_installed")?;

    // The launchd daemon was SIGKILLed five times; its log holds the
    // refusals and restarts the round-trip provoked on purpose.
    ctx.allow_error("Refusing to start");
    ctx.allow_warning("crash");
    ctx.allow_warning("Crash");
    ctx.allow_warning("unexpected");
    ctx.set_oracle(OracleMode::Homes(vec!["primary".to_string()]));
    Ok(())
}
