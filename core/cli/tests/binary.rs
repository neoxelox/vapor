//! The shell contract of the built `vapor` binary: exit codes, what
//! goes to stdout and what to stderr, and the `--json` shapes the app
//! shells parse, exercised by spawning the real executable against a
//! throwaway runtime directory. No daemon is started, so every test
//! runs on every CI OS; the daemon paths belong to Tier E2E.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Runs the binary with the throwaway directory as its runtime
/// directory and as its home directory, so neither a `~` path nor the
/// login keychain of whoever runs the tests is in reach.
fn vapor(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vapor"))
        .args(args)
        .env("VAPOR_DIR", home)
        .env("VAPOR_ENV", "dev")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("vapor binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {:?} / stderr {:?}",
            stdout(output),
            stderr(output)
        )
    })
}

#[test]
fn version_prints_one_line_and_exits_zero() {
    let home = TempDir::new().expect("home");
    let output = vapor(home.path(), &["version"]);
    assert!(output.status.success());
    let printed = stdout(&output);
    assert_eq!(printed.trim().lines().count(), 1, "{printed:?}");
    assert!(printed.contains(env!("CARGO_PKG_VERSION")), "{printed:?}");
    assert!(stderr(&output).is_empty());
}

#[test]
fn help_names_every_command() {
    let home = TempDir::new().expect("home");
    let output = vapor(home.path(), &["--help"]);
    assert!(output.status.success());
    let help = stdout(&output);
    for command in [
        "run",
        "status",
        "profiles",
        "ignored",
        "config",
        "service",
        "decisions",
        "trash",
        "doctor",
        "report",
    ] {
        assert!(help.contains(command), "help lacks {command}: {help}");
    }
}

#[test]
fn an_unknown_command_is_a_usage_error_on_stderr() {
    let home = TempDir::new().expect("home");
    let output = vapor(home.path(), &["frobnicate"]);
    assert_eq!(output.status.code(), Some(2), "clap's usage exit code");
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("frobnicate"));
}

