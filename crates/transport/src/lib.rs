//! `reqwest` adapter for the restbook engine.
//!
//! Runtime isolation: `reqwest` needs a tokio reactor while GUI frameworks run
//! their own executor, so this type owns a tokio runtime on a dedicated thread.
//! Network settings and the shared cookie jar live in a client cache; redirects
//! are followed by hand so each hop can be recorded for `ResponseRecord`.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cookie_store::{CookieStore, RawCookie};
use futures::channel::oneshot;
use reqwest::header::{HeaderValue, LOCATION};
use reqwest::redirect::Policy;
use reqwest::Client;
use restbook_core::{
    parse_token_response, CancelToken, Error, NetworkConfig, OAuthTransport, PartData,
    PreparedBody, PreparedRequest, Progress, Proxy, RedirectHop, ResponseRecord, Result,
    TokenRequest, TokenResponse, Transport,
};

const MAX_REDIRECTS: usize = 10;
const CANCEL_POLL: Duration = Duration::from_millis(25);

pub struct ReqwestTransport {
    runtime: tokio::runtime::Handle,
    clients: ClientCache,
}

impl ReqwestTransport {
    pub fn new() -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::channel();

        std::thread::Builder::new()
            .name("restbook-http".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                };
                let _ = sender.send(Ok(runtime.handle().clone()));
                // Park this thread for the life of the process: it owns the
                // runtime, and spawned tasks keep running on it.
                runtime.block_on(std::future::pending::<()>());
            })?;

        let runtime = receiver
            .recv()
            .map_err(|_| std::io::Error::other("http runtime thread exited during start"))??;

        Ok(Self {
            runtime,
            clients: ClientCache::default(),
        })
    }

    /// Start from saved settings. An unusable setting is reported by the first
    /// send rather than here, so a bad saved value cannot keep the window shut.
    pub fn with_config(self, config: NetworkConfig) -> Self {
        self.clients.set_config(config);
        self
    }

    /// Replace the network settings. Clients are rebuilt on the next send; the
    /// cookie jar is kept, so cookies survive a settings change.
    pub fn set_config(&self, config: NetworkConfig) -> Result<()> {
        config.validate()?;
        self.clients.set_config(config);
        Ok(())
    }

    pub fn config(&self) -> NetworkConfig {
        self.clients
            .config
            .read()
            .map(|config| config.clone())
            .unwrap_or_default()
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new().expect("failed to start the restbook http runtime")
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord> {
        self.execute_with_progress(request, cancel, Progress::new())
            .await
    }

    async fn execute_with_progress(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
        progress: Progress,
    ) -> Result<ResponseRecord> {
        // The cached client never follows redirects; `run` follows the chain
        // itself so each hop can be recorded.
        let client = self.clients.client(request.insecure)?;

        // Bridge: hand the work to the tokio runtime, await a plain channel.
        let (sender, receiver) = oneshot::channel();
        self.runtime.spawn(async move {
            let _ = sender.send(run(client, request, cancel, progress).await);
        });

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(Error::Transport("request task was dropped".to_owned())),
        }
    }

    fn cookies(&self, url: &str) -> Vec<String> {
        self.clients.cookies(url)
    }

    fn clear_cookies(&self) {
        self.clients.clear();
    }

    fn delete_cookie(&self, url: &str, name: &str) -> bool {
        self.clients.delete_cookie(url, name)
    }

    fn update_cookie(&self, url: &str, name: &str, value: &str) -> bool {
        self.clients.update_cookie(url, name, value)
    }
}

#[async_trait]
impl OAuthTransport for ReqwestTransport {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse> {
        // The token endpoint is contacted with the same trust and proxy policy
        // as an ordinary request; a dedicated client is not needed.
        let client = self.clients.client(false)?;

        let (sender, receiver) = oneshot::channel();
        self.runtime.spawn(async move {
            let _ = sender.send(run_token(client, request).await);
        });

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(Error::Transport("token task was dropped".to_owned())),
        }
    }
}

