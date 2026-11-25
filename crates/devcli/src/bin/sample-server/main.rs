//! The local sample server: deterministic HTTP fixtures for the response pane.
//!
//! Some pane behavior is only observable against a specific body shape or timing
//! a public echo service will not produce on demand: a minified payload, a body
//! past `MAX_BODY_BYTES`, a slow reply. Nothing is served past loopback. The
//! route tables live in `docs/05-ui-guide.md`, kept in step with `ROUTES` by test.

use std::io;
use std::time::Duration;

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

mod bodies;

use bodies::{
    binary, body_of_kilobytes, csv_text, deep, error_body, gzip, html_page, huge, invalid_json,
    json_array, json_scalar, json_string, long_string, many_lines, one_line, plain_text,
    record_compact, unicode_json, xml_doc, BINARY, CSV, HTML, INDEX, JSON, TEXT, XML,
};

#[derive(Parser)]
#[command(
    name = "sample-server",
    about = "Serve deterministic sample responses for the response pane"
)]
struct Cli {
    /// The port to bind.
    #[arg(long, default_value_t = 8787)]
    port: u16,

    /// The interface to bind. Only loopback is useful here.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let listener = TcpListener::bind((cli.host.as_str(), cli.port)).await?;
    println!("sample: http://{}", listener.local_addr()?);

    loop {
        let (socket, _peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(error) = serve(socket).await {
                // A client that hangs up is a successful test, not a failure.
                if !is_hangup(&error) {
                    eprintln!("sample: {error}");
                }
            }
        });
    }
}

/// One route and the body shape it serves. A `match` on it is the dispatcher,
/// so the table and the behaviour cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Index,
    OneLine,
    LongString,
    ManyLines,
    Huge,
    Body,
    Stream,
    Abort,
    Empty,
    Binary,
    Gzip,
    Array,
    Scalar,
    InvalidJson,
    Sniff,
    Xml,
    Html,
    Text,
    Csv,
    Unicode,
    Deep,
    Error,
    Headers,
    Cookies,
    Redirect,
    Echo,
}

/// The one table: membership (404 and HEAD) and dispatch both read it.
const ROUTES: &[(&str, Sample)] = &[
    ("/", Sample::Index),
    ("/one-line", Sample::OneLine),
    ("/long-string", Sample::LongString),
    ("/many-lines", Sample::ManyLines),
    ("/huge", Sample::Huge),
    ("/body", Sample::Body),
    ("/stream", Sample::Stream),
    ("/abort", Sample::Abort),
    ("/empty", Sample::Empty),
    ("/binary", Sample::Binary),
    ("/gzip", Sample::Gzip),
    ("/array", Sample::Array),
    ("/scalar", Sample::Scalar),
    ("/invalid-json", Sample::InvalidJson),
    ("/sniff", Sample::Sniff),
    ("/xml", Sample::Xml),
    ("/html", Sample::Html),
    ("/text", Sample::Text),
    ("/csv", Sample::Csv),
    ("/unicode", Sample::Unicode),
    ("/deep", Sample::Deep),
    ("/error", Sample::Error),
    ("/headers", Sample::Headers),
    ("/cookies", Sample::Cookies),
    ("/redirect", Sample::Redirect),
    ("/echo", Sample::Echo),
];

fn sample(path: &str) -> Option<Sample> {
    ROUTES
        .iter()
        .find_map(|(route, sample)| (*route == path).then_some(*sample))
}

struct Request {
    method: String,
    target: String,
    path: String,
    version: String,
    query: Vec<(String, String)>,
    /// As received, in order, so `/echo` can reflect them unchanged.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn wants_close(&self) -> bool {
        self.header("connection")
            .is_some_and(|value| value.to_ascii_lowercase().contains("close"))
    }
}

struct Reply {
    status: u16,
    bytes: usize,
    close: bool,
}

async fn serve(socket: TcpStream) -> io::Result<()> {
    let (read, mut write) = socket.into_split();
    let mut reader = BufReader::new(read);

    // One connection carries as many requests as the client wants, so the
    // pool in the app is exercised the same way it is against a real server.
    loop {
        let Some(request) = read_request(&mut reader).await? else {
            return Ok(());
        };
        let reply = handle(&mut write, &request).await?;
        println!(
            "sample: \"{} {} {}\" {} {}",
            request.method, request.target, request.version, reply.status, reply.bytes
        );
        if reply.close || request.wants_close() {
            return Ok(());
        }
    }
}