#[test]
fn status_without_a_daemon_fails_with_the_reason_on_stderr_only() {
    let home = TempDir::new().expect("home");
    let output = vapor(home.path(), &["status"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).is_empty(),
        "stdout must stay clean for scripts"
    );
    let message = stderr(&output);
    assert!(message.starts_with("vapor: "), "{message:?}");
    // On an OS whose IPC transport has not shipped the reason is the
    // transport, not a missing daemon.
    assert!(
        message.contains("daemon") || message.contains("transport"),
        "{message:?}"
    );
    // The same holds with --json: an error never prints half a document.
    let output = vapor(home.path(), &["status", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
}

#[test]
fn config_round_trips_and_types_its_keys() {
    let home = TempDir::new().expect("home");
    let set = vapor(home.path(), &["config", "set", "timelineLimit", "250"]);
    assert!(set.status.success(), "{}", stderr(&set));
    let get = vapor(home.path(), &["config", "get", "timelineLimit"]);
    assert!(get.status.success());
    assert_eq!(stdout(&get).trim(), "250");
    assert!(
        home.path().join("vapor.json").is_file(),
        "the file lives under VAPOR_DIR"
    );

    let bad_type = vapor(home.path(), &["config", "set", "timelineLimit", "lots"]);
    assert_eq!(bad_type.status.code(), Some(1));
    assert!(stdout(&bad_type).is_empty());
    assert!(
        stderr(&bad_type).contains("timelineLimit"),
        "{}",
        stderr(&bad_type)
    );

    let unknown = vapor(home.path(), &["config", "set", "noSuchKey", "1"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("noSuchKey"),
        "{}",
        stderr(&unknown)
    );

    let structured = vapor(
        home.path(),
        &["config", "set", "trash", "{\"retentionDays\": 7}"],
    );
    assert!(structured.status.success(), "{}", stderr(&structured));
    let not_json = vapor(home.path(), &["config", "set", "trash", "retentionDays=7"]);
    assert_eq!(not_json.status.code(), Some(1));
    assert!(stderr(&not_json).contains("JSON"), "{}", stderr(&not_json));
}

#[test]
fn profiles_set_an_install_up_without_a_daemon_and_never_ask_without_a_terminal() {
    let home = TempDir::new().expect("home");
    let listed = vapor(home.path(), &["profiles", "list", "--json"]);
    assert!(listed.status.success());
    assert_eq!(json(&listed)["profiles"], serde_json::json!([]));

    let local = home.path().join("local");
    let cloud = home.path().join("cloud");
    let added = vapor(
        home.path(),
        &[
            "profiles",
            "add",
            "--local",
            &local.to_string_lossy(),
            "--cloud",
            &cloud.to_string_lossy(),
            "--mode",
            "push-only",
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let text = stdout(&added);
    assert!(text.contains("Added profile default"), "{text}");
    assert!(
        text.contains("vapor decisions list"),
        "with no terminal a one-way mode is left for the daemon to ask: {text}"
    );
    assert!(!local.exists(), "adding a profile creates no folder");

    // No terminal and no --yes: remove does not guess a confirmation.
    let unconfirmed = vapor(home.path(), &["profiles", "remove", "default"]);
    assert_eq!(unconfirmed.status.code(), Some(1));
    assert!(stderr(&unconfirmed).contains("--yes"));
    let removed = vapor(home.path(), &["profiles", "remove", "default", "--yes"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let listed = vapor(home.path(), &["profiles", "list"]);
    assert!(stdout(&listed).contains("vapor profiles add"));
}

#[test]
fn a_token_on_the_command_line_is_refused_and_never_stored() {
    let home = TempDir::new().expect("home");
    let added = vapor(
        home.path(),
        &[
            "profiles",
            "add",
            "--provider",
            "gdrive",
            "--local",
            &home.path().join("local").to_string_lossy(),
            "--cloud",
            "/Vapor",
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));

    let login = vapor(home.path(), &["auth", "login", "--token", "ya29.test"]);
    assert_eq!(login.status.code(), Some(1));
    let reason = stderr(&login);
    assert!(reason.contains("process list"), "{reason}");
    assert!(reason.contains("--token -"), "{reason}");
    assert!(
        !reason.contains("ya29"),
        "the refusal must not repeat the token: {reason}"
    );
    assert!(stdout(&login).is_empty());
}

#[test]
fn decisions_and_trash_are_empty_and_well_formed_on_a_fresh_home() {
    let home = TempDir::new().expect("home");
    let decisions = vapor(home.path(), &["decisions", "list", "--json"]);
    assert!(decisions.status.success(), "{}", stderr(&decisions));
    let report = json(&decisions);
    assert_eq!(report["decisions"], serde_json::json!([]));
    assert_eq!(report["skippedProfiles"], serde_json::json!([]));

    let text = vapor(home.path(), &["decisions", "list"]);
    assert!(text.status.success());
    assert_eq!(stdout(&text).trim(), "No pending decisions.");

    let trash = vapor(home.path(), &["trash", "list", "--json"]);
    assert!(trash.status.success(), "{}", stderr(&trash));
    assert_eq!(json(&trash)["entries"], serde_json::json!([]));

    let missing = vapor(
        home.path(),
        &["decisions", "resolve", "7", "--choose", "apply"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("7"), "{}", stderr(&missing));
    // Emptying the trash cannot be undone: with no terminal to ask on,
    // it needs an explicit yes.
    let unconfirmed = vapor(home.path(), &["trash", "empty"]);
    assert_eq!(unconfirmed.status.code(), Some(1));
    assert!(
        stderr(&unconfirmed).contains("--yes"),
        "{}",
        stderr(&unconfirmed)
    );
    let confirmed = vapor(home.path(), &["trash", "empty", "--yes", "--json"]);
    assert!(confirmed.status.success());
    assert_eq!(json(&confirmed)["removed"], 0);

    let missing = vapor(home.path(), &["trash", "restore", "nope"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("nope"), "{}", stderr(&missing));
}

#[test]
fn doctor_json_is_a_check_list_with_a_worst_status() {
    let home = TempDir::new().expect("home");
    let output = vapor(home.path(), &["doctor", "--json"]);
    let report = json(&output);
    let checks = report["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty());
    for check in checks {
        assert!(check["name"].is_string(), "{check}");
        assert!(check["status"].is_string(), "{check}");
    }
    assert!(report["worstStatus"].is_string(), "{report}");
    // The exit code follows the worst check, never the JSON rendering.
    let expected = if report["worstStatus"] == "failure" {
        Some(1)
    } else {
        Some(0)
    };
    assert_eq!(output.status.code(), expected, "{report}");
}

#[test]
fn resolving_a_decision_that_does_not_exist_fails_on_stderr() {
    let home = TempDir::new().expect("home");
    let output = vapor(
        home.path(),
        &["decisions", "resolve", "99", "--choose", "report.md"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("99"), "{}", stderr(&output));
}

#[cfg(unix)]
#[test]
fn a_closed_stdout_pipe_ends_the_process_quietly() {
    // `vapor --help | head -c 1`: the reader goes away first. The CLI
    // must die on SIGPIPE like any Unix tool, not panic with a
    // backtrace on stderr.
    use std::io::Read;
    use std::process::Stdio;
    let home = TempDir::new().expect("home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vapor"))
        .args(["--help"])
        .env("VAPOR_DIR", home.path())
        .env("VAPOR_ENV", "dev")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut first = [0u8; 1];
    child
        .stdout
        .take()
        .expect("stdout")
        .read_exact(&mut first)
        .expect("one byte");
    // Dropping the reader closes the pipe; the child's next write
    // raises SIGPIPE.
    let output = child.wait_with_output().expect("wait");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(!errors.contains("panicked"), "{errors}");
}
