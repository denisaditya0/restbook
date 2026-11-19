# restbook — backend plan

The engine behind the UI: domain model, preparation, transport, storage. The UI
side is in [`01-ui-plan.md`](01-ui-plan.md); scope and non-goals stay in
[`00-scope.md`](00-scope.md). For how to change the engine — recipes, commands,
and pitfalls — see [`03-backend-guide.md`](03-backend-guide.md).

## The pipeline

One path, no exceptions:

```
RequestDef ──prepare()──► PreparedRequest ──Transport──► ResponseRecord
                              │
                              └──► HistoryEntry ──Store──► encrypted database
```

`prepare()` is pure: it resolves `{{vars}}`, folds params into the query string,
adds auth, picks the body encoding, and validates the URL — before a single byte
of I/O. An invalid request therefore produces a typed error and nothing is sent.
Since U30 it also expands `{{$…}}` generators (`$uuid`, `$timestamp`, `$random*`)
from a `Generators` value; `prepare_with()` takes one so a test can fix the clock
and the seed.

## Crate boundaries

| Crate | Owns | Must not |
|---|---|---|
| `core` | model, templating, `prepare()`, `Store`/`Transport` traits | depend on GPUI, reqwest, or a runtime |
| `transport` | reqwest adapter, tokio runtime on a dedicated thread | know about the UI or storage |
| `store` | `MemoryStore`, `JsonlStore`, `FolderStore`, `SqlcipherStore` | own the keyring policy of the app |
| `devcli` | a harness that exercises the pipeline | be the only way to test it |

`core` defines the traits and nothing implements them there. That is what lets
the engine be tested headlessly and the UI be replaced.

## Milestones

`done` means it is on disk, builds, and is covered by a test or exercised by
`devcli`.

| # | Deliverable | State |
|---|---|---|
| B1 | Workspace, `RequestDef`/`Body`/`Auth`/`ResponseRecord`, `{{var}}` templating | done |
| B2 | `prepare()`: params, headers, all body kinds, auth, timeout, redirects, TLS, header-injection guard | done |
| B3 | reqwest transport owning its tokio runtime; cancellation | done |
| B4 | `Store` trait; `MemoryStore` + `JsonlStore` | done |
| B5 | `SqlcipherStore` over SQLCipher, key from OS keyring, search by URL | done, feature-gated |
| B6 | `Store::get_history(id)` returning the full `HistoryEntry` | done |
| B7 | Restoring a history entry into the editor with body and auth intact | done |
| B8 | `Store` trait: collections, folders, environments as persisted rows | done |
| B9 | Variable scope resolution: precedence between global, environment, request, and live state | done (global + environment) |
| B10 | Non-strict preview mode: render the request with unresolved vars marked, send disabled | done |
| B11 | Proxy, custom CA, mTLS, redirect metadata in the record | done |
| B12 | Cookie jar shared with the transport | done |
| B13 | Import (curl, HAR, Postman v2.1, OpenAPI 3) and export | done |
| B14 | OAuth 2.0 (PKCE, client credentials) | done (client credentials + code exchange; app wires client credentials) |
| B15 | Progress reporting: `Progress` sink + `Transport::execute_with_progress` | done |
| B16 | Offline test seam: `FakeTransport` + transport integration tests | done |
| B17 | `FolderStore`: a Git-friendly workspace on disk | done |
| B18 | Streamed response bodies to disk (large downloads, resume) | step 1 done: `ResponseBody::{Memory, File}` enum; transport still memory-only |

### Planned (B19..)

Engine work only: each item lands in `core`, `transport` or `store` behind an
existing trait, so it is testable without a window (and usually without a
socket). A UI half, where one exists, is named.

