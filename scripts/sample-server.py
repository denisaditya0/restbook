#!/usr/bin/env python3
"""Serve sample requests and responses for trying the app by hand.

Some of what the response pane does is only observable against a specific body
shape or timing, and a public echo service will not produce either on demand:
a *minified* payload is a single line thousands of pixels wide, a long string
inside a pretty-printed one is what exposes the clipping in the numbered view,
a body past `MAX_BODY_BYTES` is the only way to see the truncation banner, and
a slow or chunked reply is the only way to watch the in-flight strip and Cancel.
The same holds for the formats the viewer has to label and survive — XML, HTML,
CSV, plain text, a top-level array, a scalar, invalid JSON, binary bytes, a
gzipped body, and a reply with no `Content-Type` at all — and for the request
side, which needs an endpoint that accepts a body.

    python scripts/sample-server.py            # http://127.0.0.1:8787
    python scripts/sample-server.py --port 9000

Point restbook's URL bar at any of these. Bodies are deterministic, so two
sends of the same request have the same bytes and "Copy body" is diffable.

### Bodies the viewer has to survive

| Path | Shape | What it exercises |
|---|---|---|
| `/one-line` | minified JSON, one ~10 KB line | the Raw view must scroll sideways |
| `/long-string` | indented JSON with one ~2.7 KB string | the numbered view must scroll that line sideways |
| `/many-lines` | indented JSON, ~4800 lines | the unnumbered fallback |
| `/huge` | minified JSON, ~925 KB | the "truncated for display" banner |
| `/body?kb=2048` | minified JSON of about that many KiB | size formatting, scrolling, truncation at any scale |
| `/stream?chunks=10&gap=250` | chunked JSON, one record per write | live byte counter, Cancel mid-transfer |
| `/abort?chunks=2` | chunked JSON that stops mid-body | the transport's short-read error, not a silent truncation |
| `/empty` | `204 No Content`, no body | a finished response with nothing to show |
| `/binary` | `application/octet-stream`, invalid UTF-8 | lossy decoding, the "Binary"/Text label |
| `/gzip` | gzip-compressed JSON | transparent decompression; the header disappears |

### Formats

| Path | Shape | What it exercises |
|---|---|---|
| `/array` | JSON with a top-level array | pretty-printing and highlighting a non-object root |
| `/scalar` | JSON `null` | the smallest valid JSON, and the empty-looking body |
| `/invalid-json` | `application/json` that does not parse | the raw-text fallback, not a broken pretty view |
| `/sniff` | JSON bytes, **no** `Content-Type` | content sniffing; the chip still says "Text" |
| `/xml` | indented XML, one long attribute row | the XML label and unhighlighted long lines |
| `/html` | HTML page, one long tag row | the HTML highlight and a row wider than the pane |
| `/text` | plain text, one very long line | a body with no structure at all |
| `/csv` | quoted CSV | commas, escaped quotes, and a header row |
| `/unicode` | CJK, Arabic, emoji, combining marks | glyph width, selection, and a long unbreakable run |
| `/deep` | JSON nested ~120 levels | indentation width and the line gutter |

### Transport, headers, and cookies

| Path | Shape | What it exercises |
|---|---|---|
| `/error?status=503` | that status, JSON error body | the error banner and the status colour |
| `/headers?count=40` | 40 `X-Sample-<n>` response headers | the Headers tab and its count badge |
| `/cookies` | five `Set-Cookie` headers, one name repeated | the Cookies tab, attributes, quoted values |
| `/redirect?hops=3&status=302` | a chain ending at `/one-line` | follow-redirects on and off (Settings) |

### Requests

| Path | Shape | What it exercises |
|---|---|---|
| `/echo` | the request reflected as JSON | `POST`/`PUT`/`PATCH`/`DELETE` bodies, repeated query parameters, headers |
| `/?delay=600000` | headers withheld for 10 minutes | Cancel before the first byte |

Every path also takes `?delay=<ms>`, which waits that long before the response
starts — combine it with `/stream` to hold a transfer open.

No dependencies beyond the standard library, and nothing is served past
127.0.0.1.
"""

import argparse
import base64
import gzip
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

