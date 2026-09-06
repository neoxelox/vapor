//! Runs the `vapor` CLI bound to one sandbox home and parses its
//! `--json` output with the same types the CLI serializes, so a shape
//! change fails loudly here instead of being grepped around.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use vapor_ipc::StatusResponse;

use crate::Failure;
use crate::sandbox::Home;
use crate::wait;

#[derive(Clone, Debug)]
pub struct CliOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

#[derive(Clone, Debug)]
pub struct Cli {
    pub bin: PathBuf,
    pub home: Home,
}

impl Cli {
    pub fn new(bin: &Path, home: &Home) -> Self {
        Self {
            bin: bin.to_path_buf(),
            home: home.clone(),
        }
    }

    /// Runs `vapor <args>` and captures everything. Never fails on a
    /// non-zero exit; callers decide what an exit code means.
    pub fn run(&self, args: &[&str]) -> Result<CliOutput, Failure> {
        let mut command = Command::new(&self.bin);
        command.args(args);
        self.home.apply_env(&mut command);
        let output = command.output().map_err(|error| {
            Failure::new(format!("could not run vapor {}: {error}", args.join(" ")))
        })?;
        Ok(CliOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Runs `vapor <args>` and returns stdout; a non-zero exit is a
    /// failure that quotes both streams.
    pub fn ok(&self, args: &[&str]) -> Result<String, Failure> {
        let output = self.run(args)?;
        if !output.success() {
            return Err(Failure::new(format!(
                "vapor {} exited {:?}: {}",
                args.join(" "),
                output.code,
                output.combined().trim()
            )));
        }
        Ok(output.stdout)
    }

    /// Runs a `--json` command and parses stdout.
    pub fn json(&self, args: &[&str]) -> Result<Value, Failure> {
        let stdout = self.ok(args)?;
        serde_json::from_str(&stdout).map_err(|error| {
            Failure::new(format!(
                "vapor {} did not print JSON ({error}): {}",
                args.join(" "),
                stdout.trim()
            ))
        })
    }

    pub fn config_set(&self, key: &str, value: &str) -> Result<String, Failure> {
        self.ok(&["config", "set", key, value])
    }

    pub fn config_get(&self, key: &str) -> Result<String, Failure> {
        Ok(self.ok(&["config", "get", key])?.trim().to_string())
    }

    /// `vapor status --json` typed, or `None` when the daemon does not
    /// answer (not started, gone, wedged).
    pub fn status(&self) -> Option<StatusResponse> {
        let output = self.run(&["status", "--json"]).ok()?;
        if !output.success() {
            return None;
        }
        serde_json::from_str(&output.stdout).ok()
    }

    pub fn run_state(&self) -> Option<String> {
        self.status().map(|status| status.run_state)
    }

    pub fn run_state_is(&self, expected: &str) -> bool {
        self.run_state().as_deref() == Some(expected)
    }

    /// Waits for the daemon behind this home to answer with the given
    /// run state.
    pub fn wait_run_state(&self, expected: &str, timeout: Duration) -> Result<(), Failure> {
        wait::wait_until(
            timeout,
            &format!(
                "daemon for home '{}' to report run_state={expected}",
                self.home.label
            ),
            || self.run_state_is(expected),
        )
    }

    /// A drain hint; allowed to fail benignly when it races a state flip.
    pub fn flush_now(&self) {
        let _ = self.run(&["flush-now"]);
    }

    pub fn reconcile(&self) -> Result<(), Failure> {
        self.ok(&["reconcile"]).map(|_| ())
    }

    pub fn pause(&self) -> Result<(), Failure> {
        self.ok(&["pause"]).map(|_| ())
    }

    pub fn resume(&self) -> Result<(), Failure> {
        self.ok(&["resume"]).map(|_| ())
    }
}
