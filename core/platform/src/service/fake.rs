//! In-memory `ServiceInstaller` for tests.

use std::sync::Mutex;

use super::{ServiceDescriptor, ServiceInstallError, ServiceInstaller, ServiceStatus};

/// `ServiceInstaller` that records every operation in memory. Tests can
/// inspect the recorded sequence to assert lifecycle behavior without
/// touching the real launchctl / systemctl / schtasks command surface.
#[derive(Debug)]
pub struct InMemoryServiceInstaller {
    descriptor: ServiceDescriptor,
    inner: Mutex<InnerState>,
}

#[derive(Debug)]
struct InnerState {
    operations: Vec<&'static str>,
    status: ServiceStatus,
}

impl InMemoryServiceInstaller {
    pub fn new(descriptor: ServiceDescriptor) -> Self {
        Self {
            descriptor,
            inner: Mutex::new(InnerState {
                operations: Vec::new(),
                status: ServiceStatus::NotInstalled,
            }),
        }
    }

    pub fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    pub fn operations(&self) -> Vec<&'static str> {
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .operations
            .clone()
    }

    pub fn set_status_for_testing(&self, status: ServiceStatus) {
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status = status;
    }

    fn record(&self, op: &'static str) {
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .operations
            .push(op);
    }
}

impl ServiceInstaller for InMemoryServiceInstaller {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError> {
        self.record("install");
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status = ServiceStatus::Stopped;
        Ok(())
    }

    fn disable_and_uninstall(&self) -> Result<(), ServiceInstallError> {
        self.record("uninstall");
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status = ServiceStatus::NotInstalled;
        Ok(())
    }

    fn start_daemon(&self) -> Result<(), ServiceInstallError> {
        self.record("start");
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status = ServiceStatus::Running;
        Ok(())
    }

    fn stop_daemon(&self) -> Result<(), ServiceInstallError> {
        self.record("stop");
        self.inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status = ServiceStatus::Stopped;
        Ok(())
    }

    fn status(&self) -> Result<ServiceStatus, ServiceInstallError> {
        Ok(self
            .inner
            .lock()
            .expect("InMemoryServiceInstaller mutex poisoned")
            .status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn descriptor() -> ServiceDescriptor {
        ServiceDescriptor {
            label: "sh.arn.vapor.test".to_string(),
            executable_path: PathBuf::from("/usr/bin/false"),
            arguments: vec![],
            environment: vec![],
            stdout_path: None,
            stderr_path: None,
        }
    }

    #[test]
    fn install_then_start_then_stop_records_full_lifecycle_sequence() {
        let installer = InMemoryServiceInstaller::new(descriptor());
        installer.install_and_enable().expect("install");
        installer.start_daemon().expect("start");
        installer.stop_daemon().expect("stop");
        installer.disable_and_uninstall().expect("uninstall");
        assert_eq!(
            installer.operations(),
            vec!["install", "start", "stop", "uninstall"]
        );
        assert_eq!(
            installer.status().expect("status"),
            ServiceStatus::NotInstalled
        );
    }

    #[test]
    fn fake_status_can_be_overridden_for_simulating_crash_loop_pause() {
        let installer = InMemoryServiceInstaller::new(descriptor());
        installer.install_and_enable().expect("install");
        installer.set_status_for_testing(ServiceStatus::CrashLoopPaused);
        assert_eq!(
            installer.status().expect("status"),
            ServiceStatus::CrashLoopPaused
        );
    }
}
