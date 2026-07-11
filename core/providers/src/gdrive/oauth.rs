//! OAuth 2.0 PKCE helpers for the Google Drive provider.
//!
//! Pure helpers plus a transport-injected code exchange, so everything
//! short of the interactive browser hop is unit-testable offline. The
//! CLI drives the interactive flow (`vapor auth login gdrive`);
//! tokens live only in the profile-scoped `SecretStore` entry
//! (`docs/operations/provider-auth-operations.md`).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::{HttpRequest, HttpTransport};
use crate::{ProviderError, logging};

pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Full-Drive scope: Vapor syncs a user-chosen folder that other tools
/// may also write, which the per-app `drive.file` scope cannot see.
pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";

/// Stored token set (JSON in the secret store). The manual `Debug` below
/// redacts the token fields so a stray `{:?}` cannot bypass the logging
/// redaction layer and print live credentials.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredTokens {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "refreshToken", default)]
    pub refresh_token: Option<String>,
    /// Wall-clock expiry in UNIX millis; refreshed proactively 60s
    /// before.
    #[serde(rename = "expiresAtMs", default)]
    pub expires_at_ms: u64,
}

impl std::fmt::Debug for StoredTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Fills `dest` with cryptographically-secure random bytes from the OS
/// CSPRNG. Panics only if the OS entropy source is unavailable, which is
/// unrecoverable for a security-sensitive flow.
fn random_bytes(dest: &mut [u8]) {
    getrandom::fill(dest).expect("OS CSPRNG unavailable");
}

/// RFC 7636 code verifier: 32 CSPRNG octets encoded as 43 chars from the
/// unreserved base64url alphabet (RFC 7636 §4.1 requires a
/// cryptographically random verifier).
pub fn generate_code_verifier() -> String {
    let mut octets = [0_u8; 32];
    random_bytes(&mut octets);
    base64_url_no_pad(&octets)
}

/// CSPRNG `state` value for the authorization request (CSRF / flow-fixation
/// defense per OAuth Security BCP).
pub fn generate_state() -> String {
    let mut octets = [0_u8; 16];
    random_bytes(&mut octets);
    base64_url_no_pad(&octets)
}

/// `S256` code challenge: BASE64URL-no-pad(SHA256(verifier)).
pub fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64_url_no_pad(&digest)
}

/// The browser URL for the consent hop. `state` is echoed back on the
/// redirect and the loopback listener must reject any request whose state
/// does not match.
pub fn authorization_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    format!(
        "{AUTH_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
        url_encode(client_id),
        url_encode(redirect_uri),
        url_encode(DRIVE_SCOPE),
        url_encode(challenge),
        url_encode(state),
    )
}

#[derive(Debug, Deserialize)]
struct TokenEndpointResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Exchanges an authorization code for tokens (PKCE grant).
pub fn exchange_code(
    transport: &dyn HttpTransport,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    now_ms: u64,
) -> Result<StoredTokens, ProviderError> {
    let mut form = format!(
        "grant_type=authorization_code&client_id={}&code={}&code_verifier={}&redirect_uri={}",
        url_encode(client_id),
        url_encode(code),
        url_encode(verifier),
        url_encode(redirect_uri),
    );
    if let Some(secret) = client_secret {
        form.push_str(&format!("&client_secret={}", url_encode(secret)));
    }
    token_request(transport, form, now_ms)
}

/// Refreshes an access token (refresh handling). `invalid_grant`
/// classifies as `Authentication` (user action required); transport and
/// 5xx failures classify as `Transient`.
pub fn refresh_tokens(
    transport: &dyn HttpTransport,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
    now_ms: u64,
) -> Result<StoredTokens, ProviderError> {
    let mut form = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        url_encode(client_id),
        url_encode(refresh_token),
    );
    if let Some(secret) = client_secret {
        form.push_str(&format!("&client_secret={}", url_encode(secret)));
    }
    let mut tokens = token_request(transport, form, now_ms)?;
    // Google usually omits the refresh token on refresh; keep the old
    // one so the chain never breaks.
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

