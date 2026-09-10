//! `vapor auth login|logout|status`.
//!
//! SecretStore-backed auth plumbing. For Google Drive, `login` runs an
//! OAuth-PKCE browser flow when `--token` is omitted; otherwise `login`
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
        /// The resolved token value (from `--token`, stdin, or the
        /// OAuth-PKCE flow for `gdrive`).
        token: String,
        /// Credentials are namespaced per profile; omitting
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
    /// The supported provider names are documented inline.
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
    /// expose the token value itself.
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

/// Production constructor for the native secret store (the login
/// keychain on macOS, the Secret Service or the command shim on
/// Linux). When the host has no usable store it falls back to the
/// in-process [`InMemorySecretStore`] and returns why, so the caller
/// can warn the user with the way out.
pub fn build_native_store() -> (Box<dyn SecretStore>, Option<String>) {
    match NativeSecretStore::for_current_user() {
        Ok(store) => (Box::new(store), None),
        Err(error) => (
            Box::new(InMemorySecretStore::new()),
            Some(error.to_string()),
        ),
    }
}

/// Interactive OAuth-PKCE login for Google Drive: a loopback
/// redirect listener plus the system browser. Blocking by design — the
/// CLI waits for the consent hop. Returns the stored-token JSON that
/// goes into the secret store.
pub fn run_gdrive_pkce_flow() -> Result<String, String> {
    use std::io::{BufRead, BufReader};
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
    let state = oauth::generate_state();
    let url = oauth::authorization_url(&client_id, &redirect_uri, &challenge, &state);

    eprintln!("Open this URL in your browser to authorize Vapor:");
    eprintln!("  {url}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    eprintln!("Waiting for the authorization redirect on {redirect_uri} ...");

    // Loop on accept with a per-connection read timeout and an overall
    // deadline: a stray/speculative connection (browser preconnect, a
    // local probe) must not consume the one accept and kill the login, and
    // a request whose `state` does not match ours is ignored. Only a
    // request carrying our state and a code completes the flow.
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot configure the redirect listener: {error}"))?;
    let overall_deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let code = loop {
        if std::time::Instant::now() >= overall_deadline {
            return Err(
                "timed out waiting for the authorization redirect; rerun `vapor auth login gdrive`"
                    .to_string(),
            );
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(format!("redirect listener failed: {error}")),
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        // A connection that sends nothing (or times out) is a stray probe;
        // discard it and keep waiting for the real redirect.
        if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
            continue;
        }
        // GET /?code=...&state=... HTTP/1.1
        let query = request_line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split('?').nth(1))
            .unwrap_or("");
        // Reject any request whose state does not match ours.
        if query_param(query, "state").as_deref() != Some(state.as_str()) {
            let mut stream = stream;
            let _ = write_redirect_response(&mut stream, "This request was not recognized.");
            continue;
        }
        if let Some(error) = query_param(query, "error") {
            let mut stream = stream;
            let _ = write_redirect_response(&mut stream, "Authorization was denied.");
            return Err(format!("authorization was denied by the user: {error}"));
        }
        // Google authorization codes contain '/' ("4/0A..."), which arrives
        // percent-encoded; decode before exchange (re-encoding a still-
        // encoded value yields invalid_grant).
        if let Some(code) = query_param(query, "code").filter(|code| !code.is_empty()) {
            let mut stream = stream;
            let _ = write_redirect_response(
                &mut stream,
                "Vapor is authorized. You can close this tab.",
            );
            break code;
        }
    };

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

/// Returns the percent-decoded value of `key` from a URL query string.
fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| percent_decode(value))
}

/// Writes a minimal HTML response body to the redirect connection.
fn write_redirect_response(stream: &mut std::net::TcpStream, message: &str) -> std::io::Result<()> {
    use std::io::Write;
    let body = format!("<html><body>{message}</body></html>");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Decodes `application/x-www-form-urlencoded` query values (`%XX` and
/// `+`). An incomplete/invalid escape is passed through literally.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = (bytes[index + 1] as char).to_digit(16);
                let lo = (bytes[index + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    index += 3;
                    continue;
                }
                out.push(b'%');
                index += 1;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_restores_reserved_characters_in_auth_codes() {
        // A Google authorization code arrives percent-encoded.
        assert_eq!(percent_decode("4%2F0Axyz"), "4/0Axyz");
        assert_eq!(percent_decode("a+b%20c"), "a b c");
        // Malformed escapes pass through literally.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

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
        // A token bound to one profile must be invisible to
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
        // Invariant: `vapor auth status` lists bound providers,
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
