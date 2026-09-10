//! Linux `ServiceInstaller`: a systemd user unit driven through
//! `systemctl --user`. The unit lives at
//! `$XDG_CONFIG_HOME/systemd/user/<label>.service` (default
//! `~/.config/systemd/user/`), which is the per-user equivalent of the
//! macOS LaunchAgent: it starts at login (`default.target`) and stops
//! at logout unless the user enables lingering. The daemon's unit is
//! never restarted by systemd itself (`Restart=no`); the crash-loop
//! guard owns restarts, and the headless supervisor's unit is the one
//! kept alive (`Restart=always`), the same split as launchd's
//! `KeepAlive`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{ServiceDescriptor, ServiceInstallError, ServiceInstaller, ServiceStatus};

#[derive(Debug)]
pub struct NativeServiceInstaller {
    descriptor: ServiceDescriptor,
    unit_path: PathBuf,
}

impl NativeServiceInstaller {
    pub fn for_current_user(descriptor: ServiceDescriptor) -> Result<Self, ServiceInstallError> {
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| vapor_shared::runtime_paths::home_directory().map(|home| home.join(".config")))
            .ok_or_else(|| {
                ServiceInstallError::Backend(Box::new(std::io::Error::other(
                    "neither XDG_CONFIG_HOME nor HOME is set; cannot resolve the systemd user unit path",
                )))
            })?;
        let unit_path = config_home
            .join("systemd/user")
            .join(format!("{}.service", descriptor.label));
        Ok(Self {
            descriptor,
            unit_path,
        })
    }

    /// Construct with an explicit unit path (tests).
    pub fn with_unit_path(descriptor: ServiceDescriptor, unit_path: PathBuf) -> Self {
        Self {
            descriptor,
            unit_path,
        }
    }

    pub fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    pub fn unit_path(&self) -> &Path {
        &self.unit_path
    }

    fn unit_name(&self) -> String {
        format!("{}.service", self.descriptor.label)
    }

    /// The unit file. Paths and arguments are quoted the systemd way
    /// (double quotes, backslash escapes) so a space in the runtime
    /// directory survives.
    pub fn unit_contents(&self) -> String {
        let mut exec = quote(&self.descriptor.executable_path.display().to_string());
        for argument in &self.descriptor.arguments {
            exec.push(' ');
            exec.push_str(&quote(argument));
        }
        let mut unit = String::new();
        unit.push_str("[Unit]\n");
        unit.push_str(&format!(
            "Description=Vapor sync ({})\n",
            self.descriptor.label
        ));
        unit.push_str("After=default.target\n\n");
        unit.push_str("[Service]\n");
        unit.push_str("Type=simple\n");
        unit.push_str(&format!("ExecStart={exec}\n"));
        unit.push_str(if self.descriptor.keep_alive {
            "Restart=always\nRestartSec=5\n"
        } else {
            "Restart=no\n"
        });
        unit.push_str("KillSignal=SIGTERM\n");
        unit.push_str("TimeoutStopSec=30\n");
        for (key, value) in &self.descriptor.environment {
            unit.push_str(&format!(
                "Environment={}\n",
                quote(&format!("{key}={value}"))
            ));
        }
        if let Some(path) = &self.descriptor.stdout_path {
            unit.push_str(&format!("StandardOutput=append:{}\n", path.display()));
        }
        if let Some(path) = &self.descriptor.stderr_path {
            unit.push_str(&format!("StandardError=append:{}\n", path.display()));
        }
        unit.push_str("\n[Install]\nWantedBy=default.target\n");
        unit
    }

    fn write_unit(&self) -> Result<(), ServiceInstallError> {
        let executable = &self.descriptor.executable_path;
        if !executable.is_file() {
            return Err(ServiceInstallError::InvalidExecutable {
                path: executable.clone(),
                reason: "not a file".to_string(),
            });
        }
        if let Some(parent) = self.unit_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        }
        for path in [&self.descriptor.stdout_path, &self.descriptor.stderr_path]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let mut file = fs::File::create(&self.unit_path)
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        file.write_all(self.unit_contents().as_bytes())
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))?;
        Ok(())
    }

    fn systemctl(&self, args: &[&str]) -> Result<std::process::Output, ServiceInstallError> {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|error| ServiceInstallError::Backend(Box::new(error)))
    }

    fn systemctl_ok(&self, args: &[&str]) -> Result<(), ServiceInstallError> {
        let output = self.systemctl(args)?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(ServiceInstallError::Backend(Box::new(
            std::io::Error::other(format!(
                "systemctl --user {args:?} exited with {}: {stderr}",
                output.status
            )),
        )))
    }
}