| # | Deliverable | Seam and notes |
|---|---|---|
| B19 | Streamed bodies to disk, step 2 | `Transport::execute_with_progress` already counts bytes; it must spill past a threshold into `ResponseBody::File` instead of `bytes()`, honour a resume offset, and report the path in `ResponseRecord`. UI half: U57 |
| B20 | OAuth 2.0 authorization-code flow | `core/src/oauth.rs` builds the authorize URL and exchanges a code with PKCE, and `OAuthToken::refresh_token` is parsed but never used. Missing: a loopback redirect listener, the browser hand-off, refresh-on-expiry, and the app wiring (only client credentials are wired today) |
| B21 | History search and retention | `Store::search_history` is a URL `LIKE` today. FTS5 over URL/name/body, keyset pagination, and a retention pass (age and count) so the encrypted database does not grow without a bound |
| B22 | Collection and request scopes in `vars.rs` | Scope precedence stops at Global + Environment (B9). Adding the two narrower scopes gives U34 its data, and the inspector's `secret_of` follows the same table |
| B23 | Streamed uploads | A multipart part or a `Body::Binary` path is read whole into memory at prepare time (`PreparedBody::File`). Streaming from a path keeps a large upload out of RAM; the same threshold setting as B19 applies |
| B24 | `FolderStore` external-change detection | A Git-friendly workspace can be edited by other tools while the app holds a snapshot. Detecting a changed file (mtime/hash) before a write is the difference between a merge and silent clobbering |

### Network settings (B11 + B12)

`core/src/net.rs` holds `NetworkConfig`: `proxy`, `extra_roots` (PEM
certificates), `client_cert` (a PEM certificate/key pair for mTLS), and
`default_timeout_ms`. `core` stays reqwest-free by default; `validate()` checks
the PEM blocks and parses the proxy URL only under the `network-validation`
feature, which `restbook-transport` enables. A bundle of certificates is read
permissively, so an empty anchor list is an error rather than a silently liberal
trust store.

`ReqwestTransport::set_config` takes a new configuration and rebuilds its client
on the next send; `ClientCache` holds the trust and insecure variants behind a
generation counter, so changing a setting does not rebuild two clients per
request.

Redirects are followed **by hand** (`redirect::Policy::none`) so each hop is
recorded. `ResponseRecord` gained `final_url` and `redirects: Vec<RedirectHop>`,
which is what lets the UI show `301 → 302 → 200` instead of only the destination.
A 303, and a 301/302 in reply to anything but GET/HEAD, drop the body and the
entity headers on the way through.

Cookies live in one `SharedJar` per transport, a `cookie_store::CookieStore`
behind reqwest's `CookieStore` trait. reqwest's own `Jar` cannot be read or
cleared, which is why it is not used: the `Transport` trait exposes `cookies(url)`
and `clear_cookies()` so the UI can list and forget them.

### Progress reporting (B15)

`core::transport::Progress` is a cheap, clonable sink: sent/received byte
counters, an elapsed stamp, and a `headers_received` flag, all atomics behind an
`Arc` so the transport can bump them from its I/O thread while the UI reads them.
`Progress::snapshot()` is the read side and never blocks.

The trait gained `execute_with_progress(request, cancel, progress)`.
`ReqwestTransport` overrides it: it streams the response body in chunks (rather
than `response.bytes()`), marking headers and bumping `received` as data arrives,
and reports a `Content-Length` denominator — suppressed for a response that
advertises a content encoding, because that length counts compressed bytes and
would make the counter overshoot. The whole body is still returned in the
`ResponseRecord`; this is progress reporting, not streaming-to-disk (see B18).

`execute` still exists and defaults to `execute_with_progress(.., Progress::new())`,
so a transport that cannot report progress only implements `execute`. `devcli send
--progress` drives it from a terminal and prints the counters to stderr.

### The offline test seam (B16)

`core::testing::FakeTransport` answers from a scripted `Reply` (`Status`, `Json`,
`Record`, `Error`) or a handler closure, and records every `PreparedRequest` it
was asked to send. It is always compiled — no dependencies — so a test can run
the whole pipeline (plus a `Store`) deterministically with no socket. It is the
counterpart to zaku's `FakeHttpClient`.

The real `ReqwestTransport` is covered by integration tests that drive a bare
`TcpListener`: a real send with progress assertions, upload-size reporting, and a
cancellation test against a server that stalls mid-body. Between the two, the
`Transport` seam is exercised both offline and over a socket.

### Folder workspaces (B17)

