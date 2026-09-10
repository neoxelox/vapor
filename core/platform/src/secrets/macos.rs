//! macOS `SecretStore` backed by Keychain Services.
//!
//! Every secret is one generic-password item in the user's login
//! keychain: the service attribute is Vapor's namespace and the account
//! attribute is the secret name, so `vapor auth login` from the CLI and a
//! token refresh inside `vapord` read and write the same item.
//!
//! Keychain items carry an access list naming the applications allowed
//! to read them without a prompt. A new item is created with the calling
//! binary plus its companions (`vapor` and `vapord`, in the bundled
//! layout or side by side in a build directory) so the daemon never
//! blocks on a "vapord wants to use your keychain" dialog for a token the
//! CLI stored. The access list uses `SecAccessCreate`, which Apple has
//! deprecated in favour of the data-protection keychain; that keychain
//! needs a signed access-group entitlement on both binaries and rejects
//! unsigned development builds outright, so the legacy list stays the
//! practical choice until the release trust chain is in place.
//!
//! FFI is kept to the calls below. Each `unsafe` block states the
//! invariant it relies on.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::OSStatus;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use security_framework_sys::base::{
    SecCopyErrorMessageString, errSecDuplicateItem, errSecItemNotFound, errSecSuccess,
};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword,
    kSecMatchLimit, kSecMatchLimitAll, kSecReturnAttributes, kSecReturnData, kSecValueData,
};
use security_framework_sys::keychain_item::{
    SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
};
use vapor_shared::constants;

use super::{SecretStore, SecretStoreError};

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecAttrAccess: CFStringRef;
    fn SecAccessCreate(
        descriptor: CFStringRef,
        trusted_list: CFArrayRef,
        access: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut CFTypeRef) -> OSStatus;
}

/// Keychain-backed store. Cheap to construct; every call goes straight
/// to Keychain Services, so two processes never disagree about a value.
#[derive(Debug)]
pub struct NativeSecretStore {
    namespace: String,
}

impl NativeSecretStore {
    pub fn for_current_user() -> Result<Self, SecretStoreError> {
        Ok(Self::with_namespace(constants::secrets::STORE_NAMESPACE))
    }