JSON = "application/json; charset=utf-8"
TEXT = "text/plain; charset=utf-8"
HTML = "text/html; charset=utf-8"
XML = "application/xml; charset=utf-8"
CSV = "text/csv; charset=utf-8"
BINARY = "application/octet-stream"

# 256 KiB, matching `util::MAX_BODY_BYTES` on the app side.
MAX_BODY_BYTES = 256 * 1024


def _mix(value: int) -> int:
    """SplitMix64: a stable scramble, so no id reads as a counter."""
    mask = (1 << 64) - 1
    value = (value + 0x9E3779B97F4A7C15) & mask
    value = (value ^ (value >> 30)) * 0xBF58476D1CE4E5B9 & mask
    value = (value ^ (value >> 27)) * 0x94D049BB133111EB & mask
    return value ^ (value >> 31)


def _uuid(seed: int) -> str:
    """A stable uuid-shaped string, so repeated runs are comparable."""
    high = _mix(seed)
    low = _mix(high ^ seed)
    hexed = f"{high:016x}{low:016x}"
    return f"{hexed[:8]}-{hexed[8:12]}-{hexed[12:16]}-{hexed[16:20]}-{hexed[20:]}"


def _record(index: int) -> dict:
    """One record of the shape most APIs actually send, with long ids."""
    return {
        "productUuid": _uuid(index),
        "poId": _uuid(index + 1000),
        "testCase": (
            "purchase bronet when balance is sufficient via modchan"
            if index % 2
            else "purchase bronet when single active time yes and any offer "
            "validity will increase the active period of the card"
        ),
        "uniqueIdDev": _uuid(index + 2000),
        "statusDev": None,
        "uniqueIdPrd": _uuid(index + 3000),
        "statusPrd": "ERROR" if index % 3 == 0 else "OK",
        "serviceId": str(2000004190 + index),
        "scheduleId": str(2814256 + index),
        "mpeNumber": f"MPEAXR{265359 + index}",
    }


def _records(count: int) -> list:
    return [_record(index) for index in range(count)]


def one_line() -> str:
    """The shape that exposed the clipping: minified, one very long line."""
    return json.dumps(
        {"status": "OK", "code": "00", "message": "Success", "data": _records(25)},
        separators=(",", ":"),
    )


def long_string() -> str:
    """Indented JSON with one enormous string value.

    Pretty-printing shortens every normal line, but a token has no inside to
    break: this is the row that the numbered view cannot show in full.
    """
    token = base64.urlsafe_b64encode(bytes(range(256)) * 8).decode()
    return json.dumps(
        {
            "status": "OK",
            "access_token": token,
            "token_type": "Bearer",
            "expires_in": 3600,
        },
        indent=2,
    )


def many_lines() -> str:
    """Enough lines that numbering them is not worth it."""
    return json.dumps(
        {"items": [{"id": index, "name": f"item {index}"} for index in range(1200)]},
        indent=2,
    )


def huge() -> str:
    """Past the display cap, so the banner has to appear."""
    body = json.dumps(
        {
            "items": [
                {"id": index, "name": f"item {index}", "uuid": _uuid(index)}
                for index in range(12000)
            ]
        },
        separators=(",", ":"),
    )
    assert len(body.encode()) > MAX_BODY_BYTES, "the huge sample must not fit"
    return body


def body_of_kilobytes(kilobytes: int) -> str:
    """A minified body of roughly the requested size, still valid JSON.

    One padded string is the cheapest way to any size: it keeps the payload
    parseable, so the JSON view has something to pretty-print, and it makes the
    long-line case appear at every scale rather than only at 10 KB.
    """
    target = max(kilobytes, 1) * 1024
    # Room for the wrapper keys and the uuid, so the total lands near `target`.
    padding = "x" * max(target - 96, 0)
    return json.dumps(
        {"status": "OK", "id": _uuid(7), "padding": padding},
        separators=(",", ":"),
    )


def error_body(status: int) -> str:
    return json.dumps(
        {
            "status": "ERROR",
            "code": str(status),
            "message": f"sample server: refusing with {status}",
        },
        indent=2,
    )


