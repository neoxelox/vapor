#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt::{self, Display};

use vapor_shared::{RetryFailureKind, ThrottleState};

pub mod logging;

/// Typed provider failure. Carries the shared retry taxonomy so the
/// engine's retry policy can classify provider errors without parsing
/// strings (AGENTS.md §8: explicit error enums, transient vs permanent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub failure: RetryFailureKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(failure: RetryFailureKind, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(RetryFailureKind::Transient, message)
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self::new(RetryFailureKind::Permanent, message)
    }

    pub fn authentication(message: impl Into<String>) -> Self {
        Self::new(RetryFailureKind::Authentication, message)
    }
}

impl Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} provider failure: {}",
            self.failure.label(),
            self.message
        )
    }
}

impl Error for ProviderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub supports_remote_changes_feed: bool,
    pub supports_server_side_rename: bool,
}

impl ProviderCapabilities {
    pub const GDRIVE_MVP: Self = Self {
        supports_remote_changes_feed: true,
        supports_server_side_rename: true,
    };
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ProviderCapabilities;

    fn poll_allowed(&self, throttle_state: ThrottleState) -> bool {
        let allowed = !matches!(throttle_state, ThrottleState::Suspended);
        if !allowed {
            logging::debug(
                "Blocked remote polling because throttle state is suspended",
                &[("throttle_state", format!("{:?}", throttle_state))],
            );
        }
        allowed
    }

    /// Ensures the cloud-side sync root exists. Required (no silent-Ok
    /// default): every provider must state explicitly whether it can
    /// honor this safety-relevant operation.
    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError>;
}

#[derive(Debug, Default)]
pub struct GoogleDriveProvider;

/// Pre-GA default provider stub backed by the local filesystem. All real
/// filesystem operations land in Phase 3 (P3-3); this stub exists so the
/// daemon does not run against `GoogleDriveProvider` by default and so any
/// caller of `default_provider()` sees a predictable, inert implementation.
#[derive(Debug, Default)]
pub struct FilesystemStubProvider;

pub fn default_provider() -> Box<dyn Provider> {
    Box::new(FilesystemStubProvider)
}

impl Provider for GoogleDriveProvider {
    fn name(&self) -> &'static str {
        "google_drive"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::GDRIVE_MVP
    }

    fn ensure_cloud_sync_directory(
        &self,
        _cloud_sync_directory: &str,
    ) -> Result<(), ProviderError> {
        // The real Drive integration lands in Phase C8-48. Failing loudly
        // beats pretending the folder exists.
        Err(ProviderError::permanent(
            "GoogleDriveProvider is not implemented yet (Phase C8)",
        ))
    }
}

impl Provider for FilesystemStubProvider {
    fn name(&self) -> &'static str {
        "filesystem_stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_remote_changes_feed: false,
            supports_server_side_rename: false,
        }
    }

    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError> {
        // Inert stub: it has no cloud side, so "ensuring" is a logged
        // no-op by design (not a silent trait default).
        logging::info(
            "Filesystem stub provider treats the cloud sync directory as always present",
            &[("cloud_sync_directory", cloud_sync_directory.to_string())],
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdrive_provider_exposes_expected_capabilities() {
        let provider = GoogleDriveProvider;
        assert_eq!(provider.name(), "google_drive");
        assert!(provider.capabilities().supports_remote_changes_feed);
    }

    #[test]
    fn poll_is_blocked_when_throttle_is_suspended() {
        let provider = GoogleDriveProvider;
        assert!(provider.poll_allowed(ThrottleState::Light));
        assert!(!provider.poll_allowed(ThrottleState::Suspended));
    }

    #[test]
    fn default_provider_returns_filesystem_stub_pre_ga() {
        let provider = default_provider();
        assert_eq!(provider.name(), "filesystem_stub");
    }

    #[test]
    fn filesystem_stub_reports_no_remote_changes_feed() {
        let provider = FilesystemStubProvider;
        assert!(!provider.capabilities().supports_remote_changes_feed);
        assert!(!provider.capabilities().supports_server_side_rename);
    }

    #[test]
    fn unimplemented_gdrive_ensure_directory_fails_with_permanent_classification() {
        let provider = GoogleDriveProvider;
        let error = provider
            .ensure_cloud_sync_directory("/Vapor")
            .expect_err("gdrive stub must not pretend the folder exists");
        assert_eq!(error.failure, RetryFailureKind::Permanent);
    }

    #[test]
    fn filesystem_stub_ensures_directory_without_error() {
        let provider = FilesystemStubProvider;
        assert!(provider.ensure_cloud_sync_directory("/Vapor").is_ok());
    }
}
