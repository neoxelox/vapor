//! `vapor auth login|logout|status`.
//!
//! Wave 7 ships the SecretStore-backed plumbing per `cli.md`
//! L4-1..L4-3. Real OAuth-PKCE flows are deferred to the C8 wave when
//! provider integrations land (Google Drive in C8-48); for now `login`
//! takes an explicit `--token` argument so headless / CI flows can
//! preload tokens, and the CLI surface is wire-compatible with the
//! browser-based flow that arrives later.

use std::error::Error;
use std::fmt::{self, Display};

use vapor_platform::{InMemorySecretStore, NativeSecretStore, SecretStore, SecretStoreError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthCommand {
    Login {
        provider: String,
        /// Pre-Wave-8 the CLI accepts a raw token via `--token`.
        /// When the OAuth-PKCE flow lands the token argument becomes
        /// optional and the CLI defaults to launching the browser.
        token: String,
    },
    Logout {
        provider: String,
    },
    Status,
}

#[derive(Debug)]
pub enum AuthError {
    /// Pre-Wave-8 the supported provider names are documented inline.
    /// Unknown provider strings fail fast so users don't accidentally
    /// store a token under a typoed key.
    UnknownProvider(String),
    Store(SecretStoreError),
}

impl Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider(name) => {
                write!(
                    f,
                    "unknown provider '{name}' — supported providers: {}",
                    SUPPORTED_PROVIDERS.join(", ")
                )
            }
            Self::Store(error) => write!(f, "secret store error: {error}"),
        }
    }
}

impl Error for AuthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SecretStoreError> for AuthError {
    fn from(error: SecretStoreError) -> Self {
        Self::Store(error)
    }
}

const SUPPORTED_PROVIDERS: &[&str] = &["filesystem", "google_drive"];

fn validate_provider(name: &str) -> Result<(), AuthError> {
    if SUPPORTED_PROVIDERS.contains(&name) {
        Ok(())
    } else {
        Err(AuthError::UnknownProvider(name.to_string()))
    }
}

fn key(provider: &str) -> String {
    format!("auth.{provider}.token")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthStatusEntry {
    pub provider: String,
    /// `true` when a token is present in the secret store; we never
    /// expose the token value itself per `cli.md` L4-3.
    pub bound: bool,
}

pub fn login_into(store: &dyn SecretStore, provider: &str, token: &str) -> Result<(), AuthError> {
    validate_provider(provider)?;
    store.set(&key(provider), token)?;
    Ok(())
}

pub fn logout_from(store: &dyn SecretStore, provider: &str) -> Result<(), AuthError> {
    validate_provider(provider)?;
    store.delete(&key(provider))?;
    Ok(())
}

pub fn status_from(store: &dyn SecretStore) -> Result<Vec<AuthStatusEntry>, AuthError> {
    let mut entries = Vec::new();
    for provider in SUPPORTED_PROVIDERS {
        let bound = store.get(&key(provider)).is_ok();
        entries.push(AuthStatusEntry {
            provider: provider.to_string(),
            bound,
        });
    }
    Ok(entries)
}

/// Production constructor for the native secret store. Wave 7 falls
/// back to the in-process [`InMemorySecretStore`] until the per-OS
/// Keychain / Credential Manager / libsecret bridges land in their
/// respective C4-5 / Wave 12 / Wave 13 work.
pub fn build_native_store() -> Box<dyn SecretStore> {
    match NativeSecretStore::for_current_user() {
        Ok(store) => Box::new(store),
        Err(_) => Box::new(InMemorySecretStore::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_then_status_reports_provider_as_bound() {
        let store = InMemorySecretStore::new();
        login_into(&store, "filesystem", "abc").expect("login");
        let entries = status_from(&store).expect("status");
        let filesystem = entries
            .iter()
            .find(|e| e.provider == "filesystem")
            .expect("filesystem entry");
        assert!(filesystem.bound);
        let google = entries
            .iter()
            .find(|e| e.provider == "google_drive")
            .expect("google_drive entry");
        assert!(!google.bound);
    }

    #[test]
    fn logout_clears_token_from_store() {
        let store = InMemorySecretStore::new();
        login_into(&store, "google_drive", "ya29.x").expect("login");
        logout_from(&store, "google_drive").expect("logout");
        let entries = status_from(&store).expect("status");
        assert!(entries.iter().all(|entry| !entry.bound));
    }

    #[test]
    fn login_rejects_unknown_provider_with_typed_error() {
        let store = InMemorySecretStore::new();
        let error = login_into(&store, "icloud_drive", "x").expect_err("unknown provider");
        assert!(matches!(error, AuthError::UnknownProvider(_)));
    }

    #[test]
    fn logout_rejects_unknown_provider() {
        let store = InMemorySecretStore::new();
        let error = logout_from(&store, "icloud_drive").expect_err("unknown provider");
        assert!(matches!(error, AuthError::UnknownProvider(_)));
    }

    #[test]
    fn status_never_returns_the_token_value_itself() {
        // L4-3 invariant: `vapor auth status` lists bound providers,
        // never the secret. We assert structurally — `AuthStatusEntry`
        // intentionally has no token field.
        let entry = AuthStatusEntry {
            provider: "filesystem".to_string(),
            bound: true,
        };
        let debug = format!("{entry:?}");
        assert!(!debug.contains("token"));
    }
}
