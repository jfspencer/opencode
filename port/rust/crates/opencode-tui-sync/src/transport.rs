//! What the client sends requests through: `fetch`. [`Transport`] is the seam (the harness drives the client
//! through a scripted one); [`HttpTransport`] is Bun's `fetch` as the TUI uses it (`noTimeoutFetch`).
//!
//! The HTTP transport is a copy of the opencode crate's `llm/http.rs` (HTTP/1.1 over hyper, TLS over rustls with
//! the platform's roots and `SSL_CERT_FILE`, `HTTP(S)_PROXY` and `NO_PROXY` honored, one connection per request)
//! with Bun's error texts for failures below HTTP (the CLI's `cli/client.rs` `fetch_message`). The opencode crate
//! is a binary, so it cannot be shared yet.
//!
//! Like `fetch`, [`Transport::fetch`] starts the request at once: the returned future only waits for the response
//! head. Dropping the future or the response body closes the connection (an aborted `fetch`).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::abort::Reason;

pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A request as `fetch(url, init)` receives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: String,
    pub url: String,
    /// Names lowercased (a `Headers` object), in insertion order.
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A response head and its body.
pub struct Response {
    pub status: u16,
    /// Names lowercased, in arrival order.
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn ResponseBody>,
}

impl Response {
    /// `response.headers.get(name)`: the values of every header of that name, joined with ", ".
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> =
            self.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
        if values.is_empty() { None } else { Some(values.join(", ")) }
    }
}

/// A response body read chunk by chunk (`response.body.getReader()`); dropping it cancels the body.
pub trait ResponseBody {
    /// The next chunk; `None` at the end; an error when the connection fails while reading.
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<Bytes, Reason>>>;
}

/// `fetch`. The request starts when `fetch` is called, not when its future is first polled.
pub trait Transport {
    fn fetch(&self, request: Request) -> LocalBoxFuture<'static, Result<Response, Reason>>;
}

/// Bun's `fetch` over the network.
#[derive(Clone, Copy, Debug, Default)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    fn fetch(&self, request: Request) -> LocalBoxFuture<'static, Result<Response, Reason>> {
        let task = tokio::spawn(async move {
            send(&request.method, &request.url, &request.headers, request.body.unwrap_or_default()).await
        });
        let abort = AbortOnDrop(task.abort_handle());
        Box::pin(async move {
            let result = task.await;
            drop(abort);
            match result {
                Ok(Ok(response)) => Ok(Response {
                    status: response.status,
                    headers: response.headers,
                    body: Box::new(HttpBody(response.body)),
                }),
                Ok(Err(failure)) => Err(fetch_error(&failure)),
                Err(_) => Err(Reason::abort_error()),
            }
        })
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct HttpBody(Body);

impl ResponseBody for HttpBody {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<Bytes, Reason>>> {
        Box::pin(async move { self.0.next().await.map(|chunk| chunk.map_err(|failure| fetch_error(&failure))) })
    }
}

/// Bun's `fetch` error for a failure below HTTP: a `TypeError` with Bun's words.
pub fn fetch_error(failure: &Failure) -> Reason {
    let message = match failure.kind {
        FailureKind::Refused => "Unable to connect. Is the computer able to access the url?".to_owned(),
        FailureKind::Unresolved => "Was there a typo in the url or port?".to_owned(),
        FailureKind::Closed => "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()".to_owned(),
        _ => failure.message.clone(),
    };
    Reason { name: "TypeError".into(), message }
}

// ---- the HTTP client (llm/http.rs) ------------------------------------------------------------------------

/// A transport failure below HTTP: what failed and the error's own words.
#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The connection could not be opened (refused, unreachable).
    Refused,
    /// The host name did not resolve.
    Unresolved,
    /// The connection ended before or while the response was read.
    Closed,
    /// TLS handshake or certificate failure.
    Tls,
    Other,
}

impl Failure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

struct Body {
    incoming: Incoming,
    _connection: tokio::task::JoinHandle<()>,
}

impl Body {
    async fn next(&mut self) -> Option<Result<Bytes, Failure>> {
        loop {
            match self.incoming.frame().await? {
                Ok(frame) => match frame.into_data() {
                    Ok(data) if data.is_empty() => continue,
                    Ok(data) => return Some(Ok(data)),
                    Err(_) => continue,
                },
                // Whatever ends the body early is the connection going away.
                Err(error) => return Some(Err(Failure::new(FailureKind::Closed, error.to_string()))),
            }
        }
    }
}

impl Drop for Body {
    /// An abandoned response closes its connection.
    fn drop(&mut self) {
        self._connection.abort();
    }
}

fn hyper_failure(error: &hyper::Error) -> Failure {
    if error.is_incomplete_message() || error.is_closed() || error.is_canceled() {
        return Failure::new(FailureKind::Closed, error.to_string());
    }
    let text = error.to_string();
    if text.contains("reset") || text.contains("closed") || text.contains("EOF") || text.contains("end of file") {
        return Failure::new(FailureKind::Closed, text);
    }
    Failure::new(FailureKind::Other, text)
}

fn io_failure(error: &std::io::Error) -> Failure {
    use std::io::ErrorKind::*;
    let kind = match error.kind() {
        ConnectionRefused | HostUnreachable | NetworkUnreachable | AddrNotAvailable => FailureKind::Refused,
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof => FailureKind::Closed,
        _ => FailureKind::Other,
    };
    Failure::new(kind, error.to_string())
}