def json_array() -> str:
    """A top-level array: the other root a JSON body can have."""
    return json.dumps(_records(6), indent=2)


def json_scalar() -> str:
    """The smallest valid JSON. `null` is the case most likely to be mistaken
    for an empty body, which is why it is the one served."""
    return "null"


def invalid_json() -> str:
    """`application/json` that does not parse: the viewer must fall back to the
    raw bytes instead of pretending the pretty printer succeeded."""
    return '{"status": "OK", "data": [1, 2, 3,'


def unicode_json() -> str:
    """Text the width and selection code cannot assume is ASCII: wide CJK
    glyphs, a right-to-left script, a ZWJ emoji sequence, combining marks, and
    one long run with no space to break at."""
    return json.dumps(
        {
            "status": "OK",
            "japanese": "請求書を発行します",
            "arabic": "فاتورة الضريبة",
            "emoji": "🧾👨‍👩‍👧‍👦",
            "combining": "e\u0301e\u0301e\u0301",
            "long_cjk": "請求書" * 60,
        },
        indent=2,
        ensure_ascii=False,
    )


def deep() -> str:
    """Nesting deep enough that the indentation, not the content, is the
    widest thing on the last line."""
    value = {"end": True}
    for level in range(120):
        value = {f"level{level}": value}
    return json.dumps(value, indent=2)


def xml_doc() -> str:
    """Indented XML with one long attribute row, so the XML label and the
    unhighlighted long line are visible in the same body."""
    attributes = " ".join(f'data-{index}="{_uuid(index)}"' for index in range(12))
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        "<catalog>\n"
        f'  <product id="{_uuid(0)}" sku="MPEAXR265359" {attributes}>\n'
        "    <name>bronet</name>\n"
        "    <price currency='IDR'>15000</price>\n"
        "  </product>\n"
        "</catalog>\n"
    )


def html_page() -> str:
    """An HTML page with one row of tags long enough to need the same
    horizontal treatment as a minified JSON line."""
    rows = "".join(
        f'<tr><td>{index}</td><td class="name">{_uuid(index)}</td></tr>\n'
        for index in range(8)
    )
    tags = "".join(
        f'<span class="tag">{_uuid(index)}</span>' for index in range(20)
    )
    return (
        '<!doctype html>\n<html lang="en">\n<head>\n'
        '  <meta charset="utf-8">\n'
        "  <title>restbook sample</title>\n"
        "</head>\n<body>\n"
        "  <table>\n" + rows + "  </table>\n"
        f'  <div class="tags">{tags}</div>\n'
        "</body>\n</html>\n"
    )


def plain_text() -> str:
    """No structure to label or highlight, and one line past any window."""
    words = " ".join(f"word{index}" for index in range(400))
    return (
        "restbook sample text\n"
        "This body has no syntax, so the viewer must not colour it.\n"
        f"\n{words}\n"
        "end of sample\n"
    )


def csv_text() -> str:
    """Commas inside quoted fields and a doubled quote: a body a naive
    one-line-per-row viewer misreads."""
    rows = ["id,name,note"]
    rows += [
        f'{index},"item {index}","comma, and a ""quoted"" word"'
        for index in range(12)
    ]
    return "\n".join(rows) + "\n"


INDEX = """
restbook sample server

  Bodies the viewer has to survive
  /one-line              minified JSON on one line: both views scroll sideways
  /long-string           indented JSON with a ~2.7 KB string: the numbered view scrolls that row
  /many-lines            indented JSON past 3000 lines: unnumbered fallback
  /huge                  minified JSON past 256 KiB: "truncated for display"
  /body?kb=2048          a body of about that size, any size
  /stream?chunks=10      chunked JSON, one record per write
  /abort?chunks=2        chunked JSON cut off mid-body: a short read
  /empty                 204 with no body
  /binary                bytes that are not UTF-8
  /gzip                  gzip-compressed JSON, decompressed by the client

  Formats
  /array                 JSON with a top-level array
  /scalar                JSON `null`
  /invalid-json          application/json that does not parse
  /sniff                 JSON with no Content-Type at all
  /xml                   XML with one long attribute row
  /html                  HTML with one long tag row
  /text                  plain text with one very long line
  /csv                   quoted CSV
  /unicode               CJK, Arabic, emoji, combining marks
  /deep                  JSON nested ~120 levels

  Transport, headers, cookies
  /error?status=503      that status with a JSON body
  /headers?count=40      that many X-Sample-<n> response headers
  /cookies               five Set-Cookie headers, one name repeated
  /redirect?hops=3       a 302 chain ending at /one-line

  Requests
  /echo                  any method, reflected: body, query, headers

  add ?delay=<ms> to any path to hold the response open before it starts
  (?delay=600000 with / is "never answering", for Cancel).
""".strip()


