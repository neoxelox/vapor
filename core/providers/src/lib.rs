#![forbid(unsafe_code)]

use vapor_shared::ThrottleState;

pub mod logging;

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

pub trait Provider {
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

    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), String> {
        logging::info(
            "Ensuring cloud sync directory",
            &[("cloud_sync_directory", cloud_sync_directory.to_string())],
        );
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct GoogleDriveProvider;

impl Provider for GoogleDriveProvider {
    fn name(&self) -> &'static str {
        "google_drive"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::GDRIVE_MVP
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
}
