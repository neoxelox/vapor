//! Injectable blocking HTTP transport.
//!
//! Cloud providers perform their network I/O through this seam so the
//! whole provider stack is testable offline (`AGENTS.md §9.1`: never
//! contact the real Internet in tests). Production uses the
//! `ureq`-backed [`NativeHttpTransport`]; tests script a
//! [`ScriptedHttpTransport`].

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use vapor_shared::constants;

#[derive(Clone)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// Header names are matched case-insensitively by transports.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpRequest {
    // Manual impl so a stray `{request:?}` (error path, panic payload)
    // cannot leak an `Authorization: Bearer <token>` header or an OAuth
    // secret in the body — the logging redaction layer would not catch a
    // Debug payload on stderr.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let lower = name.to_ascii_lowercase();
                let redact = lower.contains("authorization")
                    || lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("cookie");
                (
                    name.as_str(),
                    if redact { "[REDACTED]" } else { value.as_str() },
                )
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Transport-level failure (DNS, TCP, TLS, timeouts). HTTP error
/// statuses are NOT transport errors — they come back as responses so
/// providers can classify them.
#[derive(Debug)]
pub struct HttpTransportError {
    pub message: String,
}

pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError>;
}

/// Production transport over `ureq` (blocking, rustls).
#[derive(Debug, Default)]
pub struct NativeHttpTransport;

/// Shared agent with explicit socket/connect timeouts. ureq's default
/// agent leaves read/write timeouts unset ("may block forever on reads"),
/// which on the synchronous provider stack would wedge the tick loop on a
/// stalled connection.
fn shared_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(
                constants::provider::HTTP_CONNECT_TIMEOUT_SECONDS,
            ))
            .timeout_read(Duration::from_secs(
                constants::provider::HTTP_SOCKET_TIMEOUT_SECONDS,
            ))
            .timeout_write(Duration::from_secs(
                constants::provider::HTTP_SOCKET_TIMEOUT_SECONDS,
            ))
            .build()
    })
}

impl HttpTransport for NativeHttpTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        let mut builder = shared_agent().request(request.method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.set(name, value);
        }
        let result = if request.body.is_empty() {
            builder.call()
        } else {
            builder.send_bytes(&request.body)
        };
        let response = match result {
            Ok(response) => response,
            // ureq returns HTTP >= 400 as Error::Status — surface those
            // as responses, not transport failures.
            Err(ureq::Error::Status(_code, response)) => response,
            Err(ureq::Error::Transport(transport)) => {
                return Err(HttpTransportError {
                    message: transport.to_string(),
                });
            }
        };

        let status = response.status();
        let headers: Vec<(String, String)> = response
            .headers_names()
            .into_iter()
            .filter_map(|name| {
                response
                    .header(&name)
                    .map(|value| (name.clone(), value.to_string()))
            })
            .collect();
        let mut body = Vec::new();
        use std::io::Read;
        let cap = constants::provider::MAX_HTTP_RESPONSE_BYTES;
        // Read one byte past the cap so we can distinguish "exactly the
        // cap" from "over the cap" and error instead of silently truncating
        // (a truncated body would otherwise complete a download as a
        // self-consistent-but-corrupt file).
        response
            .into_reader()
            .take(cap + 1)
            .read_to_end(&mut body)
            .map_err(|error| HttpTransportError {
                message: format!("cannot read response body: {error}"),
            })?;
        if body.len() as u64 > cap {
            return Err(HttpTransportError {
                message: format!("response body exceeded the {cap}-byte cap"),
            });
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Scripted transport for tests: responses are consumed in order, and
/// every request is recorded for assertions.
#[derive(Default)]
pub struct ScriptedHttpTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, String>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedHttpTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_response(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.push_response_with_headers(status, body, Vec::new());
    }

    pub fn push_response_with_headers(
        &self,
        status: u16,
        body: impl Into<Vec<u8>>,
        headers: Vec<(String, String)>,
    ) {
        self.responses
            .lock()
            .expect("scripted transport mutex")
            .push_back(Ok(HttpResponse {
                status,
                headers,
                body: body.into(),
            }));
    }

    pub fn push_transport_error(&self, message: impl Into<String>) {
        self.responses
            .lock()
            .expect("scripted transport mutex")
            .push_back(Err(message.into()));
    }

    pub fn recorded_requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .expect("scripted transport mutex")
            .clone()
    }
}

impl HttpTransport for ScriptedHttpTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        self.requests
            .lock()
            .expect("scripted transport mutex")
            .push(request);
        match self
            .responses
            .lock()
            .expect("scripted transport mutex")
            .pop_front()
        {
            Some(Ok(response)) => Ok(response),
            Some(Err(message)) => Err(HttpTransportError { message }),
            None => Err(HttpTransportError {
                message: "scripted transport ran out of responses".to_string(),
            }),
        }
    }
}