def _write_chunk(handler, data: bytes) -> None:
    """One HTTP/1.1 chunk: hex length, CRLF, payload, CRLF, flushed at once."""
    handler.wfile.write(b"%x\r\n" % len(data) + data + b"\r\n")
    handler.wfile.flush()


def _read_request_body(handler) -> bytes:
    """The request body under either framing.

    The app sends `Content-Length`, but a chunked request is what a streaming
    client would send, so both are read rather than letting one desync the
    connection.
    """
    if "chunked" in handler.headers.get("Transfer-Encoding", "").lower():
        chunks = []
        while True:
            line = handler.rfile.readline().strip()
            size = int(line.split(b";")[0] or b"0", 16)
            if size == 0:
                handler.rfile.readline()  # the trailing CRLF
                break
            chunks.append(handler.rfile.read(size))
            handler.rfile.read(2)  # the CRLF after each chunk
        return b"".join(chunks)
    try:
        length = int(handler.headers.get("Content-Length", "0"))
    except ValueError:
        length = 0
    return handler.rfile.read(length) if length > 0 else b""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802 - the base class names this
        self._handle("GET")

    def do_POST(self) -> None:  # noqa: N802
        self._handle("POST")

    def do_PUT(self) -> None:  # noqa: N802
        self._handle("PUT")

    def do_PATCH(self) -> None:  # noqa: N802
        self._handle("PATCH")

    def do_DELETE(self) -> None:  # noqa: N802
        self._handle("DELETE")

    def do_OPTIONS(self) -> None:  # noqa: N802
        self._handle("OPTIONS")

    def do_HEAD(self) -> None:  # noqa: N802
        if urlparse(self.path).path not in ROUTES:
            self.send_error(404, "no such sample")
            return
        # A HEAD reply carries no body by definition, so the interesting part
        # is unreachable: this only proves the method reaches the pane with a
        # status and headers.
        self.send_response(200)
        self.send_header("Content-Type", JSON)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _handle(self, method: str) -> None:
        url = urlparse(self.path)
        query = parse_qs(url.query)

        if url.path not in ROUTES:
            self.send_error(404, "no such sample")
            return

        if method != "GET" and url.path != "/echo":
            # Every other route is read-only. The body of such a request is
            # unread, so the connection cannot be reused.
            self.send(
                error_body(405),
                status=405,
                headers=[("Allow", "GET, HEAD")],
            )
            self.close_connection = True
            return

        try:
            delay_ms = float(query.get("delay", ["0"])[0])
        except ValueError:
            delay_ms = 0.0
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)

        try:
            ROUTES[url.path](self, query)
        except (BrokenPipeError, ConnectionResetError):
            # A canceled request is a successful test: the client hung up.
            self.close_connection = True

    def send(
        self,
        body: str,
        content_type: str = JSON,
        status: int = 200,
        headers=(),
    ) -> None:
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format: str, *args) -> None:
        # One line per request, so a stuck request is visible while debugging.
        print(f"sample: {format % args}", flush=True)


def index(handler: Handler, _query: dict) -> None:
    handler.send(INDEX, TEXT)


def plain(body, content_type: str = JSON):
    """A route that serves one deterministic string."""

    def route(handler: Handler, _query: dict) -> None:
        handler.send(body(), content_type)

    return route