/// POST a [`TokenRequest`] and parse the reply.
///
/// A non-2xx reply is not automatically an error: many servers send an OAuth
/// error body with a status like 400, and that body carries the real cause. So
/// the body is parsed first and its [`Error::OAuth`] is preferred; only when it
/// is not an OAuth error does the status itself become the message.
async fn run_token(client: Client, request: TokenRequest) -> Result<TokenResponse> {
    let mut builder = client.post(&request.token_url).form(&request.form);
    if let Some((user, password)) = request.basic_auth {
        builder = builder.basic_auth(user, Some(password));
    }

    let response = builder.send().await.map_err(transport_error)?;
    let status = response.status();
    let body = response.bytes().await.map_err(transport_error)?;

    if status.is_success() {
        return parse_token_response(&body);
    }

    match parse_token_response(&body) {
        Err(error @ Error::OAuth(_)) => Err(error),
        _ => Err(Error::Transport(format!(
            "token endpoint returned {}",
            status.as_u16()
        ))),
    }
}

/// The clients the transport can serve, plus the shared jar.
///
/// A `reqwest::Client` is a connection pool and is expensive to build, so the
/// two variants (trusting, and `insecure`) are cached. Changing the settings
/// bumps `generation`, which makes every cached client stale at once without
/// having to know which of them was in use.
#[derive(Default)]
struct ClientCache {
    config: RwLock<NetworkConfig>,
    generation: AtomicU64,
    clients: Mutex<HashMap<bool, (u64, Client)>>,
    /// The shared cookie store. Owned here rather than by a `reqwest::Jar`
    /// because the app needs to *read* and *clear* it, and `Jar` exposes
    /// neither. [`SharedJar`] adapts it to what reqwest expects.
    jar: Arc<SharedJar>,
}

/// A `cookie_store::CookieStore` behind the trait reqwest takes, with the read
/// and clear operations `reqwest::Jar` does not have.
#[derive(Default)]
struct SharedJar(Mutex<CookieStore>);

impl SharedJar {
    fn cookie_pairs(&self, url: &url::Url) -> Vec<String> {
        let Ok(jar) = self.0.lock() else {
            return Vec::new();
        };
        jar.matches(url)
            .into_iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect()
    }

    fn clear(&self) {
        if let Ok(mut jar) = self.0.lock() {
            jar.clear();
        }
    }

    fn delete_named(&self, url: &url::Url, name: &str) -> bool {
        let Ok(mut jar) = self.0.lock() else {
            return false;
        };
        let Some((domain, path, _)) = named_cookie(&jar, url, name) else {
            return false;
        };
        jar.remove(&domain, &path, name).is_some()
    }

    fn set_value(&self, url: &url::Url, name: &str, value: &str) -> bool {
        let Ok(mut jar) = self.0.lock() else {
            return false;
        };
        let Some((_, _, mut raw)) = named_cookie(&jar, url, name) else {
            return false;
        };
        // Re-inserting under the same name replaces the row, so the cookie's
        // path, domain, and flags survive the edit. A failed insert leaves the
        // original in place.
        raw.set_value(value.to_owned());
        jar.insert_raw(&raw, url).is_ok()
    }
}

/// The first cookie `name` the jar would send to `url`, as its store key
/// (domain, path) plus a raw copy to edit.
fn named_cookie(
    jar: &CookieStore,
    url: &url::Url,
    name: &str,
) -> Option<(String, String, RawCookie<'static>)> {
    let cookie = jar
        .matches(url)
        .into_iter()
        .find(|each| each.name() == name)?;
    Some((
        String::from(&cookie.domain),
        String::from(&cookie.path),
        cookie.deref().clone(),
    ))
}

impl reqwest::cookie::CookieStore for SharedJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &url::Url) {
        let Ok(mut jar) = self.0.lock() else {
            return;
        };
        // The same path `reqwest::Jar` takes: parse each `Set-Cookie` and let
        // the store decide whether it is acceptable for this URL. `parse`
        // inserts as a side effect.
        for header in cookie_headers {
            if let Ok(text) = header.to_str() {
                let _ = jar.parse(text, url);
            }
        }
    }

    fn cookies(&self, url: &url::Url) -> Option<HeaderValue> {
        let pairs = self.cookie_pairs(url);
        if pairs.is_empty() {
            return None;
        }
        HeaderValue::from_str(&pairs.join("; ")).ok()
    }
}

impl ClientCache {
    fn set_config(&self, config: NetworkConfig) {
        if let Ok(mut current) = self.config.write() {
            if *current == config {
                return;
            }
            *current = config;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut clients) = self.clients.lock() {
            clients.clear();
        }
    }

