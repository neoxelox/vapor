//! One running guest: a clone that the harness started, runs commands
//! in, and deletes at the end. Nothing here touches the host beyond
//! Tart's own store and the two directories the guest shares.

use std::path::{Path, PathBuf};
use std::process::{Child, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vapor_e2e::wait::{wait_for, wait_until};
use vapor_e2e::{Failure, ensure};

use crate::tart::{self, Share};

/// Where a macOS guest mounts the directories that the host shares.
pub const GUEST_SHARES: &str = "/Volumes/My Shared Files";
/// The read-only share with the installer and the driver.
pub const STAGE_SHARE: &str = "stage";
/// The share a run writes its evidence to.
pub const RESULTS_SHARE: &str = "results";

/// `tart exec` gives a command a short `PATH`; scripts get this one.
const GUEST_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// A boot takes about 25 s and a resume about 10 s on an idle host.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// How long one command in a guest gets unless the caller says more.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one probe of a guest that is coming up gets.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

pub fn guest_stage() -> String {
    format!("{GUEST_SHARES}/{STAGE_SHARE}")
}

pub fn guest_results() -> String {
    format!("{GUEST_SHARES}/{RESULTS_SHARE}")
}

/// Who a guest is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Session {
    /// A guest of a run: it has no window, and it ends with the run.
    Run,
    /// A guest for a person. It has no time limit: it stays up until
    /// someone stops it, also after the harness and the process that
    /// started the harness are gone. With `window`, Tart shows its
    /// screen on the host. With `internet`, it has a route to the
    /// Internet, to test a real cloud provider.
    Manual { window: bool, internet: bool },
}

pub struct Guest {
    name: String,
    child: Option<Child>,
    stage: PathBuf,
    results: PathBuf,
    session: Session,
}

impl Guest {
    /// Clones `source` as `name`, starts the clone, and waits until it
    /// answers commands with both shares mounted and its clock set.
    /// `stage` is shared read-only and `results` read-write.
    pub fn start(
        source: &str,
        name: &str,
        stage: &Path,
        results: &Path,
        session: Session,
    ) -> Result<Self, Failure> {
        tart::clone(source, name)?;
        let mut guest = Self {
            name: name.to_string(),
            child: None,
            stage: stage.to_path_buf(),
            results: results.to_path_buf(),
            session,
        };
        if let Err(failure) = guest.boot() {
            // A clone that never came up must not stay behind.
            let _ = guest.destroy();
            return Err(failure);
        }
        Ok(guest)
    }

    /// Takes over a VM that exists and does not run, without a clone.
    pub fn adopt(name: &str, stage: &Path, results: &Path) -> Result<Self, Failure> {
        let mut guest = Self {
            name: name.to_string(),
            child: None,
            stage: stage.to_path_buf(),
            results: results.to_path_buf(),
            session: Session::Run,
        };
        if let Err(failure) = guest.boot() {
            let _ = guest.destroy();
            return Err(failure);
        }
        Ok(guest)
    }

    fn boot(&mut self) -> Result<(), Failure> {
        let log = self.results.join(format!("{}.tart.log", self.name));
        let shares = [
            Share {
                name: STAGE_SHARE,
                path: &self.stage,
                read_only: true,
            },
            Share {
                name: RESULTS_SHARE,
                path: &self.results,
                read_only: false,
            },
        ];
        let (window, internet, detached) = match self.session {
            Session::Run => (false, false, false),
            Session::Manual { window, internet } => (window, internet, true),
        };
        self.child = Some(tart::start(
            &self.name, &shares, &log, window, internet, detached,
        )?);
        self.wait_ready()?;
        self.remount_shares()?;
        self.set_clock()
    }

    /// A guest that resumes from a saved state keeps what it knew of
    /// the shared directories when it was saved: a file that the host
    /// replaced since then can be missing in the guest. A new mount
    /// reads the directories as they are now.
    fn remount_shares(&self) -> Result<(), Failure> {
        self.sh_ok(&format!(
            "sudo umount -f '{GUEST_SHARES}' 2>/dev/null; sudo mkdir -p '{GUEST_SHARES}' \
             && sudo mount_virtiofs com.apple.virtio-fs.automount '{GUEST_SHARES}' \
             && ls '{}' '{}' >/dev/null",
            guest_stage(),
            guest_results()
        ))
        .map(drop)
    }

