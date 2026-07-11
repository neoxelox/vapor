//! Secret store trait + per-OS native implementations.
//!
//! See `docs/architecture/platform-abstractions.md` §`SecretStore`.
//!
//! The native impls land progressively: macOS uses Keychain Services,
//! Linux uses libsecret (desktop) / age-encrypted file (headless),
//! Windows uses Credential Manager. The trait ships with an in-memory
//! fake usable from every host; the real Keychain bridge is not wired
//! up yet.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::sync::Mutex;

#[derive(Debug)]
pub enum SecretStoreError {
    /// The named secret does not exist in the backing store.
    NotFound(String),
    /// The OS-native store rejected the operation (e.g. user denied
    /// Keychain access, libsecret D-Bus call timed out).
    Backend(Box<dyn Error + Send + Sync>),
    /// Operation is not supported on the current OS yet.
    Unsupported(&'static str),
}

impl Display for SecretStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "secret '{name}' not found"),
            Self::Backend(error) => write!(f, "secret store backend failed: {error}"),
            Self::Unsupported(reason) => write!(f, "secret store not supported: {reason}"),
        }
    }
}

impl Error for SecretStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(&**error),
            _ => None,
        }
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<String, SecretStoreError>;
    fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, name: &str) -> Result<(), SecretStoreError>;
    fn list(&self) -> Result<Vec<String>, SecretStoreError>;

    /// Whether values written via `set` survive process exit. Process-
    /// local fakes (the in-memory store, the pre-bridge `NativeSecretStore`
    /// today on every OS) return `false`; the real Keychain / libsecret /
    /// Credential Manager backends will return `true` once their
    /// native bridges land. Surfaces of
    /// the trait that store user-visible secrets must propagate this
    /// flag so users aren't told "stored" when the value is going to
    /// disappear at process exit (`vapor auth login` would otherwise
    /// silently lose every token otherwise).
    fn is_persistent(&self) -> bool {
        false
    }
}

/// Process-local in-memory secret store. Default for tests, the
/// headless test fixture, and the placeholder while the Keychain
/// bridge is being written.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    inner: Mutex<BTreeMap<String, String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, name: &str) -> Result<String, SecretStoreError> {
        self.inner
            .lock()
            .expect("InMemorySecretStore mutex poisoned")
            .get(name)
            .cloned()
            .ok_or_else(|| SecretStoreError::NotFound(name.to_string()))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        self.inner
            .lock()
            .expect("InMemorySecretStore mutex poisoned")
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretStoreError> {
        self.inner
            .lock()
            .expect("InMemorySecretStore mutex poisoned")
            .remove(name);
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        Ok(self
            .inner
            .lock()
            .expect("InMemorySecretStore mutex poisoned")
            .keys()
            .cloned()
            .collect())
    }

    fn is_persistent(&self) -> bool {
        false
    }
}

/// Native secret store. Until the per-OS Keychain / libsecret /
/// Credential Manager bridges land, this
/// type aliases to [`InMemorySecretStore`] so the engine can already
/// thread the trait through. Construction returns
/// [`SecretStoreError::Unsupported`] on platforms whose real bridge
/// isn't ready yet, and a working in-memory store on macOS pre-bridge.
#[derive(Debug, Default)]
pub struct NativeSecretStore {
    inner: InMemorySecretStore,
}

impl NativeSecretStore {
    pub fn for_current_user() -> Result<Self, SecretStoreError> {
        // A real Keychain Services bridge will replace the in-memory
        // backing. Until then this is a strict
        // process-local store — sufficient for `vapor doctor`-style
        // checks that the trait surface is wired correctly.
        Ok(Self::default())
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self, name: &str) -> Result<String, SecretStoreError> {
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        self.inner.set(name, value)
    }
    fn delete(&self, name: &str) -> Result<(), SecretStoreError> {
        self.inner.delete(name)
    }
    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        self.inner.list()
    }
    fn is_persistent(&self) -> bool {
        // Flips to `true` once the macOS Keychain bridge is wired in
        // and the Linux / Windows native bridges land. Until then this
        // is a
        // strict process-local store.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_store_round_trips_set_get_delete_list() {
        let store = InMemorySecretStore::new();
        store.set("token.gdrive", "abc123").expect("set");
        assert_eq!(store.get("token.gdrive").expect("get"), "abc123");
        assert_eq!(
            store.list().expect("list"),
            vec!["token.gdrive".to_string()]
        );
        store.delete("token.gdrive").expect("delete");
        assert!(matches!(
            store.get("token.gdrive").expect_err("missing after delete"),
            SecretStoreError::NotFound(_)
        ));
    }

    #[test]
    fn in_memory_store_overwrites_existing_values() {
        let store = InMemorySecretStore::new();
        store.set("k", "v1").expect("set");
        store.set("k", "v2").expect("set");
        assert_eq!(store.get("k").expect("get"), "v2");
    }
}