/// systemd quoting: double quotes with `\\` and `"` escaped.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

impl ServiceInstaller for NativeServiceInstaller {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError> {
        self.write_unit()?;
        self.systemctl_ok(&["daemon-reload"])?;
        self.systemctl_ok(&["enable", &self.unit_name()])?;
        self.systemctl_ok(&["start", &self.unit_name()])
    }

    fn disable_and_uninstall(&self) -> Result<(), ServiceInstallError> {
        let unit = self.unit_name();
        // Best effort on the way out: a unit that was never loaded
        // makes these fail, and the file removal is what matters.
        let _ = self.systemctl(&["stop", &unit]);
        let _ = self.systemctl(&["disable", &unit]);
        match fs::remove_file(&self.unit_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ServiceInstallError::Backend(Box::new(error))),
        }
        let _ = self.systemctl(&["daemon-reload"]);
        Ok(())
    }

    fn start_daemon(&self) -> Result<(), ServiceInstallError> {
        self.systemctl_ok(&["start", &self.unit_name()])
    }

    fn stop_daemon(&self) -> Result<(), ServiceInstallError> {
        self.systemctl_ok(&["stop", &self.unit_name()])
    }

    fn status(&self) -> Result<ServiceStatus, ServiceInstallError> {
        if !self.unit_path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        let output = self.systemctl(&["is-active", &self.unit_name()])?;
        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(match state.as_str() {
            "active" | "activating" | "reloading" => ServiceStatus::Running,
            _ => ServiceStatus::Stopped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(keep_alive: bool) -> ServiceDescriptor {
        ServiceDescriptor {
            label: "sh.arn.vapor.daemon".to_string(),
            executable_path: PathBuf::from("/opt/vapor/bin/vapord"),
            arguments: vec!["--flag".to_string(), "a \"quoted\" value".to_string()],
            environment: vec![("VAPOR_DIR".to_string(), "/home/alex/my vapor".to_string())],
            stdout_path: Some(PathBuf::from("/home/alex/.vapor/logs/vapord.stdout.log")),
            stderr_path: Some(PathBuf::from("/home/alex/.vapor/logs/vapord.stderr.log")),
            keep_alive,
        }
    }

    #[test]
    fn the_unit_file_says_what_the_descriptor_says() {
        let installer = NativeServiceInstaller::with_unit_path(
            descriptor(false),
            PathBuf::from("/tmp/x.service"),
        );
        let unit = installer.unit_contents();
        assert!(unit.contains("[Unit]\nDescription=Vapor sync (sh.arn.vapor.daemon)\n"));
        assert!(unit.contains(
            "ExecStart=\"/opt/vapor/bin/vapord\" \"--flag\" \"a \\\"quoted\\\" value\"\n"
        ));
        assert!(unit.contains("Restart=no\n"));
        assert!(unit.contains("Environment=\"VAPOR_DIR=/home/alex/my vapor\"\n"));
        assert!(unit.contains("StandardOutput=append:/home/alex/.vapor/logs/vapord.stdout.log\n"));
        assert!(unit.contains("WantedBy=default.target\n"));
        assert!(!unit.contains("Restart=always"));
    }

    #[test]
    fn only_the_supervisor_is_kept_alive() {
        let installer = NativeServiceInstaller::with_unit_path(
            descriptor(true),
            PathBuf::from("/tmp/x.service"),
        );
        assert!(
            installer
                .unit_contents()
                .contains("Restart=always\nRestartSec=5\n")
        );
    }

    #[test]
    fn the_unit_path_follows_xdg_config_home_and_the_label() {
        let installer = NativeServiceInstaller::for_current_user(descriptor(false)).expect("path");
        assert!(
            installer
                .unit_path()
                .ends_with("systemd/user/sh.arn.vapor.daemon.service"),
            "{}",
            installer.unit_path().display()
        );
    }

    #[test]
    fn status_is_not_installed_without_a_unit_file() {
        let temp = tempfile::TempDir::new().expect("temp");
        let installer = NativeServiceInstaller::with_unit_path(
            descriptor(false),
            temp.path().join("missing.service"),
        );
        assert_eq!(
            installer.status().expect("status"),
            ServiceStatus::NotInstalled
        );
    }
}