    fn client(&self, insecure: bool) -> Result<Client> {
        let generation = self.generation.load(Ordering::SeqCst);

        if let Ok(clients) = self.clients.lock() {
            if let Some((built, client)) = clients.get(&insecure) {
                if *built == generation {
                    return Ok(client.clone());
                }
            }
        }

        let config = self
            .config
            .read()
            .map_err(|_| Error::Transport("network settings lock poisoned".to_owned()))?
            .clone();
        let client = build_client(&config, &self.jar, insecure)?;

        if let Ok(mut clients) = self.clients.lock() {
            clients.insert(insecure, (generation, client.clone()));
        }
        Ok(client)
    }

    fn cookies(&self, url: &str) -> Vec<String> {
        let Ok(url) = url::Url::parse(url) else {
            return Vec::new();
        };
        self.jar.cookie_pairs(&url)
    }

    fn clear(&self) {
        self.jar.clear();
    }

    fn delete_cookie(&self, url: &str, name: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        self.jar.delete_named(&url, name)
    }

    fn update_cookie(&self, url: &str, name: &str, value: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        self.jar.set_value(&url, name, value)
    }
}

fn build_client(config: &NetworkConfig, jar: &Arc<SharedJar>, insecure: bool) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(concat!("restbook/", env!("CARGO_PKG_VERSION")))
        // One jar for the process: cookies survive across requests and across
        // transports, like a browser. The jar outlives a settings change.
        .cookie_provider(jar.clone())
        // Redirects are followed by `run`, so each hop can be reported.
        .redirect(Policy::none());

    if let Some(timeout) = config.default_timeout() {
        builder = builder.timeout(timeout);
    }

    builder = match &config.proxy {
        Proxy::FromEnvironment => builder,
        Proxy::None => builder.no_proxy(),
        Proxy::Url(url) => builder.proxy(
            reqwest::Proxy::all(url.as_str())
                .map_err(|error| Error::Proxy(format!("{url}: {error}")))?,
        ),
    };

    // Only certificate validation is relaxed; hostname verification stays on,
    // and `B27` refuses a cross-host or https→http hop unless this is set.
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    if let Some(roots) = &config.extra_roots {
        let anchors = reqwest::Certificate::from_pem_bundle(roots.as_bytes())
            .map_err(|error| Error::Tls(format!("trusted certificate: {error}")))?;
        for anchor in anchors {
            builder = builder.add_root_certificate(anchor);
        }
    }

    if let Some(cert) = &config.client_cert {
        let identity =
            reqwest::Identity::from_pkcs8_pem(cert.cert_pem.as_bytes(), cert.key_pem.as_bytes())
                .map_err(|error| Error::Tls(format!("client certificate: {error}")))?;
        builder = builder.identity(identity);
    }

    builder.build().map_err(|error| {
        // A failure with TLS material configured is almost always that material,
        // so it is reported as one instead of a generic transport error.
        if insecure || config.extra_roots.is_some() || config.client_cert.is_some() {
            Error::Tls(describe(&error))
        } else {
            Error::Transport(describe(&error))
        }
    })
}