    fn wait_ready(&mut self) -> Result<(), Failure> {
        let name = self.name.clone();
        let stage = guest_stage();
        let results = guest_results();
        let child = &mut self.child;
        let mut exited = None;
        let ready = wait_until(
            READY_TIMEOUT,
            "the guest to answer commands with its shares mounted",
            || {
                if let Some(child) = child.as_mut()
                    && let Ok(Some(status)) = child.try_wait()
                {
                    exited = Some(status);
                    return true;
                }
                // The login session and both shares are up when the
                // Dock runs and both directories list.
                tart::exec(
                    &name,
                    &[
                        "/bin/sh",
                        "-c",
                        &format!("/usr/bin/pgrep -x Dock >/dev/null && /bin/ls '{stage}' '{results}' >/dev/null"),
                    ],
                    PROBE_TIMEOUT,
                )
                .is_ok_and(|output| output.status.success())
            },
        );
        if let Some(status) = exited {
            return Err(Failure::new(format!(
                "tart stopped before the guest `{name}` came up ({status}); see {}.tart.log",
                self.results.join(&name).display()
            )));
        }
        ready
    }

    /// A guest that was suspended resumes with the clock of the moment
    /// it was suspended, and a guest of a run has no network to correct
    /// it.
    fn set_clock(&self) -> Result<(), Failure> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Failure::new(format!("the host clock is before 1970: {error}")))?
            .as_secs();
        self.sh_ok(&format!("sudo /bin/date -f %s {now} >/dev/null"))
            .map(drop)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The host directory the guest writes its evidence to.
    pub fn results(&self) -> &Path {
        &self.results
    }

    /// Runs `script` with `/bin/sh` in the guest, as the logged-in user.
    pub fn sh(&self, script: &str) -> Result<Output, Failure> {
        self.sh_within(script, COMMAND_TIMEOUT)
    }

    /// Like [`Guest::sh`], for a script that needs more or less time
    /// than a command gets by default.
    pub fn sh_within(&self, script: &str, timeout: Duration) -> Result<Output, Failure> {
        let script = format!("export PATH={GUEST_PATH}; {script}");
        tart::exec(&self.name, &["/bin/sh", "-c", &script], timeout)
    }

    /// Like [`Guest::sh`], but a non-zero exit is a failure that quotes
    /// the script's output. Returns standard output.
    pub fn sh_ok(&self, script: &str) -> Result<String, Failure> {
        let output = self.sh(script)?;
        ensure!(
            output.status.success(),
            "in the guest, `{script}` ended with {}: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Whether `script` exits with zero in the guest.
    pub fn succeeds(&self, script: &str) -> bool {
        self.sh(script).is_ok_and(|output| output.status.success())
    }

    /// Restarts the guest's operating system and waits for the login
    /// session of the new boot.
    pub fn reboot(&mut self) -> Result<(), Failure> {
        let boot_time = |guest: &Self| guest.sh_ok("sysctl -n kern.boottime").ok();
        let before = boot_time(self)
            .ok_or_else(|| Failure::new("cannot read the boot time of the guest"))?;
        // The command ends with the connection, so its result says nothing.
        let _ = self.sh("sudo shutdown -r now");
        wait_for(READY_TIMEOUT, "the guest to start a new boot", || {
            boot_time(self).filter(|after| *after != before)
        })?;
        self.wait_ready()?;
        self.set_clock()
    }

    /// Saves the running guest to disk and ends its `tart run`.
    pub fn suspend(mut self) -> Result<(), Failure> {
        tart::suspend(&self.name)?;
        self.wait_child(SHUTDOWN_TIMEOUT, "tart to save the guest")
    }

    /// Stops the guest and deletes its clone.
    pub fn destroy(mut self) -> Result<(), Failure> {
        self.stop_now();
        tart::delete(&self.name)
    }

    /// Leaves the guest running after the harness exits.
    pub fn detach(mut self) {
        self.child = None;
    }

    fn stop_now(&mut self) {
        let _ = tart::stop(&self.name);
        if self.wait_child(SHUTDOWN_TIMEOUT, "tart to stop").is_err()
            && let Some(child) = self.child.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
    }

    fn wait_child(&mut self, timeout: Duration, what: &str) -> Result<(), Failure> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        wait_until(timeout, what, || matches!(child.try_wait(), Ok(Some(_))))?;
        self.child = None;
        Ok(())
    }
}

impl Drop for Guest {
    /// A guest that a panic or an early return left behind is stopped
    /// and its clone deleted, so a failure on any path leaves no VM.
    fn drop(&mut self) {
        if self.child.is_some() {
            self.stop_now();
            let _ = tart::delete(&self.name);
        }
    }
}
