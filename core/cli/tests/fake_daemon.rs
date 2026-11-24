//! The IPC-backed commands of the built `vapor` binary against a
//! process that speaks the daemon's protocol and is no daemon: what
//! each command sends, what it prints for an answer, and how it ends
//! when the answer is a refusal, another protocol version, or nothing.
//!
//! The stand-in answers on the socket of a throwaway runtime directory
//! with the server half of `vapor-ipc`, so the wire format is the real
//! one. Unix only: the named-pipe transport of Windows has not shipped.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vapor_ipc::protocol::{
    AckResponse, DiagnosticsResponse, IncompatibleVersion, ProfileStatus, Response, ResponseBody,
    StateReason, StatusResponse, TimelineEntry, TimelineResponse, daemon_supported_versions,
};

/// What the stand-in was asked, in order.
#[derive(Default)]
struct Scripted {
    asked: Mutex<Vec<String>>,
    /// The profile names it has; a control request for another one is
    /// refused, as the daemon refuses it.
    profiles: Vec<&'static str>,
}

impl Scripted {
    fn ack(&self, method: &str, profile: Option<&str>) -> AckResponse {
        self.asked
            .lock()
            .expect("asked")
            .push(format!("{method} {}", profile.unwrap_or("*")));
        let known = profile.is_none_or(|profile| self.profiles.contains(&profile));
        AckResponse {
            schema_version: daemon_supported_versions().0,
            accepted: known,
            note: if known {
                format!("{method} taken")
            } else {
                format!("no profile '{}'", profile.unwrap_or_default())
            },
        }
    }
}

impl vapor_ipc::Service for Scripted {
    fn status(&self) -> StatusResponse {
        self.asked.lock().expect("asked").push("status".to_string());
        StatusResponse {
            schema_version: daemon_supported_versions().0,
            daemon: "vapord/stand-in".to_string(),
            provider: "filesystem".to_string(),
            run: StateReason {
                state: "running".to_string(),
                reason: "watching /sync/home".to_string(),
            },
            throttle: StateReason {
                state: "idle".to_string(),
                reason: "idle, plugged in, and cool".to_string(),
            },
            queued: 3,
            profiles: self
                .profiles
                .iter()
                .map(|id| ProfileStatus {
                    id: (*id).to_string(),
                    name: (*id).to_string(),
                    provider: "filesystem".to_string(),
                    run: StateReason {
                        state: "running".to_string(),
                        reason: String::new(),
                    },
                    ..ProfileStatus::default()
                })
                .collect(),
            ..StatusResponse::default()
        }
    }

    fn pause(&self, profile: Option<&str>) -> AckResponse {
        self.ack("pause", profile)
    }

    fn resume(&self, profile: Option<&str>) -> AckResponse {
        self.ack("resume", profile)
    }

    fn reconcile(&self, profile: Option<&str>) -> AckResponse {
        self.ack("reconcile", profile)
    }

    fn diagnostics(&self) -> DiagnosticsResponse {
        self.asked
            .lock()
            .expect("asked")
            .push("diagnostics".to_string());
        DiagnosticsResponse {
            schema_version: daemon_supported_versions().0,
            items: Vec::new(),
            truncated: false,
            dropped_events: 4,
            suppressed_echoes: 9,
        }
    }

    fn timeline(&self) -> TimelineResponse {
        self.asked
            .lock()
            .expect("asked")
            .push("timeline".to_string());
        TimelineResponse {
            schema_version: daemon_supported_versions().0,
            entries: vec![TimelineEntry {
                timestamp_ms: 1_795_000_000_000,
                kind: "profile".to_string(),
                message: "profile composed from the new configuration".to_string(),
                profile_id: "home".to_string(),
            }],
            truncated: false,
        }
    }
}

/// A runtime directory with something that listens on its socket.
struct StandIn {
    home: TempDir,
    service: Arc<Scripted>,
}

/// How the stand-in treats a connection.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Serves the protocol.
    Answers,
    /// Accepts the connection and never writes a byte: a daemon that
    /// hangs.
    Silent,
    /// Answers the handshake as a daemon whose protocol is too new for
    /// this CLI.
    AnotherVersion,
}

