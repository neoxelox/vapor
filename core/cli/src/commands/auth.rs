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
        /// Credentials are namespaced per profile (C8-20); omitting
        /// `--profile` targets the implicit `default` profile.
        profile: String,
    },
    Logout {
        provider: String,
        profile: String,
    },
    Status {
        profile: String,
    },
}

#[derive(Debug)]
pub enum AuthError {
    /// Pre-Wave-8 the supported provider names are documented inline.
    /// Unknown provider strings fail fast so users don't accidentally
    /// store a token under a typoed key.
    UnknownProvider(String),
    /// Profile ids are validated the same way the daemon validates
    /// them, so a typo never mints a stray secret namespace.
    InvalidProfile(String),
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
            Self::InvalidProfile(name) => {
                write!(
                    f,
                    "invalid profile id '{name}' — profile ids are short lowercase slugs"
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

const SUPPORTED_PROVIDERS: &[&str] = vapor_shared::constants::provider::ALL;

fn validate_provider(name: &str) -> Result<(), AuthError> {
    if SUPPORTED_PROVIDERS.contains(&name) {
        Ok(())
    } else {
        Err(AuthError::UnknownProvider(name.to_string()))
    }
}

fn validate_profile(profile_id: &str) -> Result<(), AuthError> {
    if vapor_daemon::profiles::is_valid_profile_id(profile_id) {
        Ok(())
    } else {
        Err(AuthError::InvalidProfile(profile_id.to_string()))
    }
}

fn key(profile_id: &str, provider: &str) -> String {
    vapor_daemon::profiles::secret_key(profile_id, provider, "token")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthStatusEntry {
    pub profile: String,
    pub provider: String,
    /// `true` when a token is present in the secret store; we never
    /// expose the token value itself per `cli.md` L4-3.
    pub bound: bool,
}

pub fn login_into(
    store: &dyn SecretStore,
    profile_id: &str,
    provider: &str,
    token: &str,
) -> Result<(), AuthError> {
    validate_profile(profile_id)?;
    validate_provider(provider)?;
    store.set(&key(profile_id, provider), token)?;
    Ok(())
}

pub fn logout_from(
    store: &dyn SecretStore,
    profile_id: &str,
    provider: &str,
) -> Result<(), AuthError> {
    validate_profile(profile_id)?;
    validate_provider(provider)?;
    store.delete(&key(profile_id, provider))?;
    Ok(())
}

pub fn status_from(
    store: &dyn SecretStore,
    profile_id: &str,
) -> Result<Vec<AuthStatusEntry>, AuthError> {
    validate_profile(profile_id)?;
    let mut entries = Vec::new();
    for provider in SUPPORTED_PROVIDERS {
        let bound = store.get(&key(profile_id, provider)).is_ok();
        entries.push(AuthStatusEntry {
            profile: profile_id.to_string(),
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

/// Interactive OAuth-PKCE login for Google Drive (C8-48): a loopback
/// redirect listener plus the system browser. Blocking by design — the
/// CLI waits for the consent hop. Returns the stored-token JSON that
/// goes into the secret store.
pub fn run_gdrive_pkce_flow() -> Result<String, String> {
    use std::io::{BufRead, BufReader, Write};
    use vapor_providers::gdrive::oauth;

    let client_id = std::env::var(vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "{} is not set. Create an OAuth client id (Desktop app) in the Google Cloud console and export it; see docs/operations/provider-auth-operations.md",
                vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID
            )
        })?;
    let client_secret = std::env::var(vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_SECRET)
        .ok()
        .filter(|value| !value.trim().is_empty());

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("cannot bind the loopback redirect listener: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("cannot resolve listener address: {error}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let verifier = oauth::generate_code_verifier();
    let challenge = oauth::code_challenge(&verifier);
    let url = oauth::authorization_url(&client_id, &redirect_uri, &challenge);

    eprintln!("Open this URL in your browser to authorize Vapor:");
    eprintln!("  {url}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    eprintln!("Waiting for the authorization redirect on {redirect_uri} ...");

    let (stream, _) = listener
        .accept()
        .map_err(|error| format!("redirect listener failed: {error}"))?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("cannot read the redirect request: {error}"))?;
    // GET /?code=...&scope=... HTTP/1.1
    let code = request_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split('?').nth(1))
        .and_then(|query| {
            query
                .split('&')
                .find(|pair| pair.starts_with("code="))
                .map(|pair| pair.trim_start_matches("code=").to_string())
        })
        .filter(|code| !code.is_empty())
        .ok_or_else(|| "the redirect did not carry an authorization code".to_string())?;
    let mut stream = stream;
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK
Content-Type: text/html

<html><body>Vapor is authorized. You can close this tab.</body></html>",
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let tokens = oauth::exchange_code(
        &vapor_providers::http::NativeHttpTransport,
        &client_id,
        client_secret.as_deref(),
        &code,
        &verifier,
        &redirect_uri,
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    serde_json::to_string(&tokens).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_then_status_reports_provider_as_bound() {
        let store = InMemorySecretStore::new();
        login_into(&store, "default", "filesystem", "abc").expect("login");
        let entries = status_from(&store, "default").expect("status");
        let filesystem = entries
            .iter()
            .find(|e| e.provider == "filesystem")
            .expect("filesystem entry");
        assert!(filesystem.bound);
        let google = entries
            .iter()
            .find(|e| e.provider == "gdrive")
            .expect("gdrive entry");
        assert!(!google.bound);
    }

    #[test]
    fn credentials_are_namespaced_per_profile() {
        // C8-20: a token bound to one profile must be invisible to
        // every other profile.
        let store = InMemorySecretStore::new();
        login_into(&store, "work", "gdrive", "ya29.work").expect("login");
        let work = status_from(&store, "work").expect("status");
        assert!(work.iter().any(|e| e.provider == "gdrive" && e.bound));
        let home = status_from(&store, "home").expect("status");
        assert!(home.iter().all(|e| !e.bound));
    }

    #[test]
    fn logout_clears_token_from_store() {
        let store = InMemorySecretStore::new();
        login_into(&store, "default", "gdrive", "ya29.x").expect("login");
        logout_from(&store, "default", "gdrive").expect("logout");
        let entries = status_from(&store, "default").expect("status");
        assert!(entries.iter().all(|entry| !entry.bound));
    }

    #[test]
    fn login_rejects_unknown_provider_with_typed_error() {
        let store = InMemorySecretStore::new();
        let error =
            login_into(&store, "default", "icloud_drive", "x").expect_err("unknown provider");
        assert!(matches!(error, AuthError::UnknownProvider(_)));
    }

    #[test]
    fn login_rejects_invalid_profile_ids() {
        let store = InMemorySecretStore::new();
        let error =
            login_into(&store, "Not A Slug!", "filesystem", "x").expect_err("invalid profile");
        assert!(matches!(error, AuthError::InvalidProfile(_)));
    }

    #[test]
    fn logout_rejects_unknown_provider() {
        let store = InMemorySecretStore::new();
        let error = logout_from(&store, "default", "icloud_drive").expect_err("unknown provider");
        assert!(matches!(error, AuthError::UnknownProvider(_)));
    }

    #[test]
    fn status_never_returns_the_token_value_itself() {
        // L4-3 invariant: `vapor auth status` lists bound providers,
        // never the secret. We assert structurally — `AuthStatusEntry`
        // intentionally has no token field.
        let entry = AuthStatusEntry {
            profile: "default".to_string(),
            provider: "filesystem".to_string(),
            bound: true,
        };
        let debug = format!("{entry:?}");
        assert!(!debug.contains("token"));
    }
}
