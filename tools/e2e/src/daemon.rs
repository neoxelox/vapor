//! A daemon process the harness started: `vapor run` (the daemon linked
//! into the CLI) or the shipped `vapord` binary. Owns the child handle,
//! its stdout/stderr capture file, and the signals a scenario may send.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::Failure;
use crate::sandbox::Home;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonKind {
    /// `vapor run --foreground`: the daemon composed inside the CLI.
    CliRun,
    /// The `vapord` binary users get in `Vapor.app`.
    Vapord,
}

#[derive(Debug)]
pub struct Daemon {
    pub label: String,
    pub kind: DaemonKind,
    pub home: Home,
    pub output_path: PathBuf,
    child: Child,
    exit: Option<ExitStatus>,
}

impl Daemon {
    /// Spawns the daemon for `home`. stdout and stderr go to
    /// `<sandbox>/<label>-daemon.out` so a daemon that dies before its
    /// structured logger starts still leaves a trace.
    pub fn spawn(
        kind: DaemonKind,
        cli_bin: &Path,
        vapord_bin: &Path,
        home: &Home,
        output_dir: &Path,
    ) -> Result<Self, Failure> {
        let output_path = output_dir.join(format!("{}-daemon.out", home.label));
        let stdout = File::options()
            .create(true)
            .append(true)
            .open(&output_path)?;
        let stderr = stdout.try_clone()?;
        let mut command = match kind {
            DaemonKind::CliRun => {
                let mut command = Command::new(cli_bin);
                command.arg("run").arg("--foreground");
                command
            }
            DaemonKind::Vapord => Command::new(vapord_bin),
        };
        home.apply_env(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = command.spawn().map_err(|error| {
            Failure::new(format!(
                "could not spawn {} for home {}: {error}",
                match kind {
                    DaemonKind::CliRun => "vapor run",
                    DaemonKind::Vapord => "vapord",
                },
                home.label
            ))
        })?;
        Ok(Self {
            label: home.label.clone(),
            kind,
            home: home.clone(),
            output_path,
            child,
            exit: None,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `true` while the process has not exited.
    pub fn is_alive(&mut self) -> bool {
        if self.exit.is_some() {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit = Some(status);
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.is_alive();
        self.exit
    }

    /// Waits up to `timeout` for the process to exit on its own.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_alive() {
                return self.exit;
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Clean shutdown: SIGTERM, then wait `grace`. Returns the exit
    /// status; fails when the daemon ignored the signal (it is then
    /// SIGKILLed so the sandbox never leaks a process).
    pub fn terminate(&mut self, grace: Duration) -> Result<ExitStatus, Failure> {
        if !self.is_alive() {
            return self.exit.ok_or_else(|| Failure::new("daemon already gone"));
        }
        self.signal(Signal::Term)?;
        match self.wait_exit(grace) {
            Some(status) => Ok(status),
            None => {
                let _ = self.kill();
                Err(Failure::new(format!(
                    "daemon {} (pid {}) did not exit within {}s of SIGTERM; killed",
                    self.label,
                    self.pid(),
                    grace.as_secs()
                )))
            }
        }
    }

    /// Simulated crash: SIGKILL and reap.
    pub fn kill(&mut self) -> Result<ExitStatus, Failure> {
        if !self.is_alive() {
            return self.exit.ok_or_else(|| Failure::new("daemon already gone"));
        }
        self.child.kill()?;
        let status = self.child.wait()?;
        self.exit = Some(status);
        Ok(status)
    }

    /// Sends a Unix signal. Fails on hosts without signals.
    pub fn signal(&mut self, signal: Signal) -> Result<(), Failure> {
        #[cfg(unix)]
        {
            let number = match signal {
                Signal::Term => libc::SIGTERM,
                Signal::Stop => libc::SIGSTOP,
                Signal::Cont => libc::SIGCONT,
            };
            // SAFETY: `kill` with a pid we spawned and still own; a
            // reaped pid is guarded by `is_alive` above so we never
            // signal a recycled pid.
            let result = unsafe { libc::kill(self.child.id() as libc::pid_t, number) };
            if result != 0 {
                return Err(Failure::new(format!(
                    "kill({}, {signal:?}) failed: {}",
                    self.child.id(),
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = signal;
            Err(Failure::new(
                "Unix signals are not available on this host; use kill()",
            ))
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A scenario that returned early must never leak a daemon
        // holding the sandbox open.
        if self.is_alive() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
    Term,
    Stop,
    Cont,
}
