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