struct Target {
    secure: bool,
    host: String,
    port: u16,
    /// Path and query.
    path: String,
}

fn parse(url: &str) -> Option<Target> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    // Credentials in the URL are not sent as such: the client passes them as an `authorization` header.
    let authority = authority.rsplit_once('@').map(|(_, host)| host).unwrap_or(authority);
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']')?;
        let host = &stripped[..end];
        let port = stripped[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok());
        (host.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse().ok()),
            None => (authority.to_owned(), None),
        }
    };
    Some(Target { secure, port: port.unwrap_or(if secure { 443 } else { 80 }), host, path: path.to_owned() })
}

fn env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

/// Whether `NO_PROXY` exempts the host (`*`, a name, a `.suffix` or a bare suffix).
fn bypass(host: &str) -> bool {
    let Some(list) = env(&["NO_PROXY", "no_proxy"]) else { return false };
    let host = host.to_ascii_lowercase();
    list.split(',').map(|entry| entry.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|entry| {
        if entry == "*" {
            return true;
        }
        let entry = entry.split(':').next().unwrap_or_default().to_owned();
        let entry = entry.trim_start_matches('.');
        host == entry || host.ends_with(&format!(".{entry}"))
    })
}

fn proxy(target: &Target) -> Option<Target> {
    if bypass(&target.host) {
        return None;
    }
    let value = if target.secure {
        env(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"])
    } else {
        env(&["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"])
    }?;
    let value = if value.contains("://") { value } else { format!("http://{value}") };
    parse(&value)
}

fn tls() -> &'static Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let found = rustls_native_certs::load_native_certs();
        roots.add_parsable_certificates(found.certs);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    })
}

async fn connect(host: &str, port: u16) -> Result<TcpStream, Failure> {
    let addresses = match tokio::net::lookup_host((host, port)).await {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => return Err(Failure::new(FailureKind::Unresolved, error.to_string())),
    };
    if addresses.is_empty() {
        return Err(Failure::new(FailureKind::Unresolved, format!("no address for {host}")));
    }
    let mut last = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => last = Some(io_failure(&error)),
        }
    }
    Err(last.unwrap_or_else(|| Failure::new(FailureKind::Refused, "connection failed")))
}

/// Opens a `CONNECT` tunnel through the proxy.
async fn tunnel(mut stream: TcpStream, host: &str, port: u16) -> Result<TcpStream, Failure> {
    let authority = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(head.as_bytes()).await.map_err(|e| io_failure(&e))?;
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    // Byte by byte: what follows the head belongs to the tunnel.
    while !response.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte).await {
            Ok(0) => return Err(Failure::new(FailureKind::Closed, "proxy closed the connection")),
            Ok(_) => response.push(byte[0]),
            Err(error) => return Err(io_failure(&error)),
        }
        if response.len() > 16 * 1024 {
            return Err(Failure::new(FailureKind::Other, "proxy response too long"));
        }
    }
    let line = String::from_utf8_lossy(&response);
    let status = line.split_whitespace().nth(1).unwrap_or_default();
    if status != "200" {
        return Err(Failure::new(
            FailureKind::Refused,
            format!("proxy refused the tunnel: {}", line.lines().next().unwrap_or_default()),
        ));
    }
    Ok(stream)
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;

async fn handshake<I>(io: I) -> Result<(Sender, tokio::task::JoinHandle<()>), Failure>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| hyper_failure(&e))?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((sender, task))
}

/// Sends a request (`Accept: */*` unless the headers name one) and returns once the response head arrives.
async fn send(method: &str, url: &str, headers: &[(String, String)], body: Vec<u8>) -> Result<HttpResponse, Failure> {
    let target = parse(url).ok_or_else(|| Failure::new(FailureKind::Other, format!("invalid URL {url}")))?;
    let via = proxy(&target);
    let host_header = if (target.secure && target.port == 443) || (!target.secure && target.port == 80) {
        target.host.clone()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let (mut sender, connection, uri) = if target.secure {
        let stream = match &via {
            Some(proxy) => tunnel(connect(&proxy.host, proxy.port).await?, &target.host, target.port).await?,
            None => connect(&target.host, target.port).await?,
        };
        let name = rustls::pki_types::ServerName::try_from(target.host.clone())
            .map_err(|e| Failure::new(FailureKind::Tls, e.to_string()))?;
        let tls = tokio_rustls::TlsConnector::from(tls().clone())
            .connect(name, stream)
            .await
            .map_err(|e| Failure::new(FailureKind::Tls, e.to_string()))?;
        let (sender, connection) = handshake(tls).await?;
        (sender, connection, target.path.clone())
    } else {
        match &via {
            Some(proxy) => {
                let (sender, connection) = handshake(connect(&proxy.host, proxy.port).await?).await?;
                (sender, connection, url.to_owned())
            }
            None => {
                let (sender, connection) = handshake(connect(&target.host, target.port).await?).await?;
                (sender, connection, target.path.clone())
            }
        }
    };
    let mut builder = hyper::Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder = builder.header("Host", host_header);
    if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("accept")) {
        builder = builder.header("Accept", "*/*");
    }
    let request =
        builder.body(Full::new(Bytes::from(body))).map_err(|e| Failure::new(FailureKind::Other, e.to_string()))?;
    sender.ready().await.map_err(|e| hyper_failure(&e))?;
    let response = sender.send_request(request).await.map_err(|e| hyper_failure(&e))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_ascii_lowercase(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    Ok(HttpResponse { status, headers, body: Body { incoming: response.into_body(), _connection: connection } })
}
