//! `vapor auth login|logout|status`.
//!
//! A profile's provider decides what signing in means: Google Drive runs
//! an OAuth-PKCE browser flow (or reads a token for a headless host), the
//! filesystem provider needs no sign-in. Tokens live in the platform
//! secret store under the profile's namespace, never in `vapor.json`.

use std::error::Error;
use std::fmt::{self, Display};

use serde::Serialize;
use vapor_daemon::profiles::{ResolvedProfile, resolve_profiles};
use vapor_platform::{InMemorySecretStore, SecretStore, SecretStoreError};
use vapor_shared::config::VaporConfig;
use vapor_shared::constants;

#[derive(Debug)]
pub enum AuthError {
    /// No profile has this id.
    UnknownProfile {
        requested: String,
        known: Vec<String>,
    },
    /// Several profiles exist and none was named.
    AmbiguousProfile(Vec<String>),
    /// No profile exists yet: there is nothing to sign in for.
    NoProfile,
    /// The profile's provider has nothing to sign in to.
    NoSignIn {
        profile: String,
        provider: String,
    },
    Store(SecretStoreError),
}

impl Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProfile { requested, known } => write!(
                f,
                "no profile '{requested}' (profiles: {})",
                known.join(", ")
            ),
            Self::NoProfile => write!(f, "no sync profile yet; set Vapor up with `vapor init`"),
            Self::AmbiguousProfile(known) => write!(
                f,
                "several profiles exist; pass --profile <id> ({})",
                known.join(", ")
            ),
            Self::NoSignIn { profile, provider } => write!(
                f,
                "profile '{profile}' uses the {provider} provider, which needs no sign-in"
            ),
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

fn key(profile_id: &str, provider: &str) -> String {
    vapor_daemon::profiles::secret_key(profile_id, provider, "token")
}

fn needs_sign_in(provider: &str) -> bool {
    constants::provider::SIGN_IN_REQUIRED.contains(&provider)
}

/// The profile a command that acts on exactly one profile targets:
/// `--profile` when given, otherwise the only profile there is. With
/// several profiles and no `--profile`, the command refuses and lists
/// them rather than guessing.
pub fn select_profile(
    config: &VaporConfig,
    requested: Option<&str>,
) -> Result<ResolvedProfile, AuthError> {
    let profiles = resolve_profiles(config);
    let ids = || profiles.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    match requested {
        Some(id) => profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AuthError::UnknownProfile {
                requested: id.to_string(),
                known: ids(),
            }),
        None => match profiles.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(AuthError::NoProfile),
            _ => Err(AuthError::AmbiguousProfile(ids())),
        },
    }
}

/// The profile id a sign-out acts on. It is [`select_profile`], with
/// one more case: an install with no profile can hold the sign-in that
/// `vapor init` made before it wrote one, under the id of the first
/// profile, and a sign-out removes that.
pub fn logout_target(config: &VaporConfig, requested: Option<&str>) -> Result<String, AuthError> {
    match select_profile(config, requested) {
        Ok(profile) => Ok(profile.id),
        Err(AuthError::NoProfile) => Ok(constants::profile::DEFAULT_PROFILE_ID.to_string()),
        Err(error) => Err(error),
    }
}

/// Whether the profile's provider needs a sign-in, for `login` to refuse
/// early instead of storing a credential nothing reads.
pub fn require_sign_in(profile: &ResolvedProfile) -> Result<(), AuthError> {
    if needs_sign_in(&profile.provider_kind) {
        Ok(())
    } else {
        Err(AuthError::NoSignIn {
            profile: profile.id.clone(),
            provider: profile.provider_kind.clone(),
        })
    }
}

pub fn login_into(
    store: &dyn SecretStore,
    profile: &ResolvedProfile,
    token: &str,
) -> Result<(), AuthError> {
    require_sign_in(profile)?;
    store_sign_in(store, &profile.id, &profile.provider_kind, token)
}

/// Stores a sign-in under a profile id and a provider. `vapor init`
/// signs in before the profile exists, under the id the profile gets,
/// so the folder step can look into the account.
pub fn store_sign_in(
    store: &dyn SecretStore,
    profile_id: &str,
    provider: &str,
    token: &str,
) -> Result<(), AuthError> {
    store.set(&key(profile_id, provider), token)?;
    Ok(())
}