    /// Store scoped to an explicit keychain service name. Tests use a
    /// throwaway namespace so they never touch Vapor's real entries.
    pub fn with_namespace(namespace: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
        }
    }

    fn base_query(&self, name: &str) -> Vec<(CFString, CFType)> {
        vec![
            (
                key(unsafe_key(&raw const kSecClass)),
                key(unsafe_key(&raw const kSecClassGenericPassword)).as_CFType(),
            ),
            (
                key(unsafe_key(&raw const kSecAttrService)),
                CFString::new(&self.namespace).as_CFType(),
            ),
            (
                key(unsafe_key(&raw const kSecAttrAccount)),
                CFString::new(name).as_CFType(),
            ),
        ]
    }

    fn add(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        let mut pairs = self.base_query(name);
        pairs.push((
            key(unsafe_key(&raw const kSecValueData)),
            CFData::from_buffer(value.as_bytes()).as_CFType(),
        ));
        pairs.push((
            key(unsafe_key(&raw const kSecAttrLabel)),
            CFString::new(&format!("Vapor {name}")).as_CFType(),
        ));
        if let Some(access) = create_access(&format!("Vapor {name}")) {
            pairs.push((key(unsafe_key(&raw const kSecAttrAccess)), access));
        }
        let attributes = CFDictionary::from_CFType_pairs(&pairs);
        // SAFETY: `attributes` is a live CFDictionary for the duration of
        // the call and we pass a null result pointer, so Keychain
        // Services returns nothing we would have to release.
        let status = unsafe { SecItemAdd(attributes.as_concrete_TypeRef(), ptr::null_mut()) };
        check(status, name)
    }

    fn update(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        let query = CFDictionary::from_CFType_pairs(&self.base_query(name));
        let changes = CFDictionary::from_CFType_pairs(&[(
            key(unsafe_key(&raw const kSecValueData)),
            CFData::from_buffer(value.as_bytes()).as_CFType(),
        )]);
        // SAFETY: both dictionaries outlive the call; `SecItemUpdate`
        // only reads them.
        let status =
            unsafe { SecItemUpdate(query.as_concrete_TypeRef(), changes.as_concrete_TypeRef()) };
        check(status, name)
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self, name: &str) -> Result<String, SecretStoreError> {
        let mut pairs = self.base_query(name);
        pairs.push((
            key(unsafe_key(&raw const kSecReturnData)),
            CFBoolean::true_value().as_CFType(),
        ));
        let query = CFDictionary::from_CFType_pairs(&pairs);
        let mut result: CFTypeRef = ptr::null();
        // SAFETY: `query` outlives the call and `result` is a valid
        // out-pointer. On success Keychain Services hands us a +1
        // reference that `wrap_under_create_rule` releases on drop.
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
        check(status, name)?;
        if result.is_null() {
            return Err(SecretStoreError::NotFound(name.to_string()));
        }
        // SAFETY: `result` is non-null and owned by us (create rule).
        let value = unsafe { CFType::wrap_under_create_rule(result) };
        let data = value
            .downcast_into::<CFData>()
            .ok_or_else(|| backend(format!("keychain returned a non-data value for '{name}'")))?;
        String::from_utf8(data.bytes().to_vec())
            .map_err(|_| backend(format!("secret '{name}' is not valid UTF-8")))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        match self.update(name, value) {
            Err(SecretStoreError::NotFound(_)) => match self.add(name, value) {
                // Lost a create race against another Vapor process; the
                // item exists now, so the update path applies.
                Err(SecretStoreError::Backend(error))
                    if error.to_string().contains(DUPLICATE_MARKER) =>
                {
                    self.update(name, value)
                }
                other => other,
            },
            other => other,
        }
    }

    fn delete(&self, name: &str) -> Result<(), SecretStoreError> {
        let query = CFDictionary::from_CFType_pairs(&self.base_query(name));
        // SAFETY: `query` outlives the call; `SecItemDelete` only reads it.
        let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
        match check(status, name) {
            Err(SecretStoreError::NotFound(_)) => Ok(()),
            other => other,
        }
    }

    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        let query = CFDictionary::from_CFType_pairs(&[
            (
                key(unsafe_key(&raw const kSecClass)),
                key(unsafe_key(&raw const kSecClassGenericPassword)).as_CFType(),
            ),
            (
                key(unsafe_key(&raw const kSecAttrService)),
                CFString::new(&self.namespace).as_CFType(),
            ),
            (
                key(unsafe_key(&raw const kSecReturnAttributes)),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                key(unsafe_key(&raw const kSecMatchLimit)),
                key(unsafe_key(&raw const kSecMatchLimitAll)).as_CFType(),
            ),
        ]);
        let mut result: CFTypeRef = ptr::null();
        // SAFETY: as in `get`; the +1 result is an array of attribute
        // dictionaries that we own and release on drop.
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
        match check(status, "*") {
            Ok(()) => {}
            Err(SecretStoreError::NotFound(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        }
        if result.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: with `kSecMatchLimitAll` and attributes requested the
        // result is a CFArray of CFDictionary; we own the reference.
        let entries: CFArray<CFDictionary<CFString, CFType>> =
            unsafe { CFArray::wrap_under_create_rule(result as CFArrayRef) };
        let account_key = key(unsafe_key(&raw const kSecAttrAccount));
        let mut names: Vec<String> = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .find(&account_key)
                    .and_then(|value| value.downcast::<CFString>())
                    .map(|account| account.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    fn is_persistent(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        "login keychain".to_string()
    }
}

const DUPLICATE_MARKER: &str = "[duplicate]";

fn backend(message: String) -> SecretStoreError {
    SecretStoreError::Backend(message.into())
}

/// Reads an `extern static` CFString constant. Split out so every
/// call site is one obviously-correct expression.
fn unsafe_key(constant: *const CFStringRef) -> CFStringRef {
    // SAFETY: the pointer names a Security.framework constant that is
    // initialised at load time and never changes.
    unsafe { *constant }
}

fn key(reference: CFStringRef) -> CFString {
    // SAFETY: framework constants are owned by the framework; the get
    // rule retains a reference we release on drop.
    unsafe { CFString::wrap_under_get_rule(reference) }
}

const SUCCESS: OSStatus = errSecSuccess;
const NOT_FOUND: OSStatus = errSecItemNotFound;
const DUPLICATE: OSStatus = errSecDuplicateItem;

fn check(status: OSStatus, name: &str) -> Result<(), SecretStoreError> {
    match status {
        SUCCESS => Ok(()),
        NOT_FOUND => Err(SecretStoreError::NotFound(name.to_string())),
        DUPLICATE => Err(backend(format!(
            "{DUPLICATE_MARKER} keychain item '{name}' already exists"
        ))),
        other => Err(backend(format!(
            "keychain error {other} for '{name}': {}",
            describe(other)
        ))),
    }
}

fn describe(status: OSStatus) -> String {
    // SAFETY: a null reserved pointer is the documented calling
    // convention; the returned string is +1 and released on drop.
    let message = unsafe { SecCopyErrorMessageString(status, ptr::null_mut()) };
    if message.is_null() {
        return "unknown error".to_string();
    }
    // SAFETY: non-null CFStringRef owned by us.
    unsafe { CFString::wrap_under_create_rule(message) }.to_string()
}

/// Builds the access list for a new item: this binary plus every
/// companion Vapor binary found next to it. `None` falls back to the
/// keychain default (creator only), which still works but prompts the
/// user the first time the other process reads the item.
fn create_access(descriptor: &str) -> Option<CFType> {
    let mut apps: Vec<CFType> = Vec::new();
    // A null path means "the calling application".
    if let Some(app) = trusted_application(None) {
        apps.push(app);
    }
    for path in companion_binaries() {
        if let Some(app) = trusted_application(Some(&path)) {
            apps.push(app);
        }
    }
    if apps.is_empty() {
        return None;
    }
    let trusted = CFArray::from_CFTypes(&apps);
    let descriptor = CFString::new(descriptor);
    let mut access: CFTypeRef = ptr::null();
    // SAFETY: descriptor and array outlive the call; `access` is a valid
    // out-pointer receiving a +1 reference.
    let status = unsafe {
        SecAccessCreate(
            descriptor.as_concrete_TypeRef(),
            trusted.as_concrete_TypeRef(),
            &mut access,
        )
    };
    if status != errSecSuccess || access.is_null() {
        return None;
    }
    // SAFETY: non-null, owned by us.
    Some(unsafe { CFType::wrap_under_create_rule(access) })
}

fn trusted_application(path: Option<&Path>) -> Option<CFType> {
    let c_path = match path {
        Some(path) => Some(CString::new(path.as_os_str().as_encoded_bytes()).ok()?),
        None => None,
    };
    let mut app: CFTypeRef = ptr::null();
    // SAFETY: `c_path` (or null) is a valid NUL-terminated path for the
    // duration of the call; `app` receives a +1 reference on success.
    let status = unsafe {
        SecTrustedApplicationCreateFromPath(
            c_path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            &mut app,
        )
    };
    if status != errSecSuccess || app.is_null() {
        return None;
    }
    // SAFETY: non-null, owned by us.
    Some(unsafe { CFType::wrap_under_create_rule(app) })
}

/// The other Vapor executables that share this store: siblings in a
/// build directory, or `Contents/Helpers/vapor` and
/// `Contents/MacOS/vapord` inside the app bundle.
fn companion_binaries() -> Vec<PathBuf> {
    let Ok(current) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(parent) = current.parent() else {
        return Vec::new();
    };
    let cli = constants::runtime::CLI_BINARY_NAME;
    let daemon = constants::runtime::DAEMON_BINARY_NAME;
    let mut candidates = vec![parent.join(cli), parent.join(daemon)];
    if let Some(contents) = parent.parent() {
        candidates.push(contents.join("Helpers").join(cli));
        candidates.push(contents.join("MacOS").join(daemon));
    }
    let mut found = Vec::new();
    for candidate in candidates {
        if candidate == current || !candidate.is_file() || found.contains(&candidate) {
            continue;
        }
        found.push(candidate);
    }
    found
}
