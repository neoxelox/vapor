//! The shell contract of the built `vapor` binary: exit codes, what
//! goes to stdout and what to stderr, and the `--json` shapes the app
//! shells parse, exercised by spawning the real executable against a
//! throwaway runtime directory. No daemon is started, so every test
//! runs on every CI OS; the daemon paths belong to Tier E2E.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn vapor(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vapor"))
        .args(args)
        .env("VAPOR_DIR", home)
        .env("VAPOR_ENV", "dev")
        .env_remove("VAPOR_LOCAL_SYNC_DIRECTORY")
        .env_remove("VAPOR_CLOUD_SYNC_DIRECTORY")
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
        "config",
        "service",
        "conflicts",
        "decisions",
        "trash",
        "doctor",
        "support-bundle",
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
    assert!(report["worst_status"].is_string(), "{report}");
    // The exit code follows the worst check, never the JSON rendering.
    let expected = if report["worst_status"] == "failure" {
        Some(1)
    } else {
        Some(0)
    };
    assert_eq!(output.status.code(), expected, "{report}");
}

#[test]
fn conflicts_refuse_to_touch_a_file_that_is_not_a_conflict_copy() {
    let home = TempDir::new().expect("home");
    let plain = home.path().join("notes.txt");
    std::fs::write(&plain, b"not a copy").expect("seed");
    let output = vapor(
        home.path(),
        &[
            "conflicts",
            "resolve",
            plain.to_str().expect("utf-8"),
            "--keep",
            "canonical",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(plain.is_file(), "the file must be left alone");
    assert!(stderr(&output).contains("conflict"), "{}", stderr(&output));
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
