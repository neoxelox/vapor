#![forbid(unsafe_code)]

use vapor_shared::ThrottleState;

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
        !matches!(throttle_state, ThrottleState::Suspended)
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
