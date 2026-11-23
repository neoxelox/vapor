//! macOS `ServiceInstaller` implementation.
//!
//! Drives `launchctl` through `std::process::Command` per the policy in
//! `docs/operations/macos/launchagent-policy.md`. The plist is written
//! from a fixed XML template (the schema is fixed, so a hand-rolled
//! writer keeps the dependency set small).
//!
//! Consumed by `core/lifecycle::DaemonLifecycleManager`; the macOS app
//! reaches it through the bundled `vapor` CLI, so this is the single
//! writer of the LaunchAgent definition.

use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{ServiceDescriptor, ServiceInstallError, ServiceInstaller, ServiceStatus};

/// macOS-native `ServiceInstaller` driving `launchctl` against a
/// LaunchAgent plist. The plist policy is fixed per
/// `docs/operations/macos/launchagent-policy.md`: the daemon's job is
/// never kept alive by launchd (the crash-loop guard owns restarts),
/// and only the headless supervisor's job is.
#[derive(Debug)]
pub struct NativeServiceInstaller {
    descriptor: ServiceDescriptor,
    plist_path: PathBuf,
    user_id: u32,
}

impl NativeServiceInstaller {
    /// Construct an installer using the per-user LaunchAgent path
    /// `~/Library/LaunchAgents/<label>.plist`.
    pub fn for_current_user(descriptor: ServiceDescriptor) -> Result<Self, ServiceInstallError> {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            ServiceInstallError::Backend(Box::new(std::io::Error::other(
                "HOME is not set; cannot resolve LaunchAgent plist path",
            )))
        })?;
        let plist_path = PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", descriptor.label));
        let user_id = current_uid();
        Ok(Self {
            descriptor,
            plist_path,
            user_id,
        })
    }

    pub fn with_paths(descriptor: ServiceDescriptor, plist_path: PathBuf, user_id: u32) -> Self {
        Self {
            descriptor,
            plist_path,
            user_id,
        }
    }

    pub fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    pub fn plist_path(&self) -> &Path {
        &self.plist_path
    }

    fn domain_target(&self) -> String {
        format!("gui/{}", self.user_id)
    }

    fn service_target(&self) -> String {
        format!("{}/{}", self.domain_target(), self.descriptor.label)
    }

    fn validate_executable(&self) -> Result<(), ServiceInstallError> {
        let path = &self.descriptor.executable_path;
        let metadata =
            fs::metadata(path).map_err(|error| ServiceInstallError::InvalidExecutable {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if !metadata.is_file() {
            return Err(ServiceInstallError::InvalidExecutable {
                path: path.clone(),
                reason: "not a regular file".to_string(),
            });
        }
        if metadata.mode() & 0o111 == 0 {
            return Err(ServiceInstallError::InvalidExecutable {
                path: path.clone(),
                reason: "not executable".to_string(),
            });
        }
        Ok(())
    }

    /// The LaunchAgent definition as XML.
    fn render_plist(&self) -> String {
        let mut buffer = String::new();
        buffer.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        buffer.push_str(
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        );
        buffer.push_str("<plist version=\"1.0\">\n<dict>\n");

        push_string_entry(&mut buffer, "Label", &self.descriptor.label);

        buffer.push_str("  <key>ProgramArguments</key>\n  <array>\n");
        push_string_array_item(
            &mut buffer,
            &self.descriptor.executable_path.display().to_string(),
        );
        for argument in &self.descriptor.arguments {
            push_string_array_item(&mut buffer, argument);
        }
        buffer.push_str("  </array>\n");

        push_bool_entry(&mut buffer, "RunAtLoad", true);
        push_bool_entry(&mut buffer, "KeepAlive", self.descriptor.keep_alive);
        push_string_entry(&mut buffer, "ProcessType", "Background");

        if !self.descriptor.environment.is_empty() {
            buffer.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
            for (key, value) in &self.descriptor.environment {
                buffer.push_str("    <key>");
                buffer.push_str(&xml_escape(key));
                buffer.push_str("</key>\n    <string>");
                buffer.push_str(&xml_escape(value));
                buffer.push_str("</string>\n");
            }
            buffer.push_str("  </dict>\n");
        }

        if let Some(stdout_path) = &self.descriptor.stdout_path {
            push_string_entry(
                &mut buffer,
                "StandardOutPath",
                &stdout_path.display().to_string(),
            );
        }
        if let Some(stderr_path) = &self.descriptor.stderr_path {
            push_string_entry(
                &mut buffer,
                "StandardErrorPath",
                &stderr_path.display().to_string(),
            );
        }

        buffer.push_str("</dict>\n</plist>\n");
        buffer
    }

    fn write_plist(&self, contents: &str) -> Result<(), ServiceInstallError> {
        if let Some(parent) = self.plist_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        }
        let mut file = fs::File::create(&self.plist_path)
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        file.write_all(contents.as_bytes())
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        Ok(())
    }

    /// Whether launchd knows the job. A plist on disk is not enough: a
    /// job that was booted out, or whose bootstrap failed, has its file
    /// and is not loaded.
    fn is_loaded(&self) -> bool {
        Command::new("/bin/launchctl")
            .args(["print", &self.service_target()])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn bootstrap(&self) -> Result<(), ServiceInstallError> {
        self.run_launchctl(&[
            "bootstrap",
            &self.domain_target(),
            &self.plist_path.display().to_string(),
        ])
    }

    /// Runs launchctl with captured stdio. Capturing matters twice
    /// over: best-effort invocations (bootout of a not-loaded service,
    /// kill of an already-stopped one) must not leak launchctl noise
    /// like `Could not find service …` onto the caller's terminal, and
    /// failed required invocations should carry launchctl's stderr in
    /// the returned error instead of only an exit code.
    fn run_launchctl(&self, args: &[&str]) -> Result<(), ServiceInstallError> {
        let output = Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            let detail = if stderr.is_empty() {
                format!("launchctl {args:?} exited with {}", output.status)
            } else {
                format!("launchctl {args:?} exited with {}: {stderr}", output.status)
            };
            return Err(ServiceInstallError::Backend(Box::new(
                std::io::Error::other(detail),
            )));
        }
        Ok(())
    }
}

