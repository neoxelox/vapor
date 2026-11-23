//! Windows `SecretStore` stub. The Credential Manager backend lands
//! when Windows becomes a shipping surface.

use super::{SecretStore, SecretStoreError};

#[derive(Debug)]
pub struct NativeSecretStore;

impl NativeSecretStore {
    pub fn for_current_user() -> Result<Self, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Windows yet",
        ))
    }

    /// For a background process; nothing to turn off in the stub.
    pub fn without_prompts(self) -> Self {
        self
    }

    /// Every process writes its own secrets here: no backend on this
    /// OS ties an item to the binary that created it.
    pub fn writing_through_the_daemon(self) -> Self {
        self
    }

    /// The store a helper request names by its namespace: the one
    /// store there is.
    pub fn helper_store(_namespace: &str) -> Result<Self, SecretStoreError> {
        Self::for_current_user()
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self, _name: &str) -> Result<String, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Windows yet",
        ))
    }
    fn set(&self, _name: &str, _value: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Windows yet",
        ))
    }
    fn delete(&self, _name: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Windows yet",
        ))
    }
    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Windows yet",
        ))
    }
    fn is_persistent(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        "Credential Manager".to_string()
    }
}
