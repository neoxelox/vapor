//! Injectable blocking HTTP transport.
//!
//! Cloud providers perform their network I/O through this seam so the
//! whole provider stack is testable offline (`AGENTS.md §9.1`: never
//! contact the real Internet in tests). Production uses the
//! `ureq`-backed [`NativeHttpTransport`]; tests script a
//! [`ScriptedHttpTransport`].

use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// Header names are matched case-insensitively by transports.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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

impl HttpTransport for NativeHttpTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        let mut builder = ureq::request(request.method, &request.url);
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
        response
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|error| HttpTransportError {
                message: format!("cannot read response body: {error}"),
            })?;
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