impl NativeServiceInstaller {
    /// Whether the daemon's process is gone within `wait`.
    fn wait_until_stopped(&self, wait: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + wait;
        while std::time::Instant::now() < deadline {
            if !matches!(self.status(), Ok(ServiceStatus::Running)) {
                return true;
            }
            std::thread::sleep(STOP_POLL);
        }
        false
    }
}

impl ServiceInstaller for NativeServiceInstaller {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError> {
        self.validate_executable()?;
        let definition = self.render_plist();
        // The same definition, already loaded: nothing to install. The
        // boot-out and boot-in below would stop a healthy daemon and
        // let `RunAtLoad` start it again, at every app start.
        let unchanged =
            fs::read_to_string(&self.plist_path).is_ok_and(|current| current == definition);
        if unchanged && self.is_loaded() {
            return Ok(());
        }
        self.write_plist(&definition)?;
        // bootout is best-effort: the service may not be loaded yet.
        let _ = self.run_launchctl(&[
            "bootout",
            &self.domain_target(),
            &self.plist_path.display().to_string(),
        ]);
        self.run_launchctl(&["enable", &self.service_target()])?;
        self.bootstrap()
    }

    fn disable_and_uninstall(&self) -> Result<(), ServiceInstallError> {
        let _ = self.run_launchctl(&["disable", &self.service_target()]);
        let _ = self.run_launchctl(&[
            "bootout",
            &self.domain_target(),
            &self.plist_path.display().to_string(),
        ]);
        if self.plist_path.exists() {
            fs::remove_file(&self.plist_path)
                .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        }
        Ok(())
    }

    fn start_daemon(&self) -> Result<(), ServiceInstallError> {
        // `kickstart` cannot start a job that launchd does not know.
        // Loading it starts it too (`RunAtLoad`).
        if !self.is_loaded() {
            return self.bootstrap();
        }
        // No `-k`: that flag stops a running instance first, so a start
        // of a daemon that runs would be a restart.
        self.run_launchctl(&["kickstart", &self.service_target()])
    }

    fn stop_daemon(&self) -> Result<(), ServiceInstallError> {
        // SIGTERM via launchctl. A kill failure is only benign if the
        // service is in fact no longer running (already stopped, never
        // loaded); if it still reports Running, or the state cannot be
        // confirmed: the failure is real and must surface rather than be
        // reported to the caller (and the user's Quit flow) as a stop.
        if let Err(kill_error) = self.run_launchctl(&["kill", "TERM", &self.service_target()]) {
            return match self.status() {
                Ok(ServiceStatus::Running) => Err(kill_error),
                Ok(_) => Ok(()),
                Err(_) => Err(kill_error),
            };
        }
        // The signal is sent; the daemon is stopped when its process is
        // gone. A start that follows at once (a restart) must not find
        // the old instance still there and take it for the new one.
        if self.wait_until_stopped(STOP_WAIT) {
            return Ok(());
        }
        // The daemon did not go: a worker of it sits in a call that
        // does not return. A process left like that holds the job,
        // answers nobody, and syncs nothing, and every later start
        // would take it for a daemon that runs. Its state is durable,
        // so it is ended here.
        self.kill_daemon()?;
        if self.wait_until_stopped(KILL_WAIT) {
            return Ok(());
        }
        Err(ServiceInstallError::Backend(Box::new(
            std::io::Error::other(format!(
                "the daemon did not stop within {} s of SIGTERM, and not within {} s of SIGKILL;                  sync is down until its process is gone (`launchctl print {}` shows it)",
                STOP_WAIT.as_secs(),
                KILL_WAIT.as_secs(),
                self.service_target()
            )),
        )))
    }