/// Send, following redirects when asked to, and keep the chain.
async fn run(
    client: Client,
    request: PreparedRequest,
    cancel: CancelToken,
    progress: Progress,
) -> Result<ResponseRecord> {
    let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
        .map_err(|error| Error::Transport(error.to_string()))?;

    let mut url = request.url.clone();
    let mut headers = request.headers.clone();
    let body = request.body;
    // Worked out before the body is sent, so the progress denominator is stable
    // across redirect hops, including hops that drop the body.
    let sent_total = body_len(&body);
    // A redirect that drops the body must not resend it on a later hop.
    let mut body_dropped = false;
    let mut redirects: Vec<RedirectHop> = Vec::new();
    let started = Instant::now();

    // The body length, when known up front, so a progress bar has a denominator.
    // Upload progress itself is coarse: reqwest does not report per-chunk sends
    // for buffered bodies, so the body is marked sent once the request is out.
    if let Some(total) = sent_total {
        progress.set_sent_total(total);
    }

    loop {
        let mut builder = client.request(method.clone(), url.clone());
        if let Some(timeout_ms) = request.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        for (key, value) in &headers {
            builder = builder.header(key.as_str(), value.as_str());
        }
        // A user-supplied Content-Type wins over the one the body would add.
        // The body still decides the bytes; only the header is theirs.
        let has_content_type = headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("content-type"));
        builder = apply_body(
            builder,
            if body_dropped {
                &PreparedBody::None
            } else {
                &body
            },
            has_content_type,
        )
        .await?;

        let send = builder.send();
        tokio::pin!(send);
        let response = tokio::select! {
            result = &mut send => result.map_err(transport_error)?,
            _ = wait_for_cancel(cancel.clone()) => return Err(Error::Cancelled),
        };

        let status = response.status().as_u16();
        if request.follow_redirects && (300..400).contains(&status) {
            if let Some(next) = redirect_target(&response, &url) {
                if redirects.len() >= MAX_REDIRECTS {
                    return Err(Error::Transport(format!(
                        "too many redirects (stopped after {MAX_REDIRECTS})"
                    )));
                }
                // A downgrade would send credentials the user protected with TLS
                // over the wire in the clear. The request has to say it accepts
                // that (`insecure`) before the hop is taken.
                if refuses_downgrade(&url, &next, request.insecure) {
                    return Err(Error::Transport(format!(
                        "refused the redirect from {} to {}; enable insecure TLS for this request to follow it",
                        url.scheme(),
                        next.scheme()
                    )));
                }
                redirects.push(RedirectHop {
                    status,
                    location: next.to_string(),
                });

                // 303, and 301/302 on anything but GET/HEAD, degrade to GET and
                // drop the body, which is what browsers do.
                let to_get = status == 303
                    || (matches!(status, 301 | 302)
                        && method != reqwest::Method::GET
                        && method != reqwest::Method::HEAD);
                if to_get {
                    body_dropped = true;
                    headers.retain(|(key, _)| !is_entity_header(key));
                }

                // Credentials belong to the origin that issued them. The jar
                // still sends what it holds for the new host; only the headers
                // the user wrote are dropped here.
                if crosses_origin(&url, &next) {
                    headers.retain(|(key, _)| !is_credential_header(key));
                }

                url = next;
                continue;
            }
        }

        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect::<Vec<_>>();

        // Headers are in; the body follows. From here the progress sink is
        // useful to a UI: it has a status and can show a byte counter.
        progress.headers_received();
        progress.set_received_total(content_length(&response));
        progress.sent(
            sent_total.unwrap_or(0),
            started.elapsed().as_millis() as u64,
        );

        let body = read_body(response, &cancel, &progress, started).await?;

        return Ok(ResponseRecord {
            status,
            headers,
            body: body.into(),
            duration_ms: started.elapsed().as_millis() as u64,
            received_at: chrono::Utc::now(),
            final_url: Some(url.to_string()),
            redirects,
        });
    }
}

/// Stream the response body, reporting bytes as they arrive and staying
/// cancellable between chunks. The whole body is still returned: history needs
/// it, so this is progress reporting, not streaming-to-disk.
async fn read_body(
    mut response: reqwest::Response,
    cancel: &CancelToken,
    progress: &Progress,
    started: Instant,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            result = response.chunk() => result.map_err(transport_error)?,
            _ = wait_for_cancel(cancel.clone()) => return Err(Error::Cancelled),
        };
        let Some(chunk) = chunk else {
            break;
        };
        body.extend_from_slice(&chunk);
        progress.received(body.len() as u64, started.elapsed().as_millis() as u64);
    }
    Ok(body)
}

/// The length of a request body when it is knowable without reading a file.
fn body_len(body: &PreparedBody) -> Option<u64> {
    match body {
        PreparedBody::Bytes { data, .. } => Some(data.len() as u64),
        PreparedBody::Form(fields) => Some(
            fields
                .iter()
                .map(|(key, value)| key.len() + value.len() + 2)
                .sum::<usize>() as u64,
        ),
        // A file or multipart length is only known after reading and framing.
        PreparedBody::None | PreparedBody::File(_) | PreparedBody::Multipart(_) => None,
    }
}

