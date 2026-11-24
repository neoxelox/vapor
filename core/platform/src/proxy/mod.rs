//! The HTTPS proxy the system is set to use.
//!
//! A network that sends web traffic through a proxy sets one in the
//! network settings of the OS, not in the environment of a background
//! job. The daemon reads it here, so a provider that talks HTTPS
//! reaches its cloud on such a network.
//!
//! macOS reads the proxies of the current network (`macos.rs`). Linux
//! has no one place for it: a desktop keeps it in its own settings
//! store, and a server in the environment of the service, which the
//! HTTP transport reads on every OS. So the Linux implementation
//! reports none. Windows (WinHTTP) is not a shipping surface yet and
//! reports none as well.
//!
//! Only a proxy with a fixed host and port is reported. A network that
//! names its proxy with an auto-configuration script is not followed:
//! the script is JavaScript, and running it is out of proportion for a
//! sync daemon.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeProxySettings;

#[cfg(not(target_os = "macos"))]
mod none;
#[cfg(not(target_os = "macos"))]
pub use none::NativeProxySettings;

/// A proxy with a fixed address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxy {
    pub host: String,
    pub port: u16,
    /// Hosts that are reached with no proxy: a full host name, or a
    /// suffix written as `*.example.com` or `.example.com`.
    pub bypass: Vec<String>,
}

impl SystemProxy {
    /// Whether `host` is reached through the proxy.
    pub fn applies_to(&self, host: &str) -> bool {
        !bypasses(&self.bypass, host)
    }
}

/// Whether one of the bypass rules names `host`. A rule is a full host
/// name, or a suffix written as `*.example.com` or `.example.com`; the
/// suffix also names `example.com` itself. An address range in a rule
/// is not read: a cloud API is reached by its name.
pub fn bypasses(rules: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    rules.iter().any(|rule| {
        let rule = rule.trim().trim_end_matches('.').to_ascii_lowercase();
        let suffix = rule
            .strip_prefix("*.")
            .or_else(|| rule.strip_prefix('.'))
            .filter(|suffix| !suffix.is_empty());
        match suffix {
            Some(suffix) => {
                host == suffix
                    || host
                        .strip_suffix(suffix)
                        .is_some_and(|start| start.ends_with('.'))
            }
            None => rule == "*" || (!rule.is_empty() && host == rule),
        }
    })
}

pub trait ProxySettings: Send + Sync {
    /// The proxy for HTTPS requests on the network of now, or `None`
    /// when the system names none. Read again by the caller when it
    /// wants the setting of now: a device moves between networks.
    fn https_proxy(&self) -> Option<SystemProxy>;
}

/// A fixed answer, for tests and for a host with no settings to read.
#[derive(Clone, Debug, Default)]
pub struct FixedProxySettings(pub Option<SystemProxy>);

impl ProxySettings for FixedProxySettings {
    fn https_proxy(&self) -> Option<SystemProxy> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What every implementation holds to: an answer comes back, and a
    /// proxy that is reported has an address a client can connect to.
    fn exercise_contract(settings: &dyn ProxySettings) {
        for _ in 0..2 {
            if let Some(proxy) = settings.https_proxy() {
                assert!(!proxy.host.trim().is_empty(), "{proxy:?}");
                assert_ne!(proxy.port, 0, "{proxy:?}");
            }
        }
    }

    #[test]
    fn the_fixed_settings_satisfy_the_contract() {
        exercise_contract(&FixedProxySettings(None));
        exercise_contract(&FixedProxySettings(Some(SystemProxy {
            host: "proxy.example".to_string(),
            port: 3128,
            bypass: Vec::new(),
        })));
    }

    /// The native settings of this host, whatever they say today.
    #[test]
    fn the_native_settings_satisfy_the_contract() {
        exercise_contract(&NativeProxySettings::for_current_host());
    }

    #[test]
    fn a_bypass_rule_names_a_host_or_every_host_under_a_suffix() {
        let rules: Vec<String> = ["*.local", ".corp.example", "intranet", "169.254/16"]
            .iter()
            .map(|rule| rule.to_string())
            .collect();
        for through_no_proxy in [
            "printer.local",
            "LOCAL",
            "git.corp.example",
            "corp.example.",
            "intranet",
        ] {
            assert!(bypasses(&rules, through_no_proxy), "{through_no_proxy}");
        }
        for through_the_proxy in [
            "www.googleapis.com",
            "notlocal",
            "mylocal",
            "xcorp.example",
            "intranet.example",
        ] {
            assert!(!bypasses(&rules, through_the_proxy), "{through_the_proxy}");
        }
        assert!(bypasses(&["*".to_string()], "anything.example"));
        let proxy = SystemProxy {
            host: "proxy.example".to_string(),
            port: 8080,
            bypass: rules,
        };
        assert!(proxy.applies_to("oauth2.googleapis.com"));
        assert!(!proxy.applies_to("nas.local"));
    }
}