`store::FolderStore` is the Git-friendly shape: one file per collection under a
workspace root, marked by `restbook.json`, with a generated `.gitignore` that
keeps the tracked tree free of secrets. Local-only state (the active
environment, global variables) goes to `restbook.local.json`, and history to a
`.restbook/` state directory — both gitignored. `devcli export <file> --workspace
<dir>` writes an import into one. It sits beside `JsonlStore` and `SqlcipherStore`
behind the same `Store` trait; the app still ships SQLCipher as its default.

### Import and export (B13)

Every format is translated into the same one thing — `import::BulkImport`, a list
of `Collection`s, a list of `Environment`s, the names that were already present,
and a `Skip { at, reason }` per item that could not be read. Importers never
touch the `Store`: the caller saves each collection, and a partial import is
explained rather than refused.

| Format | Reads | Writes |
|---|---|---|
| curl | one command: URL, verb, headers, body, form parts, basic auth | — |
| Postman v2.1 | folders, headers, raw/urlencoded/formdata/file bodies, basic/bearer/apikey auth | yes, one collection |
| HAR 1.2 | the newest `limit` entries, minus assets and transport headers, grouped by host | yes |
| OpenAPI 3 | one request per operation: `{{param}}` paths, required query rows, a JSON skeleton from required properties | — |
| bundle | this app's own file: every collection and environment | yes |

`ImportFormat::detect` tells the JSON formats apart by shape, since one suffix
covers three of them. OpenAPI is import-only on purpose: generating a document
from stored requests would be inventing a description of an API, and a wrong
schema is worse than none.

`devcli import <file>` parses and reports without saving:

```
$ devcli import api.yaml --verbose
OpenApi: Imported 3 requests
Petstore:
  /pets/{petId}:
    GET Read a pet  https://api.pets.test/v1/pets/{{petId}}
```

One behaviour worth knowing when reading the HAR tests: deduping keys on
`METHOD url body`, not `METHOD url`. A browser replays a URL with different
payloads, and collapsing those would silently drop requests.

### OAuth 2.0 (B14)

`core/src/oauth.rs` is pure shape-building, like the rest of the engine:
`OAuthConfig::authorize()` returns the URL the browser opens plus the `state`
and PKCE verifier to keep, `client_credentials_request()` and
`code_exchange_request()` return a `TokenRequest` (a form plus optional HTTP
basic client auth), and `parse_token_response()` reads the reply, turning an
`{"error": …}` body into `Error::OAuth` rather than a serde message.

`OAuthConfig` defaults `pkce` to `true`; a hand-written `Default` is what makes
that true, since a derive would yield `false`. The verifier and state come from
`Uuid::new_v4()` rather than a new RNG dependency.

`prepare()` never performs I/O. It adds `authorization: Bearer …` only when the
request's `Auth::OAuth2` already carries an `access_token`; otherwise it fails
with `Error::OAuth`, and cURL export emits the header only when a token exists.
The token is `serde(skip)`, so it is never written to a saved request.

The transport implements `OAuthTransport::fetch_token` by POSTing the form to
the token URL with the same client, proxy, and TLS policy as a normal send, and
prefers an OAuth error body over the HTTP status. `devcli oauth` drives both
grants; the app's Auth tab fetches client-credentials tokens, and the
authorization-code browser flow is the one part still deferred (see the
Known gaps in [`00-scope.md`](00-scope.md)).

### How a history row restores (B6 + B7)

`search_history` returns `HistoryHit` rows: method, URL, status, timing — enough
to draw the list, not enough to re-send. The full request is recovered with:

```rust
fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>>;
```

`MemoryStore` indexes its vector, `JsonlStore` scans, and `SqlcipherStore` joins
`requests` and `executions`. The request itself is persisted as a whole
`RequestDef` in `requests.request_json`; rows written before that column existed
fall back to method, URL, and body, so the restore degrades instead of failing.

