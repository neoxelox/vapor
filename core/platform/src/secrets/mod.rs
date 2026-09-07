//! Secret store trait, the in-memory fake, and the per-OS native store.
//!
//! See `docs/architecture/platform-abstractions.md` §`SecretStore`.
//!
//! macOS stores secrets in the login keychain through Keychain Services
//! (`macos.rs`). Linux uses the program named by `VAPOR_SECRETS_COMMAND`
//! or, on a desktop, the Secret Service through `secret-tool`
//! (`linux.rs`); a host with neither gets [`SecretStoreError::Unsupported`]
//! naming the way out. Windows (Credential Manager) is not a shipping
//! surface yet, so its constructor returns `Unsupported` and callers fall
//! back to the process-local fake with a visible warning.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeSecretStore;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativeSecretStore;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativeSecretStore;

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
    /// Removing a secret that does not exist is not an error.
    fn delete(&self, name: &str) -> Result<(), SecretStoreError>;
    fn list(&self) -> Result<Vec<String>, SecretStoreError>;

    /// Whether values written via `set` survive process exit. The
    /// in-memory fake returns `false`; every native store returns
    /// `true`. Surfaces that store user-visible secrets propagate this
    /// flag so a user is never told "stored" about a value that will
    /// vanish at process exit.
    fn is_persistent(&self) -> bool {
        false
    }

    /// Where the secrets live, for `vapor doctor` and `vapor auth`
    /// output: "login keychain", "Secret Service", the shim command.
    fn describe(&self) -> String {
        "in-memory store".to_string()
    }
}

/// Process-local in-memory secret store. Default for tests and the
/// fallback on OSes without a native store.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract every `SecretStore` implementation must satisfy. The
    /// fake and the native store both run it, so fake-vs-native drift
    /// shows up here rather than in production.
    fn exercise_contract(store: &dyn SecretStore, prefix: &str) {
        let a = format!("{prefix}.alpha");
        let b = format!("{prefix}.beta");

        assert!(matches!(
            store.get(&a).expect_err("missing before set"),
            SecretStoreError::NotFound(_)
        ));
        store.delete(&a).expect("deleting a missing secret is fine");

        store.set(&a, "one").expect("set a");
        store.set(&b, "two").expect("set b");
        assert_eq!(store.get(&a).expect("get a"), "one");
        assert_eq!(store.get(&b).expect("get b"), "two");

        store.set(&a, "one-updated").expect("overwrite a");
        assert_eq!(store.get(&a).expect("get a after overwrite"), "one-updated");

        let mut listed: Vec<String> = store
            .list()
            .expect("list")
            .into_iter()
            .filter(|name| name.starts_with(prefix))
            .collect();
        listed.sort();
        assert_eq!(listed, vec![a.clone(), b.clone()]);

        store.delete(&a).expect("delete a");
        assert!(matches!(
            store.get(&a).expect_err("missing after delete"),
            SecretStoreError::NotFound(_)
        ));
        assert_eq!(store.get(&b).expect("b survives a's delete"), "two");
        store.delete(&b).expect("delete b");
        assert!(
            store
                .list()
                .expect("list after cleanup")
                .iter()
                .all(|name| !name.starts_with(prefix))
        );
    }

    #[test]
    fn in_memory_store_satisfies_the_contract() {
        let store = InMemorySecretStore::new();
        exercise_contract(&store, "contract");
        assert!(!store.is_persistent());
    }

    #[test]
    fn in_memory_store_keeps_values_unicode_intact() {
        let store = InMemorySecretStore::new();
        store.set("k", "tökén ✓ {\"json\":true}").expect("set");
        assert_eq!(store.get("k").expect("get"), "tökén ✓ {\"json\":true}");
    }

    /// The native store runs the same contract against the real login
    /// keychain under a throwaway service name, so Vapor's own entries
    /// are never touched and the test leaves nothing behind.
    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_store_satisfies_the_contract() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let service = format!("sh.arn.vapor.test.{}.{nanos}", std::process::id());
        let store = NativeSecretStore::with_namespace(&service);
        assert!(store.is_persistent());
        exercise_contract(&store, "contract");
        // Entries are scoped by service: a sibling namespace sees nothing.
        let other = NativeSecretStore::with_namespace(&format!("{service}.other"));
        store.set("scoped", "v").expect("set");
        assert!(matches!(
            other.get("scoped").expect_err("other namespace is empty"),
            SecretStoreError::NotFound(_)
        ));
        assert!(other.list().expect("list").is_empty());
        store.delete("scoped").expect("cleanup");
    }
}