/// Whether a sign-in is stored for this profile id and provider. The
/// store is asked for names only; no secret is read.
pub fn signed_in(
    store: &dyn SecretStore,
    profile_id: &str,
    provider: &str,
) -> Result<bool, AuthError> {
    Ok(stored_names(store)?.contains(&key(profile_id, provider)))
}

/// Removes the sign-ins a profile id holds for every provider but
/// `keep`: what a sign-in made before the profile existed leaves behind
/// when the profile is then written for another provider.
pub fn forget_other_providers(
    store: &dyn SecretStore,
    profile_id: &str,
    keep: &str,
) -> Result<(), AuthError> {
    let stored = stored_names(store)?;
    for provider in constants::provider::ALL {
        let name = key(profile_id, provider);
        if *provider == keep || !stored.contains(&name) {
            continue;
        }
        match store.delete(&name) {
            Ok(()) | Err(SecretStoreError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// The names the store holds. The store answers this from the items'
/// attributes: no secret is read, so no keychain dialog opens.
fn stored_names(store: &dyn SecretStore) -> Result<Vec<String>, AuthError> {
    Ok(store.list()?)
}

/// Removes every credential the profile holds, for any provider, so a
/// profile that switched providers leaves nothing behind. Returns whether
/// anything was removed. A credential that is there and cannot be
/// removed is an error, never "was not signed in".
pub fn logout_from(store: &dyn SecretStore, profile_id: &str) -> Result<bool, AuthError> {
    let stored = stored_names(store)?;
    let mut removed = false;
    for provider in constants::provider::ALL {
        let name = key(profile_id, provider);
        if !stored.contains(&name) {
            continue;
        }
        match store.delete(&name) {
            Ok(()) | Err(SecretStoreError::NotFound(_)) => removed = true,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

/// One profile's sign-in state; never the token itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatusEntry {
    pub profile_id: String,
    pub provider: String,
    /// One of `constants::auth::STATE_*`.
    pub state: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatusReport {
    pub profiles: Vec<AuthStatusEntry>,
}

/// The sign-in state of `profile`, or of every profile when `None`.
pub fn status_from(
    store: &dyn SecretStore,
    config: &VaporConfig,
    profile: Option<&str>,
) -> Result<AuthStatusReport, AuthError> {
    let profiles = match profile {
        Some(id) => vec![select_profile(config, Some(id))?],
        None => resolve_profiles(config),
    };
    // Whether a credential is stored, and never the credential: the
    // store is asked for names only.
    let stored = if profiles
        .iter()
        .any(|profile| needs_sign_in(&profile.provider_kind))
    {
        stored_names(store)?
    } else {
        Vec::new()
    };
    let entries = profiles
        .into_iter()
        .map(|profile| {
            let state = if !needs_sign_in(&profile.provider_kind) {
                constants::auth::STATE_NOT_NEEDED
            } else if stored.contains(&key(&profile.id, &profile.provider_kind)) {
                constants::auth::STATE_SIGNED_IN
            } else {
                constants::auth::STATE_SIGNED_OUT
            };
            AuthStatusEntry {
                profile_id: profile.id,
                provider: profile.provider_kind,
                state,
            }
        })
        .collect();
    Ok(AuthStatusReport { profiles: entries })
}

pub fn render_status(report: &AuthStatusReport) -> String {
    report
        .profiles
        .iter()
        .map(|entry| format!("{} ({}): {}", entry.profile_id, entry.provider, entry.state))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Production constructor for the native secret store (the login
/// keychain on macOS, the Secret Service or the command shim on
/// Linux). When the host has no usable store it falls back to the
/// in-process [`InMemorySecretStore`] and returns why, so the caller
/// can warn the user with the way out.
pub fn build_native_store() -> (Box<dyn SecretStore>, Option<String>) {
    match vapor_platform::open_store(vapor_platform::SecretAccess::Interactive) {
        Ok(store) => (store, None),
        Err(error) => (
            Box::new(InMemorySecretStore::new()),
            Some(error.to_string()),
        ),
    }
}

/// Interactive OAuth-PKCE login for Google Drive: a loopback
/// redirect listener plus the system browser. Blocking by design: the
/// CLI waits for the consent hop. Returns the stored-token JSON that
/// goes into the secret store.
pub fn run_gdrive_pkce_flow() -> Result<String, String> {
    use std::io::{BufRead, BufReader};
    use vapor_providers::gdrive::oauth;

    let oauth::ClientCredentials {
        client_id,
        client_secret,
        ..
    } = oauth::client_credentials().ok_or_else(|| {
        format!(
            "this build has no Google Drive OAuth client and {} is not set. Create an OAuth client (Desktop app) in the Google Cloud console and export it, or build with it set; see docs/operations/provider-auth-operations.md",
            vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID
        )
    })?;

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
    let overall_deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(
            vapor_shared::constants::provider::BROWSER_SIGN_IN_TIMEOUT_SECONDS,
        );
    let code = loop {
        if std::time::Instant::now() >= overall_deadline {
            return Err(
                "timed out waiting for the authorization redirect; rerun `vapor auth login`"
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

    fn config_with(profiles: &[(&str, &str)]) -> VaporConfig {
        VaporConfig {
            profiles: profiles
                .iter()
                .map(|(id, provider)| vapor_shared::config::ProfileConfig {
                    id: id.to_string(),
                    provider: Some(provider.to_string()),
                    local_root: Some(format!("/tmp/vapor-auth-test/{id}")),
                    cloud_root: Some(format!("/{id}")),
                    ..Default::default()
                })
                .collect(),
            ..VaporConfig::default()
        }
    }

    /// The implicit profile: the two top-level roots, set.
    fn configured_default() -> VaporConfig {
        VaporConfig {
            local_root: Some("/tmp/vapor-auth-test/local".to_string()),
            cloud_root: Some("/tmp/vapor-auth-test/cloud".to_string()),
            ..VaporConfig::default()
        }
    }

    #[test]
    fn the_only_profile_is_selected_without_a_flag() {
        let config = configured_default();
        let profile = select_profile(&config, None).expect("single profile");
        assert_eq!(profile.id, constants::profile::DEFAULT_PROFILE_ID);
    }

    #[test]
    fn an_install_with_no_profile_is_told_how_to_add_one() {
        let error = select_profile(&VaporConfig::default(), None).expect_err("no profile");
        assert!(matches!(error, AuthError::NoProfile));
        assert!(error.to_string().contains("vapor init"));
    }

    #[test]
    fn a_sign_out_with_no_profile_removes_the_sign_in_made_before_one() {
        let store = InMemorySecretStore::new();
        store_sign_in(&store, "default", "gdrive", "ya29.test").expect("store");
        assert!(signed_in(&store, "default", "gdrive").expect("signed in"));

        let target = logout_target(&VaporConfig::default(), None).expect("target");
        assert_eq!(target, constants::profile::DEFAULT_PROFILE_ID);
        assert!(logout_from(&store, &target).expect("logout"));
        assert!(!signed_in(&store, "default", "gdrive").expect("signed out"));

        // With a profile it is the profile that is signed out.
        let config = config_with(&[("work", "gdrive")]);
        assert_eq!(logout_target(&config, None).expect("target"), "work");
    }

    #[test]
    fn a_profile_keeps_only_the_sign_in_of_its_provider() {
        let store = InMemorySecretStore::new();
        for provider in constants::provider::ALL {
            store_sign_in(&store, "default", provider, "ya29.test").expect("store");
        }
        forget_other_providers(&store, "default", constants::provider::GDRIVE).expect("forget");
        assert!(signed_in(&store, "default", constants::provider::GDRIVE).expect("kept"));
        assert!(!signed_in(&store, "default", constants::provider::FILESYSTEM).expect("gone"));
    }

    #[test]
    fn several_profiles_need_an_explicit_profile() {
        let config = config_with(&[("work", "gdrive"), ("home", "gdrive")]);
        let error = select_profile(&config, None).expect_err("ambiguous");
        assert!(matches!(error, AuthError::AmbiguousProfile(ref ids) if ids.len() == 2));
        let error = select_profile(&config, Some("play")).expect_err("unknown");
        assert!(matches!(error, AuthError::UnknownProfile { .. }));
        assert_eq!(
            select_profile(&config, Some("home")).expect("named").id,
            "home"
        );
    }

    #[test]
    fn status_json_shape_is_locked() {
        let report = AuthStatusReport {
            profiles: vec![AuthStatusEntry {
                profile_id: "work".into(),
                provider: "gdrive".into(),
                state: constants::auth::STATE_SIGNED_IN,
            }],
        };
        assert_eq!(
            serde_json::to_string(&report).expect("serialize"),
            r#"{"profiles":[{"profileId":"work","provider":"gdrive","state":"signed-in"}]}"#
        );
    }

    #[test]
    fn login_uses_the_profile_provider_and_status_reports_it() {
        let store = InMemorySecretStore::new();
        let config = config_with(&[("work", "gdrive"), ("local", "filesystem")]);
        let work = select_profile(&config, Some("work")).expect("work");
        login_into(&store, &work, "ya29.work").expect("login");

        let report = status_from(&store, &config, None).expect("status");
        let states: Vec<_> = report
            .profiles
            .iter()
            .map(|entry| {
                (
                    entry.profile_id.as_str(),
                    entry.provider.as_str(),
                    entry.state,
                )
            })
            .collect();
        assert_eq!(
            states,
            vec![
                ("work", "gdrive", constants::auth::STATE_SIGNED_IN),
                ("local", "filesystem", constants::auth::STATE_NOT_NEEDED),
            ]
        );
    }

    #[test]
    fn a_provider_without_sign_in_refuses_login() {
        let store = InMemorySecretStore::new();
        let profile = select_profile(&configured_default(), None).expect("default");
        let error = login_into(&store, &profile, "x").expect_err("filesystem needs no sign-in");
        assert!(matches!(error, AuthError::NoSignIn { .. }));
    }

    #[test]
    fn logout_removes_every_credential_of_the_profile_only() {
        let store = InMemorySecretStore::new();
        let config = config_with(&[("work", "gdrive"), ("home", "gdrive")]);
        for id in ["work", "home"] {
            let profile = select_profile(&config, Some(id)).expect("profile");
            login_into(&store, &profile, "token").expect("login");
        }
        assert!(logout_from(&store, "work").expect("logout"));
        assert!(!logout_from(&store, "work").expect("second logout"));

        let report = status_from(&store, &config, None).expect("status");
        let signed_in: Vec<_> = report
            .profiles
            .iter()
            .filter(|entry| entry.state == constants::auth::STATE_SIGNED_IN)
            .map(|entry| entry.profile_id.as_str())
            .collect();
        assert_eq!(signed_in, vec!["home"]);
    }

    /// A store that holds credentials and refuses to give or remove
    /// them, the way a keychain does when it would have to ask.
    struct Guarded {
        inner: InMemorySecretStore,
        refuse_delete: bool,
    }

    impl SecretStore for Guarded {
        fn get(&self, name: &str) -> Result<String, SecretStoreError> {
            panic!("the secret {name} was read; status and logout ask for names only")
        }
        fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
            self.inner.set(name, value)
        }
        fn delete(&self, name: &str) -> Result<(), SecretStoreError> {
            if self.refuse_delete {
                return Err(SecretStoreError::Backend("the keychain refused".into()));
            }
            self.inner.delete(name)
        }
        fn list(&self) -> Result<Vec<String>, SecretStoreError> {
            self.inner.list()
        }
    }

    #[test]
    fn status_and_logout_never_read_a_secret_and_a_refused_logout_is_an_error() {
        let config = config_with(&[("work", "gdrive")]);
        let work = select_profile(&config, Some("work")).expect("work");
        let mut store = Guarded {
            inner: InMemorySecretStore::new(),
            refuse_delete: true,
        };
        login_into(&store, &work, "ya29.test").expect("login");

        let report = status_from(&store, &config, None).expect("status");
        assert_eq!(report.profiles[0].state, constants::auth::STATE_SIGNED_IN);

        // The credential is there and cannot be removed: that is not
        // "was not signed in".
        let refused = logout_from(&store, "work").expect_err("the store refused");
        assert!(refused.to_string().contains("refused"), "{refused}");
        store.refuse_delete = false;
        assert!(logout_from(&store, "work").expect("logout"));
        let report = status_from(&store, &config, None).expect("status");
        assert_eq!(report.profiles[0].state, constants::auth::STATE_SIGNED_OUT);
    }

    #[test]
    fn status_never_carries_the_token() {
        let store = InMemorySecretStore::new();
        let config = config_with(&[("work", "gdrive")]);
        let work = select_profile(&config, Some("work")).expect("work");
        login_into(&store, &work, "ya29.secret-value").expect("login");
        let report = status_from(&store, &config, None).expect("status");
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(!json.contains("ya29"), "{json}");
    }
}
