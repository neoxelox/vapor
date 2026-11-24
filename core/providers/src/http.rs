//! Injectable blocking HTTP transport.
//!
//! Cloud providers perform their network I/O through this seam so the
//! whole provider stack is testable offline (`AGENTS.md §9.1`: never
//! contact the real Internet in tests). Production uses the
//! `ureq`-backed [`NativeHttpTransport`]; this crate's tests script a
//! `ScriptedHttpTransport`.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use vapor_platform::ProxySettings as _;
use vapor_shared::constants;

use crate::logging;

#[derive(Clone)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// Header names are matched case-insensitively by transports.
    pub headers: Vec<(String, String)>,
    /// Shared, so a request that is sent again is not copied.
    pub body: std::sync::Arc<Vec<u8>>,
}

impl std::fmt::Debug for HttpRequest {
    // Manual impl so a stray `{request:?}` (error path, panic payload)
    // cannot leak an `Authorization: Bearer <token>` header or an OAuth
    // secret in the body: the logging redaction layer would not catch a
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
/// statuses are NOT transport errors: they come back as responses so
/// providers can classify them.
#[derive(Debug)]
pub struct HttpTransportError {
    pub message: String,
}

pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError>;
}

/// Production transport over `ureq` (blocking, rustls).
///
/// It trusts the certificate authorities that ship with Vapor and the
/// ones the system trusts, so it works on a network that inspects TLS
/// with an authority of its own. It sends a request through the proxy
/// that the environment or the network settings of the system name
/// ([`ProxyRoute::of_this_host`]).
#[derive(Default)]
pub struct NativeHttpTransport {
    /// Agents of this transport alone, for a test that names its own
    /// proxy. `None` shares the agents of the process.
    own: Option<Arc<Agents>>,
}

impl std::fmt::Debug for NativeHttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHttpTransport")
            .finish_non_exhaustive()
    }
}

impl NativeHttpTransport {
    /// A transport whose proxy comes from `route`, read again as the
    /// shared one reads the settings of the host.
    #[cfg(test)]
    fn with_route(route: impl Fn() -> Option<ProxyRoute> + Send + Sync + 'static) -> Self {
        Self {
            own: Some(Arc::new(Agents::new(Box::new(route)))),
        }
    }

    fn agents(&self) -> &Agents {
        static SHARED: OnceLock<Agents> = OnceLock::new();
        match &self.own {
            Some(own) => own,
            None => SHARED.get_or_init(|| Agents::new(Box::new(ProxyRoute::of_this_host))),
        }
    }
}

/// The proxy that requests go through, and the hosts that go around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyRoute {
    /// `http://host:port`, with a user and a password when the setting
    /// has them. Never logged as it is.
    url: String,
    bypass: Vec<String>,
}