impl StandIn {
    fn start(behaviour: Behaviour) -> Self {
        // A short path: the socket address of a Unix socket has a small
        // fixed size, and the CLI moves a socket with a longer path.
        let home = tempfile::Builder::new()
            .prefix("vp")
            .tempdir()
            .expect("home");
        let location = vapor_shared::runtime_paths::ipc_socket_location_of(home.path());
        if let Some(parent) = location.path.parent() {
            std::fs::create_dir_all(parent).expect("socket directory");
        }
        let listener =
            vapor_ipc::transport::bind_listener(location.path.clone()).expect("bind the socket");
        let service = Arc::new(Scripted {
            asked: Mutex::new(Vec::new()),
            profiles: vec!["home", "work"],
        });
        let serving = service.clone();
        // The thread ends with the test process; the listener goes with it.
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.listener().incoming() {
                let Ok(stream) = stream else { continue };
                match behaviour {
                    Behaviour::Answers => {
                        let serving = serving.clone();
                        std::thread::spawn(move || {
                            let mut reader = stream.try_clone().expect("clone");
                            let mut writer = stream;
                            let _ = vapor_ipc::serve_connection(
                                &mut reader,
                                &mut writer,
                                serving.as_ref(),
                            );
                        });
                    }
                    Behaviour::Silent => held.push(stream),
                    Behaviour::AnotherVersion => {
                        let mut stream = stream;
                        let _ = vapor_ipc::read_frame(&mut stream);
                        let (current, _) = daemon_supported_versions();
                        let refusal =
                            Response::Ok(ResponseBody::IncompatibleVersion(IncompatibleVersion {
                                peer_version: current,
                                required_min: current + 7,
                                local_version: current + 9,
                            }));
                        let frame = serde_json::to_vec(&refusal).expect("frame");
                        let _ = vapor_ipc::write_frame(&mut stream, &frame);
                    }
                }
            }
        });
        Self { home, service }
    }

    fn vapor(&self, args: &[&str]) -> Output {
        vapor(self.home.path(), args)
    }

    fn asked(&self) -> Vec<String> {
        self.service.asked.lock().expect("asked").clone()
    }
}

fn vapor(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vapor"))
        .args(args)
        .env("VAPOR_DIR", home)
        .env("VAPOR_ENV", "dev")
        .env("HOME", home)
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
fn every_ipc_command_asks_the_daemon_and_prints_its_answer() {
    let daemon = StandIn::start(Behaviour::Answers);

    let status = daemon.vapor(&["status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let document = json(&status);
    assert_eq!(document["daemon"], "vapord/stand-in");
    assert_eq!(document["run"]["state"], "running");
    assert_eq!(document["queued"], 3);
    assert_eq!(document["profiles"][1]["id"], "work");
    let text = daemon.vapor(&["status"]);
    assert!(text.status.success());
    assert!(stdout(&text).contains("running"), "{}", stdout(&text));

    // The three controls: for every profile, and for one by name.
    for (command, profile) in [
        ("pause", None),
        ("resume", Some("work")),
        ("reconcile", Some("home")),
    ] {
        let mut args = vec![command, "--json"];
        if let Some(profile) = profile {
            args.extend(["--profile", profile]);
        }
        let output = daemon.vapor(&args);
        assert!(output.status.success(), "{command}: {}", stderr(&output));
        let ack = json(&output);
        assert_eq!(ack["accepted"], true, "{command}: {ack}");
        assert_eq!(ack["note"], format!("{command} taken"));
    }

    let diagnostics = daemon.vapor(&["diagnostics", "--json"]);
    assert!(diagnostics.status.success(), "{}", stderr(&diagnostics));
    assert_eq!(json(&diagnostics)["droppedEvents"], 4);
    let timeline = daemon.vapor(&["timeline", "--json"]);
    assert!(timeline.status.success(), "{}", stderr(&timeline));
    assert_eq!(
        json(&timeline)["entries"][0]["message"],
        "profile composed from the new configuration"
    );

    assert_eq!(
        daemon.asked(),
        [
            "status",
            "status",
            "pause *",
            "resume work",
            "reconcile home",
            "diagnostics",
            "timeline",
        ],
        "each command is one request, with the profile it names"
    );
}

#[test]
fn a_request_that_the_daemon_refuses_exits_one_with_the_answer_on_stdout() {
    let daemon = StandIn::start(Behaviour::Answers);
    let refused = daemon.vapor(&["pause", "--profile", "nope", "--json"]);
    assert_eq!(refused.status.code(), Some(1));
    let ack = json(&refused);
    assert_eq!(ack["accepted"], false);
    assert_eq!(ack["note"], "no profile 'nope'");
}

#[test]
fn a_daemon_that_gives_no_answer_fails_the_command_and_never_hangs_it() {
    let daemon = StandIn::start(Behaviour::Silent);
    // One command: every IPC command connects and calls the same way,
    // and each such call waits out its deadline of 3 seconds.
    let started = Instant::now();
    let output = daemon.vapor(&["status", "--json"]);
    let took = started.elapsed();
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).starts_with("vapor: "),
        "{}",
        stderr(&output)
    );
    // The bound only says that the command ends.
    assert!(took < Duration::from_secs(30), "status took {took:?}");
}

#[test]
fn a_daemon_of_another_protocol_version_is_named_as_such() {
    let daemon = StandIn::start(Behaviour::AnotherVersion);
    let output = daemon.vapor(&["status", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    let message = stderr(&output);
    assert!(
        message.contains("version"),
        "the refusal names the version skew: {message}"
    );
}

#[test]
fn with_no_daemon_every_ipc_command_fails_at_once() {
    let home = tempfile::Builder::new()
        .prefix("vp")
        .tempdir()
        .expect("home");
    for command in [
        "status",
        "pause",
        "resume",
        "reconcile",
        "diagnostics",
        "timeline",
    ] {
        let started = Instant::now();
        let output = vapor(home.path(), &[command]);
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(stdout(&output).is_empty(), "{command}");
        assert!(
            stderr(&output).contains("daemon"),
            "{command}: {}",
            stderr(&output)
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{command} waited for a daemon that is not there"
        );
    }
}
