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
        "init",
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

/// The exit codes of `core/cli/README.md`, command by command, on a
/// fresh runtime directory with no daemon.
#[test]
fn each_command_ends_with_its_documented_exit_code() {
    let home = TempDir::new().expect("home");
    let cases: &[(&[&str], i32)] = &[
        // 0: done, and "nothing to do" counts.
        (&["version"], 0),
        (&["config", "get", "provider"], 0),
        (&["profiles", "list"], 0),
        (&["decisions", "list"], 0),
        (&["trash", "list"], 0),
        (&["trash", "empty", "--yes"], 0),
        (&["ignored"], 0),
        (&["auth", "status"], 0),
        (&["logs"], 0),
        // 1: it could not be done.
        (&["config", "get", "nope"], 1),
        (&["config", "set", "nope", "1"], 1),
        (&["profiles", "enable", "nope"], 1),
        (&["profiles", "rename", "nope", "Name"], 1),
        (&["decisions", "show", "99"], 1),
        (&["decisions", "resolve", "99", "--choose", "skip"], 1),
        (&["trash", "restore", "99"], 1),
        (&["init"], 1),
        // 1: the commands that need the daemon, with none running.
        (&["status"], 1),
        (&["pause"], 1),
        (&["resume"], 1),
        (&["reconcile"], 1),
        (&["timeline"], 1),
        (&["diagnostics"], 1),
        // 2: a command line that is wrong.
        (&["nope"], 2),
        (&["status", "--nope"], 2),
        (&["profiles", "rename", "only-an-id"], 2),
        (&["config", "set", "provider"], 2),
    ];
    for (args, expected) in cases {
        let output = vapor(home.path(), args);
        assert_eq!(
            output.status.code(),
            Some(*expected),
            "vapor {}: stdout {:?}, stderr {:?}",
            args.join(" "),
            stdout(&output),
            stderr(&output)
        );
        if *expected != 0 {
            assert!(
                !stderr(&output).is_empty(),
                "vapor {} says nothing on stderr",
                args.join(" ")
            );
        }
    }
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

    // One edit of one profile: the result on stdout, a refusal on
    // stderr with exit code 1.
    let disabled = vapor(home.path(), &["profiles", "disable", "default", "--json"]);
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert_eq!(json(&disabled)["profile"]["enabled"], false);
    assert_eq!(json(&disabled)["changed"], true);
    let renamed = vapor(home.path(), &["profiles", "rename", "default", "Home"]);
    assert!(renamed.status.success(), "{}", stderr(&renamed));
    assert!(
        stdout(&renamed).contains("\"Home\""),
        "{}",
        stdout(&renamed)
    );
    let enabled = vapor(home.path(), &["profiles", "enable", "default"]);
    assert!(enabled.status.success(), "{}", stderr(&enabled));
    let unknown = vapor(home.path(), &["profiles", "enable", "nope", "--json"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stdout(&unknown).is_empty(), "{}", stdout(&unknown));
    assert!(stderr(&unknown).contains("no profile 'nope'"));

    // No terminal and no --yes: remove does not guess a confirmation.
    let unconfirmed = vapor(home.path(), &["profiles", "remove", "default"]);
    assert_eq!(unconfirmed.status.code(), Some(1));
    assert!(stderr(&unconfirmed).contains("--yes"));
    let removed = vapor(home.path(), &["profiles", "remove", "default", "--yes"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let listed = vapor(home.path(), &["profiles", "list"]);
    assert!(stdout(&listed).contains("vapor init"));
}

#[test]
fn init_sets_an_install_up_and_a_second_run_changes_nothing() {
    let home = TempDir::new().expect("home");
    let local = home.path().join("local");
    let cloud = home.path().join("cloud");
    // `--auto-launch off` in each call that could register a service:
    // a test never does that to the host. The last call needs none,
    // and proves why: the answer is in `vapor.json` by then, and it is
    // read before the service manager is looked at.

    // Nobody to ask and no folders: a refusal on stderr, nothing on
    // stdout, nothing written.
    let refused = vapor(home.path(), &["init", "--auto-launch", "off", "--json"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stdout(&refused).is_empty(), "{}", stdout(&refused));
    assert!(
        stderr(&refused).contains("--local <path> --cloud <path>"),
        "{}",
        stderr(&refused)
    );
    assert!(!home.path().join("vapor.json").exists());

    let first = vapor(
        home.path(),
        &[
            "init",
            "--local",
            &local.to_string_lossy(),
            "--cloud",
            &cloud.to_string_lossy(),
            "--auto-launch",
            "off",
            "--json",
        ],
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let report = json(&first);
    assert_eq!(report["initialized"], true, "{report}");
    let states: Vec<(&str, &str)> = report["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| {
            (
                step["step"].as_str().expect("step"),
                step["state"].as_str().expect("state"),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            ("profile", "done"),
            ("sign-in", "not-needed"),
            ("service", "off"),
        ]
    );
    assert_eq!(report["profiles"][0]["id"], "default");
    assert!(report.get("daemon").is_none(), "no daemon runs: {report}");
    assert!(!local.exists(), "init creates no folder");

    // The second run changes nothing and says so in words.
    let written = std::fs::read_to_string(home.path().join("vapor.json")).expect("config");
    let second = vapor(home.path(), &["init"]);
    assert!(second.status.success(), "{}", stderr(&second));
    let text = stdout(&second);
    assert!(text.starts_with("Vapor is initialized.\n"), "{text}");
    assert!(text.contains("daemon: not running"), "{text}");
    assert_eq!(
        std::fs::read_to_string(home.path().join("vapor.json")).expect("config"),
        written
    );
}

#[test]
fn init_exits_one_while_a_step_waits_for_a_person() {
    let home = TempDir::new().expect("home");
    // A provider with an account, no terminal for the browser, and no
    // token on stdin: nobody can sign in here, and the command says so
    // with exit code 1.
    let waiting = vapor(
        home.path(),
        &[
            "init",
            "--provider",
            "gdrive",
            "--local",
            &home.path().join("local").to_string_lossy(),
            "--cloud",
            "/Vapor",
            "--auto-launch",
            "off",
            "--json",
        ],
    );
    assert_eq!(waiting.status.code(), Some(1), "{}", stderr(&waiting));
    let report = json(&waiting);
    assert_eq!(report["initialized"], false, "{report}");
    let sign_in = report["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|step| step["step"] == "sign-in")
        .unwrap_or_else(|| panic!("no sign-in step: {report}"));
    match sign_in["state"].as_str() {
        // The host has a secret store: the profile is written, and the
        // sign-in waits for a person.
        Some("waits") => assert_eq!(report["profiles"][0]["provider"], "gdrive", "{report}"),
        // The host has none (a bare Linux runner, Windows): a sign-in
        // would be gone when the command ends, so the step fails with
        // that reason and nothing is written.
        Some("failed") => {
            assert!(
                sign_in["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("keeps no sign-in")),
                "{report}"
            );
            assert_eq!(report["profiles"], serde_json::json!([]), "{report}");
        }
        other => panic!("the sign-in step must wait or fail, got {other:?}: {report}"),
    }

    // A token on the command line is refused before anything runs.
    let on_the_command_line = vapor(home.path(), &["init", "--token", "ya29.test"]);
    assert_eq!(on_the_command_line.status.code(), Some(1));
    let reason = stderr(&on_the_command_line);
    assert!(reason.contains("process list"), "{reason}");
    assert!(
        !reason.contains("ya29"),
        "the refusal must not repeat the token: {reason}"
    );
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
        &["decisions", "resolve", "7", "--choose", "skip"],
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