fn token_request(
    transport: &dyn HttpTransport,
    form: String,
    now_ms: u64,
) -> Result<StoredTokens, ProviderError> {
    let response = transport
        .execute(HttpRequest {
            method: "POST",
            url: TOKEN_URL.to_string(),
            headers: vec![(
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: form.into_bytes(),
        })
        .map_err(|error| {
            ProviderError::transient(format!("token endpoint unreachable: {}", error.message))
        })?;

    let parsed: TokenEndpointResponse = serde_json::from_slice(&response.body).map_err(|_| {
        ProviderError::transient(format!(
            "token endpoint returned unparsable body (status {})",
            response.status
        ))
    })?;
    if let Some(error) = parsed.error {
        logging::warning(
            "OAuth token request rejected",
            &[("oauth_error", error.clone())],
        );
        let description = parsed.error_description.unwrap_or_default();
        return Err(if error == "invalid_grant" || error == "invalid_client" {
            ProviderError::authentication(format!(
                "authorization is no longer valid ({error}): {description}; run `vapor auth login gdrive`"
            ))
        } else if response.status >= 500 {
            ProviderError::transient(format!("token endpoint failed: {error}"))
        } else {
            ProviderError::authentication(format!("token request rejected: {error}"))
        });
    }
    if parsed.access_token.is_empty() {
        return Err(ProviderError::transient(
            "token endpoint returned no access token",
        ));
    }
    Ok(StoredTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at_ms: now_ms + parsed.expires_in.saturating_mul(1_000),
    })
}

pub fn base64_url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        encoded.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        encoded.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        if chunk.len() > 1 {
            encoded.push(ALPHABET[(triple >> 6) as usize & 0x3F] as char);
        }
        if chunk.len() > 2 {
            encoded.push(ALPHABET[triple as usize & 0x3F] as char);
        }
    }
    encoded
}

pub fn url_encode(raw: &str) -> String {
    let mut encoded = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ScriptedHttpTransport;
    use vapor_shared::ProviderErrorKind;

    #[test]
    fn code_challenge_matches_the_rfc_7636_appendix_vector() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape_satisfies_pkce_requirements() {
        let verifier = generate_code_verifier();
        // 32 octets base64url-no-pad == 43 chars from the unreserved
        // alphabet (RFC 7636 §4.1).
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(verifier, generate_code_verifier());
    }

    #[test]
    fn authorization_url_carries_the_pkce_parameters() {
        let url = authorization_url(
            "client-123",
            "http://127.0.0.1:9999",
            "challenge-abc",
            "state-xyz",
        );
        assert!(url.starts_with(AUTH_URL));
        assert!(url.contains("code_challenge=challenge-abc"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("client_id=client-123"));
        assert!(url.contains("state=state-xyz"));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn exchange_parses_tokens_and_computes_expiry() {
        let transport = ScriptedHttpTransport::new();
        transport.push_response(
            200,
            r#"{"access_token":"ya29.abc","refresh_token":"1//rt","expires_in":3600}"#,
        );
        let tokens = exchange_code(
            &transport,
            "client",
            None,
            "code",
            "verifier",
            "http://127.0.0.1:1",
            1_000_000,
        )
        .expect("exchange");
        assert_eq!(tokens.access_token, "ya29.abc");
        assert_eq!(tokens.refresh_token.as_deref(), Some("1//rt"));
        assert_eq!(tokens.expires_at_ms, 1_000_000 + 3_600_000);

        // The request used the form grant.
        let requests = transport.recorded_requests();
        let body = String::from_utf8(requests[0].body.clone()).expect("utf8");
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code_verifier=verifier"));
    }

    #[test]
    fn invalid_grant_classifies_as_authentication() {
        let transport = ScriptedHttpTransport::new();
        transport.push_response(400, r#"{"error":"invalid_grant"}"#);
        let error = refresh_tokens(&transport, "client", None, "stale", 0)
            .expect_err("invalid grant must fail");
        assert_eq!(error.kind, ProviderErrorKind::Authentication);
        assert!(error.message.contains("vapor auth login"));
    }

    #[test]
    fn refresh_keeps_the_old_refresh_token_when_omitted() {
        let transport = ScriptedHttpTransport::new();
        transport.push_response(200, r#"{"access_token":"ya29.new","expires_in":100}"#);
        let tokens = refresh_tokens(&transport, "client", None, "1//old", 0).expect("refresh");
        assert_eq!(tokens.refresh_token.as_deref(), Some("1//old"));
    }

    #[test]
    fn transport_failures_classify_as_transient() {
        let transport = ScriptedHttpTransport::new();
        transport.push_transport_error("dns failure");
        let error = refresh_tokens(&transport, "client", None, "rt", 0).expect_err("must fail");
        assert_eq!(error.kind, ProviderErrorKind::Transient);
    }
}