    fn kill_daemon(&self) -> Result<(), ServiceInstallError> {
        self.run_launchctl(&["kill", "KILL", &self.service_target()])
    }

    fn status(&self) -> Result<ServiceStatus, ServiceInstallError> {
        if !self.plist_path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        // Loaded: the exit status of `launchctl print`. Running: a
        // process of launchd with the job's executable. The text that
        // `launchctl print` writes is for people, and Apple changes it
        // between releases, so nothing here reads it.
        if !self.is_loaded() {
            return Ok(ServiceStatus::Stopped);
        }
        let processes = Command::new("/bin/ps")
            .args(["-axww", "-o", "ppid=,comm="])
            .output()
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        // The executable of the definition that is on disk: it is the
        // one launchd started, also when this binary now lives at
        // another path (an app that was moved or updated).
        let executable = fs::read_to_string(&self.plist_path)
            .ok()
            .and_then(|definition| registered_executable(&definition))
            .unwrap_or_else(|| {
                self.descriptor
                    .executable_path
                    .to_string_lossy()
                    .into_owned()
            });
        if job_process_runs(&String::from_utf8_lossy(&processes.stdout), &executable) {
            Ok(ServiceStatus::Running)
        } else {
            Ok(ServiceStatus::Stopped)
        }
    }
}

