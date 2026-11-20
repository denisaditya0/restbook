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