impl ProxyRoute {
    /// The route this host names. The environment wins: `HTTPS_PROXY`,
    /// then `ALL_PROXY` (each also in lower case), with `NO_PROXY` for
    /// the hosts that go around. That is how a server and a container
    /// say it. Without one, the network settings of the system decide,
    /// where the platform layer can read them.
    pub fn of_this_host() -> Option<Self> {
        let variable = |names: [&str; 2]| {
            names
                .into_iter()
                .filter_map(|name| std::env::var(name).ok())
                .map(|value| value.trim().to_string())
                .find(|value| !value.is_empty())
        };
        if let Some(proxy) = variable(["HTTPS_PROXY", "https_proxy"])
            .or_else(|| variable(["ALL_PROXY", "all_proxy"]))
        {
            let bypass = variable(["NO_PROXY", "no_proxy"])
                .map(|list| {
                    list.split(',')
                        .map(|rule| rule.trim().to_string())
                        .filter(|rule| !rule.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            return Some(Self::new(&proxy, bypass));
        }
        let system = vapor_platform::NativeProxySettings::for_current_host().https_proxy()?;
        Some(Self::new(
            &format!("{}:{}", system.host, system.port),
            system.bypass,
        ))
    }

    /// `address` is `host:port`, with or without a scheme in front.
    fn new(address: &str, bypass: Vec<String>) -> Self {
        let url = if address.contains("://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };
        Self { url, bypass }
    }

    fn applies_to(&self, host: &str) -> bool {
        !vapor_platform::proxy::bypasses(&self.bypass, host)
    }

    /// The address with no user and no password, for a log line.
    fn shown(&self) -> String {
        match (self.url.find("://"), self.url.rfind('@')) {
            (Some(scheme_end), Some(at)) if at > scheme_end => {
                format!("{}{}", &self.url[..scheme_end + 3], &self.url[at + 1..])
            }
            _ => self.url.clone(),
        }
    }
}

/// The agents of a transport: one that connects directly, and one for
/// the proxy that the route names now.
struct Agents {
    route: Box<dyn Fn() -> Option<ProxyRoute> + Send + Sync>,
    direct: ureq::Agent,
    proxied: Mutex<Proxied>,
}

struct Proxied {
    route: Option<ProxyRoute>,
    agent: Option<ureq::Agent>,
    /// When the route is read next.
    due: Instant,
}

impl Agents {
    fn new(route: Box<dyn Fn() -> Option<ProxyRoute> + Send + Sync>) -> Self {
        Self {
            route,
            direct: agent_builder().build(),
            proxied: Mutex::new(Proxied {
                route: None,
                agent: None,
                due: Instant::now(),
            }),
        }
    }

    /// The agent for a request to `host`. The route is read again each
    /// [`constants::provider::HTTP_PROXY_RECHECK_SECONDS`], so a device
    /// that joins or leaves a network with a proxy follows it.
    fn agent_for(&self, host: &str) -> ureq::Agent {
        let mut proxied = self
            .proxied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        if now >= proxied.due {
            proxied.due =
                now + Duration::from_secs(constants::provider::HTTP_PROXY_RECHECK_SECONDS);
            let route = (self.route)();
            if route != proxied.route {
                proxied.agent =
                    route
                        .as_ref()
                        .and_then(|route| match ureq::Proxy::new(&route.url) {
                            Ok(proxy) => {
                                logging::info(
                                    "Requests go through a proxy",
                                    &[("proxy", route.shown())],
                                );
                                Some(agent_builder().proxy(proxy).build())
                            }
                            Err(error) => {
                                logging::warning(
                                    "The proxy setting cannot be used; requests go directly",
                                    &[("proxy", route.shown()), ("error", error.to_string())],
                                );
                                None
                            }
                        });
                if route.is_none() && proxied.route.is_some() {
                    logging::info("Requests no longer go through a proxy", &[]);
                }
                proxied.route = route;
            }
        }
        match (&proxied.agent, &proxied.route) {
            (Some(agent), Some(route)) if route.applies_to(host) => agent.clone(),
            _ => self.direct.clone(),
        }
    }
}

/// An agent with explicit socket and connect timeouts, and the roots
/// of [`tls_config`]. ureq's default agent leaves read and write
/// timeouts unset ("may block forever on reads"), which on the
/// synchronous provider stack would hold a worker on a stalled
/// connection.
fn agent_builder() -> ureq::AgentBuilder {
    ureq::AgentBuilder::new()
        .tls_config(tls_config())
        .timeout_connect(Duration::from_secs(
            constants::provider::HTTP_CONNECT_TIMEOUT_SECONDS,
        ))
        .timeout_read(Duration::from_secs(
            constants::provider::HTTP_SOCKET_TIMEOUT_SECONDS,
        ))
        .timeout_write(Duration::from_secs(
            constants::provider::HTTP_SOCKET_TIMEOUT_SECONDS,
        ))
}

/// TLS for every agent: the certificate authorities that ship with
/// Vapor, and the ones the system trusts. A network that inspects TLS
/// signs each site with an authority of its own, which the
/// administrator of the device put in the trust store of the system;
/// with the shipped list alone every request there fails. The shipped
/// list stays, so a trust store that cannot be read costs nothing.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = trusted_roots();
            // The provider is named, as `ureq` names it: with no
            // process-wide default set, the plain builder would panic.
            let config = rustls::ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
            .expect("the ring provider has suites for TLS 1.2 and 1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

/// The shipped authorities, and with them the ones the system trusts.
fn trusted_roots() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let shipped = roots.len();
    let system = rustls_native_certs::load_native_certs();
    let (added, unusable) = roots.add_parsable_certificates(system.certs);
    logging::debug(
        "Loaded the certificate authorities of the HTTP transport",
        &[
            ("shipped", shipped.to_string()),
            ("from_the_system", added.to_string()),
            ("unusable", unusable.to_string()),
            ("read_errors", system.errors.len().to_string()),
        ],
    );
    roots
}

/// The host of `url`: what a bypass rule is matched on.
fn host_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or(host)
}

impl HttpTransport for NativeHttpTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        let agent = self.agents().agent_for(host_of(&request.url));
        let mut builder = agent.request(request.method, &request.url);
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
            // ureq returns HTTP >= 400 as Error::Status: surface those
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

#[cfg(test)]
mod tests {
    use super::*;

    fn route(address: &str, bypass: &[&str]) -> ProxyRoute {
        ProxyRoute::new(
            address,
            bypass.iter().map(|rule| rule.to_string()).collect(),
        )
    }

    #[test]
    fn a_request_goes_through_the_proxy_that_the_route_names() {
        use std::io::{BufRead, BufReader, Write};
        // A stand-in for a proxy: it reads the first line of what
        // reaches it and refuses the tunnel.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let proxy = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut first_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut first_line)
                .expect("request line");
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            first_line
        });
        let transport = NativeHttpTransport::with_route(move || {
            Some(route(&address.to_string(), &["*.local"]))
        });
        let refused = transport.execute(HttpRequest {
            method: "GET",
            url: "https://www.googleapis.com/drive/v3/about".to_string(),
            headers: Vec::new(),
            body: Arc::new(Vec::new()),
        });
        assert!(refused.is_err(), "the stand-in refuses the tunnel");
        assert_eq!(
            proxy.join().expect("proxy thread").trim(),
            "CONNECT www.googleapis.com:443 HTTP/1.1",
            "the request asked the proxy for a tunnel to the host"
        );
    }

    #[test]
    fn a_host_that_a_bypass_rule_names_goes_around_the_proxy() {
        let agents = Agents::new(Box::new(|| {
            Some(route("proxy.example:3128", &["*.local", "direct.example"]))
        }));
        // Same agent as the direct one means no proxy on the way.
        let goes_direct = |host: &str| {
            let proxied = agents.proxied.lock().expect("proxied");
            proxied
                .route
                .as_ref()
                .is_none_or(|route| !route.applies_to(host))
        };
        let _ = agents.agent_for("www.googleapis.com");
        assert!(!goes_direct("www.googleapis.com"));
        assert!(goes_direct("nas.local"));
        assert!(goes_direct("direct.example"));
    }

    #[test]
    fn a_proxy_address_is_shown_with_no_user_and_no_password() {
        assert_eq!(
            route("http://sam:s3cret@proxy.example:3128", &[]).shown(),
            "http://proxy.example:3128"
        );
        assert_eq!(
            route("proxy.example:3128", &[]).shown(),
            "http://proxy.example:3128"
        );
    }

    #[test]
    fn the_host_of_a_url_is_what_a_bypass_rule_is_matched_on() {
        for (url, host) in [
            (
                "https://www.googleapis.com/drive/v3/files?q=x",
                "www.googleapis.com",
            ),
            (
                "https://oauth2.googleapis.com:443/token",
                "oauth2.googleapis.com",
            ),
            ("http://user:pass@proxy.example:3128", "proxy.example"),
            ("https://host.example", "host.example"),
        ] {
            assert_eq!(host_of(url), host, "{url}");
        }
    }

    #[test]
    fn the_transport_trusts_the_shipped_authorities_whatever_the_system_holds() {
        // The system store adds to the list and never replaces it: a
        // host whose store cannot be read still reaches the cloud.
        assert!(trusted_roots().len() >= webpki_roots::TLS_SERVER_ROOTS.len());
    }
}

#[cfg(test)]
pub use scripted::ScriptedHttpTransport;

/// The scripted transport, a test double: compiled for this crate's
/// tests only, so the shipped library carries no fake.
#[cfg(test)]
mod scripted {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{HttpRequest, HttpResponse, HttpTransport, HttpTransportError};

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
}