On the UI side, `HistoryEvent::Picked { id: i64 }` carries the row id, and
`workspace/` asks the store for the entry, hands the `RequestDef` to the editor
(`RequestPanel::adopt_request`), and puts the stored `ResponseRecord` in the
document without sending. The history pane does not change beyond the event
payload — it already owns the store. See
[`01-ui-plan.md`](01-ui-plan.md#ordering-notes).

### Why B9 and B10 are one design

Scope precedence has to be visible, not implicit. B9 decides *what a name
resolves to*; B10 shows the user *which layer won* and what is still unresolved.
Splitting them produces a resolver with no way to explain itself, which is the
usual reason people distrust environment features.

Both are on disk now. `crates/core/src/vars.rs` holds `ScopedVars`: five layers
in fixed precedence, `lookup()` for the winner, `context()` for `prepare()`, and
`preview()` for the non-strict render. `crates/app/src/variables.rs` pushes
`Global` and `Environment`; `Collection`, `Request`, and `Live` exist but have
no producer yet. The workspace builds the context per send, and the request pane
lists unresolved names and keeps Send inert while any exist.

## Kept and deferred deliberately

- **Plaintext JSONL.** `devcli` writes plaintext by design; only the app path is
  encrypted. The store crate must not decide which is in use.
- **No scripting.** No `pm.*`, no JS engine. This is a product boundary, not a
  scheduling decision.
- **OAuth's browser listener.** The engine builds the authorize URL, derives the
  PKCE challenge, exchanges a code, and fetches client-credentials tokens; the
  app fetches client-credentials tokens and shows the settings. The local
  redirect listener that completes the authorization-code flow in-app is the
  one piece still deferred.

## Streamed bodies to disk (B18, in progress)

Step 1 has landed: `ResponseRecord.body` is now a `ResponseBody` enum with a
`Memory(Vec<u8>)` variant and a `File(FileBody)` variant. The enum is
`#[serde(untagged)]`, so a memory body still serialises as a bare array and
every record written before the enum parses unchanged; a small test pins that
shape. Nothing produces a `File` body yet — `FileBody` carries `path`, `len`,
`preview`, `etag`, and `resume_from` so the schema is ready before step 4.

Readers go through `as_bytes()` (memory bytes, or a file's preview) and `len()`
(the file's full size), so the response pane already renders a file body's
preview. The remaining steps:

Today `ResponseRecord.body` is `Vec<u8>`: every response is buffered whole, and
history keeps it. That is right for the common case (progress reporting, B15,
gets the UI benefit without moving the bytes) and wrong for multi-hundred-MB
downloads. The intended shape when it is needed:

- Split the bytes from the record. `ResponseRecord.body` becomes an enum:
  `Memory(Vec<u8>)` or `File { path, len, preview, etag, resume_from }`, where
  `preview` is the first few KiB kept so the response pane can render something
  without a re-read.
- The sink is chosen by a `ResponsePolicy` decided *before* the response arrives
  (so streaming can start at the first chunk, since `Content-Length` is often
  absent or wrong): buffer up to `memory_limit`; on crossing it, open the file,
  flush what was buffered, and keep writing.
- Raw, not decoded. Streaming a `.tar.gz` byte-exact means per-request opt-out of
  reqwest's content decoding; a down side is that `Content-Length` then matches
  and resume is possible.
- Resume is the real payoff, so the schema carries `etag`/`resume_from` from day
  one; a retry sends `Range: bytes=N-` and appends.
- The store stops owning the bytes, which is the risky part: the `bodies/`
  directory needs a GC sweep, cancelled streams leave partials to clean up, and
  a history row with a file body shows a preview plus actions, not a body viewer.

Rolled out in small steps, each independently testable: (1) introduce the enum
with `Memory` only (pure refactor); (2) add `preview_bytes` + a `SendOptions` at
the transport seam, still memory-only; (3) implement spill-over against
`FakeTransport` first, so the mid-stream branch is tested with no disk; (4) the
real `File` sink in `ReqwestTransport`, behind an opt-in policy; (5) the store
migration and GC sweep; (6) the response pane branches on the body kind. Request
uploads are a separate axis and are not part of this.

## Known constraints on this machine

- TLS is `native-tls` (SChannel): `rustls`/`ring` need a C compiler, and there is
  no MSVC Build Tools.
- The `sqlcipher` feature needs `OPENSSL_DIR`. The app enables it by default and
  `scripts/dev.sh` detects the prefix; the store crate keeps it opt-in so engine
  tests stay dependency-light.
- The app crate is a separate workspace, so GPUI never enters the engine build.