async fn read_request(reader: &mut BufReader<OwnedReadHalf>) -> io::Result<Option<Request>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        if !line.iter().all(u8::is_ascii_whitespace) {
            break;
        }
    }

    let request_line = String::from_utf8_lossy(&line);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let version = parts.next().unwrap_or("HTTP/1.1").to_owned();

    let mut headers = Vec::new();
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            break;
        }
        if let Some((name, value)) = text.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let (path, query_text) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query),
        None => (target.clone(), ""),
    };
    // `parse_qs` drops blank values, and so does this.
    let query = url::form_urlencoded::parse(query_text.as_bytes())
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();

    let body = read_body(reader, &headers).await?;

    Ok(Some(Request {
        method,
        target,
        path,
        version,
        query,
        headers,
        body,
    }))
}

/// The request body under either framing. The app sends `Content-Length`, but
/// a streaming client sends chunked, and leaving either unread desyncs the
/// next request on the connection.
async fn read_body(
    reader: &mut BufReader<OwnedReadHalf>,
    headers: &[(String, String)],
) -> io::Result<Vec<u8>> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    if header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        let mut body = Vec::new();
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).await? == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line);
            let size = text
                .trim()
                .split(';')
                .next()
                .and_then(|size| usize::from_str_radix(size.trim(), 16).ok())
                .unwrap_or(0);
            if size == 0 {
                let mut trailer = Vec::new();
                reader.read_until(b'\n', &mut trailer).await?;
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).await?;
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf).await?;
        }
        return Ok(body);
    }

    let length = header("content-length")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok(body)
}