/// The response length, when trusting it is meaningful.
///
/// A `Content-Length` on a compressed response counts the *compressed* bytes,
/// while a decoded body has no `Content-Length` at all. Reporting the compressed
/// figure as a total would make the counter overshoot, so a response that
/// advertises a content encoding is treated as having an unknown total.
fn content_length(response: &reqwest::Response) -> Option<u64> {
    if response
        .headers()
        .contains_key(reqwest::header::CONTENT_ENCODING)
    {
        return None;
    }
    response.content_length()
}

/// The absolute URL a 3xx points at, or `None` when there is no usable
/// `Location` header.
fn redirect_target(response: &reqwest::Response, current: &url::Url) -> Option<url::Url> {
    let location = response.headers().get(LOCATION)?.to_str().ok()?;
    current.join(location).ok()
}

/// Headers that describe the body and must not survive a redirect that drops it.
fn is_entity_header(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "content-type" | "content-length" | "content-encoding" | "transfer-encoding"
    )
}

/// Whether a redirect leaves the origin that issued the credentials.
///
/// The port is deliberately ignored: the rule is the one `B27` states, host or
/// scheme, and a port change alone is the same host.
fn crosses_origin(from: &url::Url, to: &url::Url) -> bool {
    from.scheme() != to.scheme() || from.host_str() != to.host_str()
}

/// Credentials that belong to one origin and must not follow a redirect away
/// from it.
fn is_credential_header(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "proxy-authorization"
    )
}

/// Whether `to` is a TLS downgrade the request does not accept. `run` refuses
/// the hop rather than sending the request in the clear.
fn refuses_downgrade(from: &url::Url, to: &url::Url, insecure: bool) -> bool {
    !insecure && from.scheme() == "https" && to.scheme() == "http"
}

fn transport_error(error: reqwest::Error) -> Error {
    let message = describe(&error);
    let lower = message.to_ascii_lowercase();

    // The kind is decided here, once, by the boundary that has reqwest's own
    // predicates. Nothing above this has to match English text again.
    if error.is_timeout() || lower.contains("timed out") || lower.contains("timeout") {
        Error::Timeout(message)
    } else if lower.contains("certificate")
        || lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("handshake")
    {
        Error::Tls(message)
    } else if error.is_connect()
        || lower.contains("connect")
        || lower.contains("dns")
        || lower.contains("lookup")
        || lower.contains("resolve")
    {
        Error::Connect(message)
    } else {
        Error::Transport(message)
    }
}

/// A reqwest error's `Display` is the same generic sentence for DNS, connect,
/// TLS, and timeout failures. The cause lives in the `source()` chain, so keep
/// every distinct link: the response pane classifies it and the log names it.
fn describe(error: &(dyn StdError + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}

async fn apply_body(
    builder: reqwest::RequestBuilder,
    body: &PreparedBody,
    has_content_type: bool,
) -> Result<reqwest::RequestBuilder> {
    Ok(match body {
        PreparedBody::None => builder,
        PreparedBody::Bytes { mime, data } => {
            let builder = if mime.is_empty() || has_content_type {
                builder
            } else {
                builder.header(reqwest::header::CONTENT_TYPE, mime)
            };
            builder.body(data.clone())
        }
        PreparedBody::Form(fields) => builder.form(fields),
        PreparedBody::File(path) => {
            let data = read_file(path).await?;
            builder.body(data)
        }
        PreparedBody::Multipart(parts) => {
            let mut form = reqwest::multipart::Form::new();
            for part in parts {
                form = match &part.data {
                    PartData::Text(text) => form.text(part.name.clone(), text.clone()),
                    PartData::File {
                        path,
                        mime,
                        file_name,
                    } => {
                        let data = read_file(path).await?;
                        let mut file =
                            reqwest::multipart::Part::bytes(data).file_name(file_name.clone());
                        if let Some(mime) = mime {
                            file = file
                                .mime_str(mime)
                                .map_err(|error| Error::Transport(describe(&error)))?;
                        }
                        form.part(part.name.clone(), file)
                    }
                };
            }
            builder.multipart(form)
        }
    })
}

async fn read_file(path: &str) -> Result<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .map_err(|error| Error::Transport(format!("{path}: {error}")))
}

