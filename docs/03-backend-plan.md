# restbook — backend plan

The engine behind the UI: domain model, preparation, transport, storage. The UI
side is in [`02-ui-plan.md`](02-ui-plan.md); scope and non-goals stay in
[`00-scope.md`](00-scope.md). For how to change the engine — recipes, commands,
and pitfalls — see [`04-backend-guide.md`](04-backend-guide.md).

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

### Crates we adopt, and why not others

A crate earns its place by replacing hand-rolled code or serving a lane already
planned; the dev-only ones never reach a release build.

| Crate | Replaces / unlocks | Lane |
|---|---|---|
| `bytes` | `Bytes`-backed request and response bodies, zero-copy chunks | B18 |
| `encoding_rs` + `mime` | decode a `text/*` body by its `charset=`, not `from_utf8_lossy` | unassigned (response pane) |
| `serde_path_to_error` | an import error names `resources[2].request.url`, not serde's bare type | 0.2.7 |
| `secrecy` + `zeroize` | secret values redacted in `Debug` and zeroed on drop | 0.3.2 |
| `notify` | workspace file watch | B24 |
| `insta`, `proptest`, `tempfile` (dev) | import snapshots, curl-tokenizer properties, real temp dirs | test only |

The popular alternatives that target a ruled-out feature stay out, for the same
reason `00-scope.md` names the feature: a template engine (`handlebars`,
`minijinja`, `tera`) or a wasm template runtime (`yaak-templates`) turns
`{{var}}` into scripting, an ORM, query builder, or connection pool (`diesel`,
`sqlx`, `sea-query`, `r2d2`) serves a multi-user server, an RPC bridge
(`yaak-rpc`) exists only to host plugins, a JS runtime (`deno_core`, `boa`,
`rquickjs`) is the plugin runtime, a web framework (`axum`, `actix`) serves no
endpoint, and `tracing` + `opentelemetry` is telemetry.

## Where each stage lives

The pipeline diagram names types, not directories. This is the map from a stage
to the crate and file that implements it, so "the transport" also tells you
where `prepare()` and the store are. The table above says what each crate may
not do; the app's full directory tree is in
[`11-repo-map-and-dev-cycle.md`](11-repo-map-and-dev-cycle.md#repository-map).

| Stage | Crate | Directory | Key files | What it does |
|---|---|---|---|---|
| `RequestDef`, `Method`, `Kv`, `Body`, `Auth` | `core` | `crates/core/src/` | `model.rs` | the unresolved request and every collection/environment shape |
| `prepare()` → `PreparedRequest` | `core` | `crates/core/src/` | `prepare.rs` | pure fold: vars, query, auth, body encoding, URL validation |
| `{{var}}` resolution | `core` | `crates/core/src/` | `template.rs`, `vars.rs` | placeholder substitution and the scope lookup |
| `{{$…}}` generators | `core` | `crates/core/src/` | `generator.rs` | `$uuid`, `$timestamp`, `$random*`, expanded at send time |
| `Transport` trait | `core` | `crates/core/src/` | `transport.rs` | the `Transport` / `CancelToken` / `Progress` contract |
| `ReqwestTransport` | `transport` | `crates/transport/src/` | `lib.rs` | reqwest adapter, client cache, shared cookie jar, tokio runtime |
| `ResponseRecord` / `ResponseBody` | `core` | `crates/core/src/` | `model.rs` | what came back; `Memory` bytes or a `File` body |
| `Store` trait | `core` | `crates/core/src/` | `store.rs` | `Store`, `HistoryEntry`, `HistoryHit`, `Origin`, `Draft` |
| Store implementations | `store` | `crates/store/src/` | `lib.rs`, `jsonl.rs`, `folder.rs`, `sqlcipher.rs` | memory, plaintext JSONL, folder workspace, encrypted database |
| Headless harness | `devcli` | `crates/devcli/src/` | `main.rs` | `send` / `history` / `import` / `export` over the pipeline |
| Import adapters | `core` | `crates/core/src/import/` | `curl.rs`, `har.rs`, `postman.rs`, `openapi.rs` | a foreign format → `RequestDef` |
| Errors | `core` | `crates/core/src/` | `error.rs` | typed failures the UI renders verbatim |
| Fake transport | `core` | `crates/core/src/` | `testing.rs` | an offline `FakeTransport` for engine tests |
| Send orchestration | `app` | `crates/app/src/workspace/` | `send.rs` | the only glue: prepare → transport → history → `Document` notify |
| Panes | `app` | `crates/app/src/panels/` | `request.rs`, `response.rs`, … | content and events; never a dock or a sibling |

Everything above `app` is the engine and builds with `cargo` at the repository
root. The shell is its own workspace under `crates/app`, so a root build never
compiles GPUI and an app build never sees the engine's test targets.

## Milestones

The engine milestone table (`B` items) and what is deliberately deferred live in
[`01-milestones.md`](01-milestones.md). This document keeps the pipeline, the
crate boundaries, and the constraints a milestone has to respect.

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