async fn handle(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    // HEAD carries no body by definition, so this only proves the method
    // reaches the pane with a status and headers.
    if request.method == "HEAD" {
        return if sample(&request.path).is_some() {
            respond(w, 200, Some(JSON), &[], b"").await
        } else {
            respond(w, 404, Some(HTML), &[], b"").await
        };
    }

    let Some(kind) = sample(&request.path) else {
        return respond(w, 404, Some(HTML), &[], NOT_FOUND_PAGE.as_bytes()).await;
    };

    // Every other route is read-only, and such a request's body is unread, so
    // the connection cannot be reused.
    if request.method != "GET" && kind != Sample::Echo {
        let body = error_body(405);
        let allow = [("Allow".to_owned(), "GET, HEAD".to_owned())];
        let mut reply = respond(w, 405, Some(JSON), &allow, body.as_bytes()).await?;
        reply.close = true;
        return Ok(reply);
    }

    let delay = query_float(request, "delay", 0.0).unwrap_or(0.0);
    sleep_millis(delay).await;

    match kind {
        Sample::Index => respond(w, 200, Some(TEXT), &[], INDEX.as_bytes()).await,
        Sample::OneLine => {
            let body = one_line();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::LongString => {
            let body = long_string();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::ManyLines => {
            let body = many_lines();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::Huge => {
            let body = huge();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::Body => body_route(w, request).await,
        Sample::Stream => stream_route(w, request).await,
        Sample::Abort => abort_route(w, request).await,
        Sample::Empty => respond(w, 204, None, &[], b"").await,
        Sample::Binary => {
            let body = binary();
            respond(w, 200, Some(BINARY), &[], &body).await
        }
        Sample::Gzip => {
            let body = gzip(&one_line());
            let encoding = [("Content-Encoding".to_owned(), "gzip".to_owned())];
            respond(w, 200, Some(JSON), &encoding, &body).await
        }
        Sample::Array => {
            let body = json_array();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::Scalar => respond(w, 200, Some(JSON), &[], json_scalar().as_bytes()).await,
        Sample::InvalidJson => respond(w, 200, Some(JSON), &[], invalid_json().as_bytes()).await,
        // No `Content-Type` header at all: the viewer's own sniff has to
        // decide, while the format chip still says "Text".
        Sample::Sniff => {
            let body = one_line();
            respond(w, 200, None, &[], body.as_bytes()).await
        }
        Sample::Xml => {
            let body = xml_doc();
            respond(w, 200, Some(XML), &[], body.as_bytes()).await
        }
        Sample::Html => {
            let body = html_page();
            respond(w, 200, Some(HTML), &[], body.as_bytes()).await
        }
        Sample::Text => {
            let body = plain_text();
            respond(w, 200, Some(TEXT), &[], body.as_bytes()).await
        }
        Sample::Csv => {
            let body = csv_text();
            respond(w, 200, Some(CSV), &[], body.as_bytes()).await
        }
        Sample::Unicode => {
            let body = unicode_json();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::Deep => {
            let body = deep();
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
        Sample::Error => error_route(w, request).await,
        Sample::Headers => headers_route(w, request).await,
        Sample::Cookies => cookies_route(w).await,
        Sample::Redirect => redirect_route(w, request).await,
        Sample::Echo => {
            let body = echo_body(request);
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
    }
}

/// `/body?kb=<n>` — the size is the point, so a bad value is a 400.
async fn body_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    let kilobytes = query_integer(request, "kb", 512);
    match kilobytes {
        Err(()) => respond(w, 400, Some(TEXT), &[], b"kb must be a whole number\n").await,
        Ok(kilobytes) if kilobytes < 1 => {
            respond(w, 400, Some(TEXT), &[], b"kb must be at least 1\n").await
        }
        Ok(kilobytes) => {
            let body = body_of_kilobytes(kilobytes);
            respond(w, 200, Some(JSON), &[], body.as_bytes()).await
        }
    }
}

async fn stream_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    let chunks = query_integer(request, "chunks", 10);
    let gap = query_float(request, "gap", 250.0);
    match (chunks, gap) {
        (Ok(chunks), Ok(gap)) => stream(w, chunks.max(1), gap).await,
        _ => respond(w, 400, Some(TEXT), &[], b"chunks and gap must be numbers\n").await,
    }
}

async fn abort_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    let chunks = query_integer(request, "chunks", 2).unwrap_or(2).max(1);
    let gap = query_float(request, "gap", 100.0).unwrap_or(100.0);
    abort(w, chunks, gap).await
}

async fn error_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    // `send_error` would answer in HTML; a JSON body is what an API client
    // actually has to render.
    let status = u16::try_from(query_integer(request, "status", 500).unwrap_or(500)).unwrap_or(500);
    let body = error_body(i64::from(status));
    respond(w, status, Some(JSON), &[], body.as_bytes()).await
}

async fn headers_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    let count = query_integer(request, "count", 10).unwrap_or(10).max(0);
    let body = format!("{{\"status\": \"OK\", \"headers\": {count}}}");
    let extra: Vec<(String, String)> = (0..count)
        .map(|index| {
            (
                format!("X-Sample-{index}"),
                format!("value-{index}-{}", "y".repeat(40)),
            )
        })
        .collect();
    respond(w, 200, Some(JSON), &extra, body.as_bytes()).await
}

async fn cookies_route(w: &mut OwnedWriteHalf) -> io::Result<Reply> {
    // One header per cookie, including a repeated name, a quoted value with a
    // semicolon in it, and an already-expired one.
    let cookies = [
        "session=abc123; Path=/; HttpOnly; SameSite=Lax",
        "theme=dark; Path=/; Max-Age=3600",
        "session=second; Path=/other",
        "prefs=\"a=b; c=d\"; Path=/",
        "legacy=gone; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
    ];
    let extra: Vec<(String, String)> = cookies
        .iter()
        .map(|cookie| ("Set-Cookie".to_owned(), (*cookie).to_owned()))
        .collect();
    let body = "{\"status\": \"OK\", \"setCookies\": 5}";
    respond(w, 200, Some(JSON), &extra, body.as_bytes()).await
}

async fn redirect_route(w: &mut OwnedWriteHalf, request: &Request) -> io::Result<Reply> {
    let hops = query_integer(request, "hops", 3).unwrap_or(3);
    let status = query_integer(request, "status", 302).unwrap_or(302);
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return respond(
            w,
            400,
            Some(TEXT),
            &[],
            b"status must be a redirect status\n",
        )
        .await;
    }
    if hops <= 0 {
        let body = one_line();
        return respond(w, 200, Some(JSON), &[], body.as_bytes()).await;
    }
    let location = [(
        "Location".to_owned(),
        format!("/redirect?hops={}&status={status}", hops - 1),
    )];
    respond(w, status as u16, None, &location, b"").await
}

/// Chunked JSON, flushed one record at a time, so the in-flight byte counter
/// moves and Cancel lands mid-transfer.
async fn stream(w: &mut OwnedWriteHalf, chunks: i64, gap: f64) -> io::Result<Reply> {
    let chunked = [("Transfer-Encoding".to_owned(), "chunked".to_owned())];
    w.write_all(head(200, Some(JSON), &chunked, None).as_bytes())
        .await?;

    let mut total = write_chunk(w, b"{\"status\":\"OK\",\"data\":[").await?;
    for index in 0..chunks {
        let record = record_compact(index as u64);
        let mut payload = Vec::with_capacity(record.len() + 1);
        if index > 0 {
            payload.push(b',');
        }
        payload.extend_from_slice(record.as_bytes());
        total += write_chunk(w, &payload).await?;
        sleep_millis(gap).await;
    }
    total += write_chunk(w, b"]}").await?;

    w.write_all(b"0\r\n\r\n").await?;
    w.flush().await?;
    Ok(Reply {
        status: 200,
        bytes: total,
        close: false,
    })
}

/// Chunked JSON that stops mid-body: a few chunks, then the socket closes
/// without the final chunk or the terminating one. That is the shape that
/// shows the transport's short-read error instead of a quiet truncation.
async fn abort(w: &mut OwnedWriteHalf, chunks: i64, gap: f64) -> io::Result<Reply> {
    let chunked = [("Transfer-Encoding".to_owned(), "chunked".to_owned())];
    w.write_all(head(200, Some(JSON), &chunked, None).as_bytes())
        .await?;

    let mut total = write_chunk(w, b"{\"status\":\"OK\",\"data\":[").await?;
    for index in 0..chunks {
        let record = record_compact(index as u64);
        let mut payload = Vec::with_capacity(record.len() + 1);
        if index > 0 {
            payload.push(b',');
        }
        payload.extend_from_slice(record.as_bytes());
        total += write_chunk(w, &payload).await?;
        sleep_millis(gap).await;
    }

    // No `]}` and no terminating chunk: closing here is the point.
    Ok(Reply {
        status: 200,
        bytes: total,
        close: true,
    })
}

/// One HTTP/1.1 chunk: hex length, CRLF, payload, CRLF, flushed at once.
async fn write_chunk(w: &mut OwnedWriteHalf, data: &[u8]) -> io::Result<usize> {
    w.write_all(format!("{:x}\r\n", data.len()).as_bytes())
        .await?;
    w.write_all(data).await?;
    w.write_all(b"\r\n").await?;
    w.flush().await?;
    Ok(data.len())
}

/// The request reflected as JSON: method, path, repeated query parameters,
/// headers, and body. The only route that accepts a body, and what makes POST,
/// PUT, PATCH, DELETE, and OPTIONS testable at all.
fn echo_body(request: &Request) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"method\": {},\n",
        json_string(&request.method)
    ));
    out.push_str(&format!("  \"path\": {},\n", json_string(&request.path)));
    out.push_str(&format!(
        "  \"httpVersion\": {},\n",
        json_string(&request.version)
    ));

    let pairs = ordered_query_pairs(&request.query);
    if pairs.is_empty() {
        out.push_str("  \"query\": [],\n");
    } else {
        out.push_str("  \"query\": [\n");
        for (position, (name, value)) in pairs.iter().enumerate() {
            let comma = if position + 1 < pairs.len() { "," } else { "" };
            out.push_str(&format!(
                "    [\n      {},\n      {}\n    ]{comma}\n",
                json_string(name),
                json_string(value)
            ));
        }
        out.push_str("  ],\n");
    }

    match request.header("content-type") {
        Some(value) => out.push_str(&format!("  \"contentType\": {},\n", json_string(value))),
        None => out.push_str("  \"contentType\": null,\n"),
    }

    if request.headers.is_empty() {
        out.push_str("  \"headers\": [],\n");
    } else {
        out.push_str("  \"headers\": [\n");
        for (position, (name, value)) in request.headers.iter().enumerate() {
            let comma = if position + 1 < request.headers.len() {
                ","
            } else {
                ""
            };
            out.push_str(&format!(
                "    [\n      {},\n      {}\n    ]{comma}\n",
                json_string(name),
                json_string(value)
            ));
        }
        out.push_str("  ],\n");
    }

    out.push_str(&format!("  \"bodyBytes\": {},\n", request.body.len()));
    out.push_str(&format!(
        "  \"body\": {}\n",
        json_string(&String::from_utf8_lossy(&request.body))
    ));
    out.push('}');
    out
}