/// The executable of a service definition that this installer wrote:
/// the first string of its `ProgramArguments`.
fn registered_executable(definition: &str) -> Option<String> {
    let arguments = definition.split_once("<key>ProgramArguments</key>")?.1;
    let first = arguments.split_once("<string>")?.1;
    let escaped = first.split_once("</string>")?.0;
    Some(
        escaped
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Whether `listing` (one `ppid command` line for each process) holds
/// a process that launchd started from `executable`. launchd is the
/// parent of every job it runs, and it starts a job by the path in the
/// job's definition, so the two together name the job's process. A
/// copy of the binary that a person started from a shell has the shell
/// as its parent.
fn job_process_runs(listing: &str, executable: &str) -> bool {
    listing.lines().any(|line| {
        let line = line.trim_start();
        let (parent, command) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        parent == "1" && command.trim_start() == executable
    })
}

/// How long a stop waits for the daemon's process to go, and how often
/// it looks. The daemon's own shutdown takes well under a second, and
/// it ends itself when a worker holds it longer than its own limit.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a kill waits for the process to go.
const KILL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(100);

fn current_uid() -> u32 {
    // SAFETY: `getuid` is documented as never failing and has no side
    // effects. The libc binding mirrors the POSIX signature one-to-one;
    // we are not forwarding any pointer / lifetime arguments. The trade-
    // off vs. shelling out to `id -u` is one fewer process spawn.
    #[allow(unsafe_code)]
    unsafe {
        libc::getuid()
    }
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn push_string_entry(buf: &mut String, key: &str, value: &str) {
    buf.push_str("  <key>");
    buf.push_str(&xml_escape(key));
    buf.push_str("</key>\n  <string>");
    buf.push_str(&xml_escape(value));
    buf.push_str("</string>\n");
}

fn push_bool_entry(buf: &mut String, key: &str, value: bool) {
    buf.push_str("  <key>");
    buf.push_str(&xml_escape(key));
    buf.push_str("</key>\n  ");
    buf.push_str(if value { "<true/>\n" } else { "<false/>\n" });
}

fn push_string_array_item(buf: &mut String, value: &str) {
    buf.push_str("    <string>");
    buf.push_str(&xml_escape(value));
    buf.push_str("</string>\n");
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_job_runs_when_launchd_has_a_process_of_its_executable() {
        let executable = "/Applications/Vapor.app/Contents/MacOS/vapord";
        let listing =
            "    1 /usr/sbin/cfprefsd\n  812 /Applications/Vapor.app/Contents/MacOS/vapord\n";
        assert!(
            !job_process_runs(listing, executable),
            "a copy that a shell started is not the job"
        );
        let listing = format!("{listing}    1 {executable}\n");
        assert!(job_process_runs(&listing, executable));
        // The definition on disk names the executable that runs.
        let definition = "<key>ProgramArguments</key>\n  <array>\n    \
                          <string>/Apps/R&amp;D/vapord</string>\n    <string>run</string>";
        assert_eq!(
            registered_executable(definition).as_deref(),
            Some("/Apps/R&D/vapord")
        );
        // A path with a space in it is one command.
        let spaced = "/Users/sam/My Apps/Vapor.app/Contents/MacOS/vapord";
        assert!(job_process_runs(&format!("    1 {spaced}\n"), spaced));
        assert!(!job_process_runs(
            &format!("    1 {spaced} extra\n"),
            spaced
        ));
    }
    use super::*;
    use tempfile::TempDir;

    fn make_executable(temp: &TempDir, name: &str) -> PathBuf {
        let path = temp.path().join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write executable");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("set executable bit");
        path
    }

    #[test]
    fn the_supervisor_job_is_the_only_one_launchd_keeps_alive() {
        let temp = TempDir::new().expect("temp dir");
        let executable = make_executable(&temp, "vapor");
        let plist_path = temp.path().join("sh.arn.vapor.supervisor.plist");
        let installer = NativeServiceInstaller::with_paths(
            ServiceDescriptor {
                label: "sh.arn.vapor.supervisor".to_string(),
                executable_path: executable,
                arguments: vec!["service".into(), "check".into(), "--loop".into()],
                environment: vec![],
                stdout_path: None,
                stderr_path: None,
                keep_alive: true,
            },
            plist_path.clone(),
            501,
        );
        installer
            .write_plist(&installer.render_plist())
            .expect("write plist");
        let contents = fs::read_to_string(&plist_path).expect("read plist");
        assert!(contents.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(contents.contains("<string>--loop</string>"));
    }

    #[test]
    fn write_plist_emits_policy_approved_keys_with_correct_values() {
        let temp = TempDir::new().expect("temp dir");
        let executable = make_executable(&temp, "vapord");
        let plist_path = temp.path().join("sh.arn.vapor.daemon.plist");
        let installer = NativeServiceInstaller::with_paths(
            ServiceDescriptor {
                label: "sh.arn.vapor.daemon".to_string(),
                executable_path: executable.clone(),
                arguments: vec![],
                environment: vec![("VAPOR_DIR".to_string(), "/tmp/.vapor".to_string())],
                stdout_path: Some(temp.path().join("vapord.stdout.log")),
                stderr_path: Some(temp.path().join("vapord.stderr.log")),
                keep_alive: false,
            },
            plist_path.clone(),
            501,
        );

        installer
            .write_plist(&installer.render_plist())
            .expect("write plist");
        let contents = fs::read_to_string(&plist_path).expect("read plist");

        assert!(contents.contains("<key>Label</key>"));
        assert!(contents.contains("<string>sh.arn.vapor.daemon</string>"));
        assert!(contents.contains("<key>ProgramArguments</key>"));
        assert!(contents.contains(&format!("<string>{}</string>", executable.display())));
        assert!(contents.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(contents.contains("<key>KeepAlive</key>\n  <false/>"));
        assert!(contents.contains("<key>ProcessType</key>\n  <string>Background</string>"));
        assert!(contents.contains("<key>EnvironmentVariables</key>"));
        assert!(contents.contains("<key>VAPOR_DIR</key>"));
        assert!(contents.contains("<key>StandardOutPath</key>"));
        assert!(contents.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn validate_executable_rejects_missing_binary() {
        let temp = TempDir::new().expect("temp dir");
        let installer = NativeServiceInstaller::with_paths(
            ServiceDescriptor {
                label: "sh.arn.vapor.daemon".to_string(),
                executable_path: temp.path().join("missing"),
                arguments: vec![],
                environment: vec![],
                stdout_path: None,
                stderr_path: None,
                keep_alive: false,
            },
            temp.path().join("plist"),
            501,
        );

        let error = installer.validate_executable().expect_err("missing binary");
        assert!(matches!(
            error,
            ServiceInstallError::InvalidExecutable { reason, .. } if reason.contains("No such")
                || reason.contains("not a regular")
        ));
    }

    #[test]
    fn validate_executable_rejects_non_executable_file() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("plain");
        fs::write(&path, b"x").expect("write file");

        let installer = NativeServiceInstaller::with_paths(
            ServiceDescriptor {
                label: "sh.arn.vapor.daemon".to_string(),
                executable_path: path,
                arguments: vec![],
                environment: vec![],
                stdout_path: None,
                stderr_path: None,
                keep_alive: false,
            },
            temp.path().join("plist"),
            501,
        );

        let error = installer.validate_executable().expect_err("not executable");
        assert!(matches!(
            error,
            ServiceInstallError::InvalidExecutable { reason, .. } if reason == "not executable"
        ));
    }
}
