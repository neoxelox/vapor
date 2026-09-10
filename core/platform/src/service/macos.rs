//! macOS `ServiceInstaller` implementation.
//!
//! Ports the LaunchAgent installer logic from
//! `apps/macos/Sources/VaporCore/LaunchAgentController.swift` into Rust.
//! Uses `launchctl` via `std::process::Command` per the policy in
//! `docs/operations/macos/launchagent-policy.md`. Plist serialization is
//! emitted as a known-good XML template (the schema is fixed, so a hand-
//! rolled writer keeps the dep set tiny).
//!
//! Consumed by `core/lifecycle::DaemonLifecycleManager`; the macOS
//! Swift app delegates to it through the bundled `vapor` CLI, so this is
//! the single writer of the LaunchAgent definition.

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

    fn write_plist(&self) -> Result<(), ServiceInstallError> {
        if let Some(parent) = self.plist_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        }

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

        let mut file = fs::File::create(&self.plist_path)
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        file.write_all(buffer.as_bytes())
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        Ok(())
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

impl ServiceInstaller for NativeServiceInstaller {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError> {
        self.validate_executable()?;
        self.write_plist()?;
        // bootout is best-effort: the service may not be loaded yet.
        let _ = self.run_launchctl(&[
            "bootout",
            &self.domain_target(),
            &self.plist_path.display().to_string(),
        ]);
        self.run_launchctl(&["enable", &self.service_target()])?;
        self.run_launchctl(&[
            "bootstrap",
            &self.domain_target(),
            &self.plist_path.display().to_string(),
        ])
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
        self.run_launchctl(&["kickstart", "-k", &self.service_target()])
    }

    fn stop_daemon(&self) -> Result<(), ServiceInstallError> {
        // SIGTERM via launchctl. A kill failure is only benign if the
        // service is in fact no longer running (already stopped, never
        // loaded); if it still reports Running — or the state cannot be
        // confirmed — the failure is real and must surface rather than be
        // reported to the caller (and the user's Quit flow) as a stop.
        match self.run_launchctl(&["kill", "TERM", &self.service_target()]) {
            Ok(()) => Ok(()),
            Err(kill_error) => match self.status() {
                Ok(ServiceStatus::Running) => Err(kill_error),
                Ok(_) => Ok(()),
                Err(_) => Err(kill_error),
            },
        }
    }

    fn status(&self) -> Result<ServiceStatus, ServiceInstallError> {
        if !self.plist_path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        let output = Command::new("/bin/launchctl")
            .args(["print", &self.service_target()])
            .output()
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        if !output.status.success() {
            return Ok(ServiceStatus::Stopped);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("state = running") || stdout.contains("pid = ") {
            Ok(ServiceStatus::Running)
        } else {
            Ok(ServiceStatus::Stopped)
        }
    }
}

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
        installer.write_plist().expect("write plist");
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

        installer.write_plist().expect("write plist");
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
