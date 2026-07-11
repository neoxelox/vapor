//! Linux `ServiceInstaller` stub.

use super::{ServiceDescriptor, ServiceInstallError, ServiceInstaller, ServiceStatus};

#[derive(Debug)]
pub struct NativeServiceInstaller {
    descriptor: ServiceDescriptor,
}

impl NativeServiceInstaller {
    pub fn for_current_user(descriptor: ServiceDescriptor) -> Result<Self, ServiceInstallError> {
        Ok(Self { descriptor })
    }

    pub fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }
}

impl ServiceInstaller for NativeServiceInstaller {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError> {
        Err(ServiceInstallError::Unsupported(
            "Linux ServiceInstaller is not implemented yet",
        ))
    }
    fn disable_and_uninstall(&self) -> Result<(), ServiceInstallError> {
        Err(ServiceInstallError::Unsupported(
            "Linux ServiceInstaller is not implemented yet",
        ))
    }
    fn start_daemon(&self) -> Result<(), ServiceInstallError> {
        Err(ServiceInstallError::Unsupported(
            "Linux ServiceInstaller is not implemented yet",
        ))
    }
    fn stop_daemon(&self) -> Result<(), ServiceInstallError> {
        Err(ServiceInstallError::Unsupported(
            "Linux ServiceInstaller is not implemented yet",
        ))
    }
    fn status(&self) -> Result<ServiceStatus, ServiceInstallError> {
        Ok(ServiceStatus::NotInstalled)
    }
}