/// Query pairs in the order `parse_qs` lists them: names in first-seen order,
/// each repeated with all of its values, so `/echo` can show a repeated
/// parameter as several rows rather than one list.
fn ordered_query_pairs(query: &[(String, String)]) -> Vec<(&str, &str)> {
    let mut names: Vec<&str> = Vec::new();
    for (name, _) in query {
        if !names.contains(&name.as_str()) {
            names.push(name);
        }
    }

    let mut pairs = Vec::with_capacity(query.len());
    for name in names {
        for (other, value) in query {
            if other == name {
                pairs.push((name, value.as_str()));
            }
        }
    }
    pairs
}

fn head(
    status: u16,
    content_type: Option<&str>,
    extra: &[(String, String)],
    content_length: Option<usize>,
) -> String {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    if let Some(value) = content_type {
        out.push_str(&format!("Content-Type: {value}\r\n"));
    }
    if let Some(length) = content_length {
        out.push_str(&format!("Content-Length: {length}\r\n"));
    }
    for (name, value) in extra {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out
}

async fn respond(
    w: &mut OwnedWriteHalf,
    status: u16,
    content_type: Option<&str>,
    extra: &[(String, String)],
    body: &[u8],
) -> io::Result<Reply> {
    // 204 means "nothing to say": no body and no `Content-Length`.
    let content_length = (status != 204).then_some(body.len());
    w.write_all(head(status, content_type, extra, content_length).as_bytes())
        .await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(Reply {
        status,
        bytes: body.len(),
        close: false,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        418 => "I'm a Teapot",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// The first value for `key`, or `None` when the parameter is absent.
fn first_query<'a>(request: &'a Request, key: &str) -> Option<&'a str> {
    request
        .query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// `Ok` is the value, `Err` is "present but not a number" — the difference
/// between a default and a 400.
fn query_integer(request: &Request, key: &str, default: i64) -> Result<i64, ()> {
    match first_query(request, key) {
        None => Ok(default),
        Some(raw) => raw.trim().parse().map_err(|_| ()),
    }
}

fn query_float(request: &Request, key: &str, default: f64) -> Result<f64, ()> {
    match first_query(request, key) {
        None => Ok(default),
        Some(raw) => raw.trim().parse().map_err(|_| ()),
    }
}

async fn sleep_millis(milliseconds: f64) {
    if milliseconds > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(milliseconds / 1000.0)).await;
    }
}

fn is_hangup(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
    )
}

/// The body Python's `send_error` produced: an HTML page, not a bare string.
const NOT_FOUND_PAGE: &str = "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error response</title>\n</head>\n<body>\n<h1>Error response</h1>\n<p>Error code: 404</p>\n<p>Message: no such sample.</p>\n<p>Error code explanation: 404 - Nothing matches the given URI.</p>\n</body>\n</html>\n";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route is named in the owning document. `/` is the index, not a
    /// fixture a reader looks up.
    #[test]
    fn every_sample_route_is_documented() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/05-ui-guide.md");
        let doc = std::fs::read_to_string(path).expect("docs/05-ui-guide.md");
        for (route, _) in ROUTES {
            if *route == "/" {
                continue;
            }
            assert!(
                doc.contains(&format!("`{route}")),
                "docs/05-ui-guide.md is missing the sample route {route:?}",
            );
        }
    }

    /// The table is the only registration, so a duplicate would shadow a route
    /// rather than fail to compile.
    #[test]
    fn no_route_is_registered_twice() {
        for (position, (route, _)) in ROUTES.iter().enumerate() {
            assert!(
                !ROUTES[..position].iter().any(|(seen, _)| seen == route),
                "{route} is registered twice",
            );
        }
        assert!(ROUTES.len() > 10);
    }
}
