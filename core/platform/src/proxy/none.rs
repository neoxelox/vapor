//! A host whose system settings name no proxy that Vapor reads: Linux
//! (the environment of the service carries it, and the HTTP transport
//! reads that) and Windows (not a shipping surface yet).

use super::{ProxySettings, SystemProxy};

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProxySettings;

impl NativeProxySettings {
    pub fn for_current_host() -> Self {
        Self
    }
}

impl ProxySettings for NativeProxySettings {
    fn https_proxy(&self) -> Option<SystemProxy> {
        None
    }
}