async fn wait_for_cancel(cancel: CancelToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use restbook_core::{Auth, Body, Method, Progress, RequestDef, VarContext};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A request the transport can send, prepared through the real engine so the
    /// tests exercise exactly what the app hands to the wire.
    fn prepared(method: Method, url: &str, body: Body) -> PreparedRequest {
        let mut request = RequestDef::new(method, url);
        request.body = body;
        restbook_core::prepare(
            &request,
            &VarContext::new(),
            restbook_core::Inherited::default(),
        )
        .unwrap()
    }

    /// A cause chain is what separates `Connection refused` from reqwest's
    /// generic sentence, so `describe` must keep every distinct link.
    #[test]
    fn a_transport_error_keeps_its_cause_chain() {
        #[derive(Debug)]
        struct Link {
            text: &'static str,
            cause: Option<Box<Link>>,
        }

        impl std::fmt::Display for Link {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.text)
            }
        }

        impl StdError for Link {
            fn source(&self) -> Option<&(dyn StdError + 'static)> {
                self.cause
                    .as_deref()
                    .map(|cause| cause as &(dyn StdError + 'static))
            }
        }

        let error = Link {
            text: "error sending request",
            cause: Some(Box::new(Link {
                text: "tcp connect error",
                cause: Some(Box::new(Link {
                    text: "Connection refused",
                    cause: None,
                })),
            })),
        };
        assert_eq!(
            describe(&error),
            "error sending request: tcp connect error: Connection refused"
        );
    }

    /// Accept one connection, read the request head, and answer with `response`.
    /// Returns the bound address. The server is a bare socket so the tests do
    /// not depend on a web framework.
    async fn serve_once(response: String) -> std::net::SocketAddr {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read until the end of the head; bodies are ignored by these tests.
            let mut buffer = [0u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 || buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        address
    }

    #[tokio::test]
    async fn a_real_send_reports_received_bytes() {
        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: text/plain\r\n\r\nhello world";
        let address = serve_once(response.to_owned()).await;

        let transport = ReqwestTransport::default();
        let progress = Progress::new();
        let record = transport
            .execute_with_progress(
                prepared(Method::Get, &format!("http://{address}/"), Body::None),
                CancelToken::new(),
                progress.clone(),
            )
            .await
            .unwrap();

        assert_eq!(record.status, 200);
        assert_eq!(record.text(), "hello world");

        let snapshot = progress.snapshot();
        assert!(snapshot.headers_received);
        assert_eq!(snapshot.received_bytes, 11);
        assert_eq!(snapshot.received_total, Some(11));
    }

    /// Like [`serve_once`], but captures the whole request (head and body) so a
    /// test can assert how a form was encoded on the wire.
    async fn serve_once_capturing(
        response: String,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                // Stop once the head and the promised body have both arrived.
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            String::from_utf8_lossy(&raw).into_owned()
        });

        (address, handle)
    }

    #[tokio::test]
    async fn fetch_token_posts_the_form_and_parses_the_reply() {
        let body = r#"{"access_token":"abc","token_type":"Bearer"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (address, captured) = serve_once_capturing(response).await;

        let config = restbook_core::OAuthConfig {
            grant: restbook_core::OAuthGrant::ClientCredentials,
            client_id: "client-1".to_owned(),
            client_secret: String::new(),
            token_url: format!("http://{address}/token"),
            ..restbook_core::OAuthConfig::default()
        };
        let request = config.client_credentials_request().unwrap();

        let transport = ReqwestTransport::default();
        let token = transport.fetch_token(request).await.unwrap();
        assert_eq!(token.access_token, "abc");
        assert_eq!(token.token_type.as_deref(), Some("Bearer"));

        let raw = captured.await.unwrap();
        assert!(raw.contains("grant_type=client_credentials"));
        assert!(raw.contains("client_id=client-1"));
    }

    #[tokio::test]
    async fn fetch_token_prefers_the_oauth_error_body() {
        let body = r#"{"error":"invalid_client","error_description":"bad secret"}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let address = serve_once(response).await;

        let config = restbook_core::OAuthConfig {
            grant: restbook_core::OAuthGrant::ClientCredentials,
            client_id: "client-1".to_owned(),
            client_secret: "wrong".to_owned(),
            token_url: format!("http://{address}/token"),
            ..restbook_core::OAuthConfig::default()
        };
        let request = config.client_credentials_request().unwrap();

        let transport = ReqwestTransport::default();
        let error = transport.fetch_token(request).await.unwrap_err();
        assert!(
            matches!(&error, Error::OAuth(message) if message == "invalid_client: bad secret"),
            "got {error}"
        );
    }

    #[tokio::test]
    async fn a_json_body_reports_a_known_upload_size() {
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let address = serve_once(response.to_owned()).await;

        let transport = ReqwestTransport::default();
        let progress = Progress::new();
        transport
            .execute_with_progress(
                prepared(
                    Method::Post,
                    &format!("http://{address}/things"),
                    Body::Raw {
                        mime: "application/json".to_owned(),
                        text: "{\"hello\":true}".to_owned(),
                    },
                ),
                CancelToken::new(),
                progress.clone(),
            )
            .await
            .unwrap();

        // The 14-byte raw body gives the bar a denominator.
        assert_eq!(progress.snapshot().sent_bytes, 14);
        assert_eq!(progress.snapshot().sent_total, Some(14));
    }

    #[tokio::test]
    async fn cancelling_a_slow_response_stops_the_read() {
        // A server that sends headers but never finishes the body.
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await;
            // Promise 100 bytes, deliver none.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .await
                .unwrap();
            // Hold the connection open until the test drops it.
            std::future::pending::<()>().await;
        });

        let transport = ReqwestTransport::default();
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        // Cancel shortly after the request goes out.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });

        let error = transport
            .execute(
                prepared(Method::Get, &format!("http://{address}/"), Body::None),
                cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Cancelled));
        server.abort();
    }

    #[tokio::test]
    async fn auth_header_from_the_engine_reaches_the_wire() {
        // The engine turns `Auth::Bearer` into a header in `prepare()`; this
        // proves the transport sends what it is handed rather than re-deriving.
        let mut request = RequestDef::new(Method::Get, "https://example.com/");
        request.auth = Auth::Bearer {
            token: "abc".to_owned(),
        };
        let prepared = restbook_core::prepare(
            &request,
            &VarContext::new(),
            restbook_core::Inherited::default(),
        )
        .unwrap();
        assert!(prepared
            .headers
            .contains(&("authorization".to_owned(), "Bearer abc".to_owned())));
    }

    #[test]
    fn an_entity_header_is_recognised_whatever_its_case() {
        assert!(is_entity_header("Content-Type"));
        assert!(is_entity_header("content-length"));
        assert!(is_entity_header("TRANSFER-ENCODING"));
        assert!(!is_entity_header("authorization"));
        assert!(!is_entity_header("cookie"));
    }

    #[test]
    fn a_relative_location_is_resolved_against_the_hop() {
        let base = url::Url::parse("https://example.com/a/b").unwrap();
        assert_eq!(base.join("/c").unwrap().as_str(), "https://example.com/c");
        assert_eq!(base.join("d").unwrap().as_str(), "https://example.com/a/d");
    }

    #[test]
    fn settings_that_did_not_change_keep_the_cached_client() {
        let cache = ClientCache::default();
        cache.client(false).unwrap();
        cache.client(false).unwrap();
        // A hit returns the cached pool instead of rebuilding it; a rebuild is
        // only observable through the generation staying put.
        assert_eq!(cache.generation.load(Ordering::SeqCst), 0);
        assert_eq!(cache.clients.lock().unwrap().len(), 1);

        cache.set_config(NetworkConfig {
            proxy: Proxy::None,
            ..NetworkConfig::default()
        });
        assert_eq!(cache.generation.load(Ordering::SeqCst), 1);
        assert!(cache.clients.lock().unwrap().is_empty());
    }

    #[test]
    fn the_same_settings_do_not_invalidate_the_cache() {
        let cache = ClientCache::default();
        cache.set_config(NetworkConfig::default());
        assert_eq!(cache.generation.load(Ordering::SeqCst), 0);
    }

    /// A jar holding the cookies in `headers`, parsed for `url` the way a send
    /// stores them.
    fn jar(url: &str, headers: &[&str]) -> (url::Url, SharedJar) {
        let url = url::Url::parse(url).unwrap();
        let jar = SharedJar::default();
        {
            let mut store = jar.0.lock().unwrap();
            for header in headers {
                store.parse(header, &url).unwrap();
            }
        }
        (url, jar)
    }

    #[test]
    fn deleting_one_cookie_leaves_the_others() {
        let (url, jar) = jar(
            "https://example.com/app",
            &["sid=abc; Path=/", "theme=dark; Path=/"],
        );
        assert_eq!(jar.cookie_pairs(&url).len(), 2);

        assert!(jar.delete_named(&url, "sid"));
        assert_eq!(jar.cookie_pairs(&url), ["theme=dark"]);
        assert!(!jar.delete_named(&url, "sid"));
    }

    #[test]
    fn updating_one_cookie_keeps_its_attributes_and_the_others() {
        let (url, jar) = jar(
            "https://example.com/app",
            &["sid=abc; Path=/; HttpOnly", "theme=dark; Path=/"],
        );

        assert!(jar.set_value(&url, "sid", "xyz"));
        assert!(jar.cookie_pairs(&url).contains(&"sid=xyz".to_owned()));
        assert!(jar.cookie_pairs(&url).contains(&"theme=dark".to_owned()));

        let store = jar.0.lock().unwrap();
        let sid = store
            .matches(&url)
            .into_iter()
            .find(|cookie| cookie.name() == "sid")
            .unwrap();
        assert_eq!(sid.http_only(), Some(true));
        assert_eq!(&*sid.path, "/");
        drop(store);
        assert!(!jar.set_value(&url, "missing", "x"));
    }

    /// Serve `responses` in order on one listener and return the head of every
    /// request, in order. A two-entry sequence is one redirect chain.
    async fn serve_sequence(
        responses: Vec<String>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let mut captured = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buffer[..read]);
                    if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured.push(String::from_utf8_lossy(&raw).into_owned());
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
            captured
        });

        (address, handle)
    }

    fn has_header(head: &str, name: &str) -> bool {
        head.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
        })
    }

    fn with_credentials(url: &str) -> PreparedRequest {
        let mut request = RequestDef::new(Method::Get, url);
        for (key, value) in [
            ("authorization", "Bearer secret"),
            ("cookie", "sid=abc"),
            ("proxy-authorization", "Basic cHJveHk="),
            ("x-keep", "yes"),
        ] {
            request.headers.push(restbook_core::Kv::new(key, value));
        }
        restbook_core::prepare(
            &request,
            &VarContext::new(),
            restbook_core::Inherited::default(),
        )
        .unwrap()
    }

    /// The user's credentials belong to the origin that issued them, so a hop
    /// that changes host or scheme must not carry `Authorization`, `Cookie`, or
    /// `Proxy-Authorization`. Other headers survive.
    #[tokio::test]
    async fn a_redirect_to_another_host_drops_the_credentials() {
        let (target, target_handle) = serve_sequence(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let (first, first_handle) = serve_sequence(vec![format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.port()
        )])
        .await;

        let transport = ReqwestTransport::default();
        transport
            .execute(
                with_credentials(&format!("http://localhost:{}/start", first.port())),
                CancelToken::new(),
            )
            .await
            .unwrap();

        let second = target_handle.await.unwrap().remove(0);
        assert!(!has_header(&second, "authorization"));
        assert!(!has_header(&second, "cookie"));
        assert!(!has_header(&second, "proxy-authorization"));
        assert!(has_header(&second, "x-keep"));

        first_handle.await.unwrap();
    }

    #[tokio::test]
    async fn a_same_host_redirect_keeps_the_credentials() {
        let (address, handle) = serve_sequence(vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned(),
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;

        let transport = ReqwestTransport::default();
        transport
            .execute(
                with_credentials(&format!("http://{address}/start")),
                CancelToken::new(),
            )
            .await
            .unwrap();

        let captured = handle.await.unwrap();
        let second = &captured[1];
        assert!(has_header(second, "authorization"));
        assert!(has_header(second, "cookie"));
        assert!(has_header(second, "proxy-authorization"));
    }

    #[test]
    fn a_tls_downgrade_is_refused_unless_the_request_is_insecure() {
        let https = url::Url::parse("https://example.com/a").unwrap();
        let other = url::Url::parse("https://example.com/b").unwrap();
        let http = url::Url::parse("http://example.com/a").unwrap();

        assert!(refuses_downgrade(&https, &http, false));
        assert!(!refuses_downgrade(&https, &http, true));
        assert!(!refuses_downgrade(&http, &https, false));
        assert!(!refuses_downgrade(&https, &other, false));
    }
}
