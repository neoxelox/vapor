//! Linux `SecretStore` stub. The Secret Service (desktop) and
//! age-encrypted file (headless) backends land when Linux becomes a
//! shipping surface.

use super::{SecretStore, SecretStoreError};

#[derive(Debug)]
pub struct NativeSecretStore;

impl NativeSecretStore {
    pub fn for_current_user() -> Result<Self, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Linux yet",
        ))
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self, _name: &str) -> Result<String, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Linux yet",
        ))
    }
    fn set(&self, _name: &str, _value: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Linux yet",
        ))
    }
    fn delete(&self, _name: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Linux yet",
        ))
    }
    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        Err(SecretStoreError::Unsupported(
            "no native secret store on Linux yet",
        ))
    }
    fn is_persistent(&self) -> bool {
        true
    }
}