def sized(handler: Handler, query: dict) -> None:
    """`/body?kb=<n>` — the size is the point, so a bad value is a 400."""
    try:
        kilobytes = int(query.get("kb", ["512"])[0])
    except ValueError:
        handler.send("kb must be a whole number\n", TEXT, status=400)
        return
    if kilobytes < 1:
        handler.send("kb must be at least 1\n", TEXT, status=400)
        return
    handler.send(body_of_kilobytes(kilobytes))


def sniff(handler: Handler, _query: dict) -> None:
    """No `Content-Type` header at all: the body is JSON, so the viewer's own
    sniff has to decide. It pretty-prints, but the format chip is chosen from
    the header and still says "Text" — the one visible inconsistency."""
    data = one_line().encode()
    handler.send_response(200)
    handler.send_header("Content-Length", str(len(data)))
    handler.end_headers()
    handler.wfile.write(data)


def empty(handler: Handler, _query: dict) -> None:
    """204 means "succeeded, nothing to say": no body, no `Content-Length`."""
    handler.send_response(204)
    handler.end_headers()


def binary(handler: Handler, _query: dict) -> None:
    """Every byte value, four times: not valid UTF-8 anywhere, so the lossy
    decode has to produce replacement characters rather than an error."""
    data = bytes(range(256)) * 4
    handler.send_response(200)
    handler.send_header("Content-Type", BINARY)
    handler.send_header("Content-Length", str(len(data)))
    handler.end_headers()
    handler.wfile.write(data)


def gzipped(handler: Handler, _query: dict) -> None:
    """A gzip body: reqwest decodes it and drops `Content-Encoding`, so the
    pane should show plain JSON with only `Content-Type` left to label it.

    `mtime=0` keeps the compressed bytes identical between runs.
    """
    data = gzip.compress(one_line().encode(), mtime=0)
    handler.send_response(200)
    handler.send_header("Content-Type", JSON)
    handler.send_header("Content-Encoding", "gzip")
    handler.send_header("Content-Length", str(len(data)))
    handler.end_headers()
    handler.wfile.write(data)


def cookies(handler: Handler, _query: dict) -> None:
    """One header per cookie, including a repeated name, a quoted value with a
    semicolon in it, and an already-expired one."""
    data = json.dumps({"status": "OK", "setCookies": 5}).encode()
    handler.send_response(200)
    handler.send_header("Content-Type", JSON)
    handler.send_header("Content-Length", str(len(data)))
    for cookie in (
        "session=abc123; Path=/; HttpOnly; SameSite=Lax",
        "theme=dark; Path=/; Max-Age=3600",
        "session=second; Path=/other",
        'prefs="a=b; c=d"; Path=/',
        "legacy=gone; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
    ):
        handler.send_header("Set-Cookie", cookie)
    handler.end_headers()
    handler.wfile.write(data)


def stream(handler: Handler, query: dict) -> None:
    """Chunked JSON, flushed one record at a time.

    No `Content-Length`, so the reply is framed with chunked transfer encoding
    and each write reaches the client as it happens. That is what makes the
    in-flight byte counter move and Cancel land mid-transfer.
    """
    try:
        chunks = max(int(query.get("chunks", ["10"])[0]), 1)
        gap_ms = float(query.get("gap", ["250"])[0])
    except ValueError:
        handler.send("chunks and gap must be numbers\n", TEXT, status=400)
        return

    handler.send_response(200)
    handler.send_header("Content-Type", JSON)
    handler.send_header("Transfer-Encoding", "chunked")
    handler.end_headers()

    _write_chunk(handler, b'{"status":"OK","data":[')
    for index in range(chunks):
        record = json.dumps(_record(index), separators=(",", ":"))
        _write_chunk(handler, (b"," if index else b"") + record.encode())
        time.sleep(max(gap_ms, 0) / 1000.0)
    _write_chunk(handler, b"]}")
    handler.wfile.write(b"0\r\n\r\n")
    handler.wfile.flush()


def abort(handler: Handler, query: dict) -> None:
    """Chunked JSON that stops mid-body.

    A few chunks arrive, then the socket closes without the final chunk or the
    terminating zero-length one. The client has a partial body and no way to
    know it is complete, so this is the route that shows the transport's
    short-read error instead of a quietly truncated response.
    """
    try:
        chunks = max(int(query.get("chunks", ["2"])[0]), 1)
        gap_ms = float(query.get("gap", ["100"])[0])
    except ValueError:
        chunks = 2
        gap_ms = 100.0

    handler.send_response(200)
    handler.send_header("Content-Type", JSON)
    handler.send_header("Transfer-Encoding", "chunked")
    handler.end_headers()

    _write_chunk(handler, b'{"status":"OK","data":[')
    for index in range(chunks):
        record = json.dumps(_record(index), separators=(",", ":"))
        _write_chunk(handler, (b"," if index else b"") + record.encode())
        time.sleep(max(gap_ms, 0) / 1000.0)

    # No `]}` and no terminating chunk: closing here is the point.
    handler.close_connection = True


def echo(handler: Handler, query: dict) -> None:
    """The request reflected as JSON: method, path, repeated query parameters,
    headers, and body.

    This is the only route that accepts a body, and it is what makes POST,
    PUT, PATCH, DELETE, and OPTIONS testable at all — including the request
    pane's own editors.
    """
    raw = _read_request_body(handler)
    body = {
        "method": handler.command,
        "path": urlparse(handler.path).path,
        "httpVersion": handler.request_version,
        "query": [
            [name, value] for name, values in query.items() for value in values
        ],
        "contentType": handler.headers.get("Content-Type"),
        "headers": [[name, value] for name, value in handler.headers.items()],
        "bodyBytes": len(raw),
        "body": raw.decode("utf-8", "replace"),
    }
    handler.send(json.dumps(body, indent=2, ensure_ascii=False))


def error(handler: Handler, query: dict) -> None:
    try:
        status = int(query.get("status", ["500"])[0])
    except ValueError:
        status = 500
    # `send_error` would answer in HTML; a JSON body is what an API client
    # actually has to render.
    handler.send(error_body(status), status=status)


def headers(handler: Handler, query: dict) -> None:
    try:
        count = max(int(query.get("count", ["10"])[0]), 0)
    except ValueError:
        count = 10
    data = json.dumps({"status": "OK", "headers": count}).encode()
    handler.send_response(200)
    handler.send_header("Content-Type", JSON)
    handler.send_header("Content-Length", str(len(data)))
    for index in range(count):
        handler.send_header(f"X-Sample-{index}", f"value-{index}-{'y' * 40}")
    handler.end_headers()
    handler.wfile.write(data)


def redirect(handler: Handler, query: dict) -> None:
    try:
        hops = int(query.get("hops", ["3"])[0])
        status = int(query.get("status", ["302"])[0])
    except ValueError:
        hops = 3
        status = 302
    if status not in (301, 302, 303, 307, 308):
        handler.send("status must be a redirect status\n", TEXT, status=400)
        return
    if hops <= 0:
        one = one_line().encode()
        handler.send_response(200)
        handler.send_header("Content-Type", JSON)
        handler.send_header("Content-Length", str(len(one)))
        handler.end_headers()
        handler.wfile.write(one)
        return
    handler.send_response(status)
    handler.send_header(
        "Location", f"/redirect?hops={hops - 1}&status={status}"
    )
    handler.send_header("Content-Length", "0")
    handler.end_headers()


ROUTES = {
    "/": index,
    "/one-line": plain(one_line),
    "/long-string": plain(long_string),
    "/many-lines": plain(many_lines),
    "/huge": plain(huge),
    "/body": sized,
    "/stream": stream,
    "/abort": abort,
    "/empty": empty,
    "/binary": binary,
    "/gzip": gzipped,
    "/array": plain(json_array),
    "/scalar": plain(json_scalar),
    "/invalid-json": plain(invalid_json),
    "/sniff": sniff,
    "/xml": plain(xml_doc, XML),
    "/html": plain(html_page, HTML),
    "/text": plain(plain_text, TEXT),
    "/csv": plain(csv_text, CSV),
    "/unicode": plain(unicode_json),
    "/deep": plain(deep),
    "/error": error,
    "/headers": headers,
    "/cookies": cookies,
    "/redirect": redirect,
    "/echo": echo,
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"sample: http://{args.host}:{args.port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
