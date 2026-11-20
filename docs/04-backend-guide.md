# restbook — backend developer guide

The engine: model, preparation, transport, storage. This guide is for changing
these crates. The desktop shell has its own guide in
[`05-ui-guide.md`](05-ui-guide.md); plans and scope stay in
[`03-backend-plan.md`](03-backend-plan.md) and [`00-scope.md`](00-scope.md), and
the per-OS build differences (crypto backend, keyring, toolchain pin) are in
[`06-cross-platform.md`](06-cross-platform.md).

## The two rules

1. **`core` owns the contracts, never the implementations.** `Transport`,
   `Store`, `prepare()`, the model, and `{{var}}` resolution live there.
   `core` must not depend on GPUI, reqwest, tokio, or a filesystem.
2. **`prepare()` is pure.** No I/O, no network. It resolves variables,
   folds params into the URL, adds auth, encodes the body, and validates
   headers. Its only nondeterminism is the send-time generators — a clock read
   and a seeded counter — and `prepare_with(request, ctx, &generators)` lets a
   test fix both, so the pipeline stays reproducible without a server.

## Build, test, drive

From the repository root:

```sh
cargo build
cargo test
cargo clippy --all-targets --message-format short
```

On macOS and Linux the `sqlcipher` build uses CommonCrypto and the system
OpenSSL respectively, so no environment variable is involved; see
[`06-cross-platform.md`](06-cross-platform.md).

Anything that compiles SQLCipher goes through a `dev` script instead (it is
just cargo with `OPENSSL_DIR` detected). Use `scripts/dev.ps1` or
`scripts/dev.cmd` on Windows without `sh`:

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher
```

The command-line harness is the fastest way to exercise a change end to end:

```sh
cargo run -p restbook-devcli -- send https://httpbin.org/get
cargo run -p restbook-devcli -- send https://httpbin.org/post -X POST --json '{"hello":"world"}'
cargo run -p restbook-devcli -- send https://httpbin.org/bearer --var TOKEN=abc \
  -H 'Authorization:Bearer {{TOKEN}}'
cargo run -p restbook-devcli -- send https://httpbin.org/bytes/1048576 --progress
cargo run -p restbook-devcli -- history httpbin
```

`--progress` sends through `Transport::execute_with_progress` and prints live
sent/received byte counts to stderr, which is how the progress path (B15) is
exercised without a window.

`devcli` writes plaintext JSONL to `.restbook/history.jsonl` in the current
directory. That is a development convenience, unrelated to the app. The app
writes to a per-user data directory — `%APPDATA%\restbook` on Windows,
`~/Library/Application Support/restbook` on macOS, `$XDG_DATA_HOME/restbook`
elsewhere — overridable with `RESTBOOK_DATA_DIR`. `crates/app/src/workspace/storage.rs`
owns that decision, including a one-time migration of legacy
`.restbook/history.jsonl` files.

The encrypted store needs an OpenSSL dev tree: `libsqlite3-sys` reads
`OPENSSL_DIR` and panics without it. The `dev` scripts detect a prefix on this
machine and forward the command, so the checked-in docs never carry a personal
path (`dev.ps1`/`dev.cmd` are the native Windows twins):

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher

cd crates/app
sh ../../scripts/dev.sh run          # sqlcipher is a default feature there
```

Set `OPENSSL_DIR` yourself to override the detection. The store crate keeps
`sqlcipher` opt-in so `cargo test` at the root stays light; the app enables it by
default and falls back to plaintext JSONL (with a stderr warning) when the
keyring is unavailable, or entirely when built with `--no-default-features`.

Where a test goes and which seam to use is [Testing](#testing) below; this
section is about running the commands.

## Testing

Tests live **in the file they test**, at the bottom, in a module the compiler
only builds under `cargo test`:

```rust
// ... the code ...

#[cfg(test)]
mod tests {
    // A named import is optional here; `use super::*` is fine when the module
    // has no conflicting glob. The UI crate cannot use the glob at all.
    use super::matching;

    #[test]
    fn an_empty_query_returns_the_newest_rows() { /* ... */ }
}
```

There is no `tests/` directory, and no `_test.rs` sibling. A child module sees
its parent's private items, so a unit test reaches `fn matching` without the
function becoming `pub`; a file in `tests/` would be a separate crate and could
only use the public API. Add `tests/` only for something that genuinely needs
two crates at once (a store written by the real transport, say).

`#[test]` is the marker, so names are sentences about behavior —
`an_empty_query_returns_the_newest_rows`, not `test_matching_1` — and they act
as the failure message when the assertion does not explain enough.

Use the seams instead of a network or a real directory:

| Need | Use |
|---|---|
| a send without a socket | `core::testing::FakeTransport` (see [above](#test-the-pipeline-without-a-network)) |
| a store without a file | `restbook_store::MemoryStore` |
| deterministic generators | `prepare_with(request, ctx, &generators)` |
| a store that really writes | `std::env::temp_dir()` + a per-test subdirectory, created fresh (the `temp_dir()` helper at the bottom of `jsonl.rs`) |

The encrypted store is feature-gated on purpose, so a plain `cargo test` at the
root never needs OpenSSL. Exercise it through the wrapper:

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher
cargo clippy --all-targets --message-format short   # lints tests too
```

Filter with a substring or a module path; a failing test prints the assertion
and the `file:line` that panicked:

```sh
cargo test prepare          # every test whose path contains "prepare"
cargo test -- --nocapture   # keep println!/log output
```

## The map

| Crate | Files | Owns |
|---|---|---|
| `core` | `model.rs` | `Method`, `Kv`, `Body`, `Auth`, `RequestDef`, `ResponseRecord`, `Environment`, `FormPart` |
| | `template.rs` | `VarContext` (scoped lookup, last scope wins) and `resolve()` |
| | `vars.rs` | `ScopedVars`: the five layers, `ResolvedVar` (value + owning scope), `names()` for completion |
| | `generator.rs` | `Generators`: the `$`-token values (`$uuid`, `$timestamp`, `$random*`) and their descriptions |
| | `prepare.rs` | `prepare()` and the `Prepared*` types |
| | `transport.rs` | the `Transport` trait, `CancelToken`, and the `Progress`/`ProgressSnapshot` sink |
| | `store.rs` | the `Store` trait (`append_history`, `search_history`, `get_history`), `HistoryEntry`, `HistoryHit` |
| | `error.rs` | `Error`, `Result` — messages are user-facing |
| | `net.rs` | `NetworkConfig`, `Proxy`, `ClientCert` (B11) |
| | `import/` | `BulkImport`, `Bundle`, and one module per format (B13) |
| | `testing.rs` | `FakeTransport`, the offline `Transport` for tests and previews (B16) |
| `transport` | `lib.rs` | `ReqwestTransport`: one tokio runtime on its own thread, pooled clients, `SharedJar` |
| `store` | `jsonl.rs`, `folder.rs`, `sqlcipher.rs` | `JsonlStore` (dev/fallback), `FolderStore` (Git workspace), `SqlcipherStore` (app, feature-gated), `MemoryStore` |
| `devcli` | `main.rs` | the harness; never a product feature |

Dependencies point one way: `devcli` -> `store`/`transport` -> `core`.

## The pipeline

```
RequestDef ──prepare(&VarContext)──► PreparedRequest ──Transport::execute──► ResponseRecord
                                          │
                                          └──► HistoryEntry ──Store::append_history──► storage
```

`prepare()` in order:

1. resolve the URL and parse it (`Error::InvalidUrl`);
2. resolve and validate each enabled header (`check_header` rejects `:` in a
   name and any `\r`/`\n` — request splitting is a bug, not a feature);
3. resolve enabled params and append them to the query string;
4. apply `Auth` (basic becomes `authorization: Basic base64(user:pass)`, bearer
   and API keys follow the same seam);
5. encode the body: raw with a MIME, form-urlencoded pairs, multipart parts, or
   a file path.

Unknown variables are an error, not an empty string. A silently blank URL or
credential is worse than a visible failure; the UI relies on this.

## Recipes

### Add a field to the model

1. Add it to the struct/enum in `crates/core/src/model.rs`.
2. Give it `#[serde(default)]` (or a default function) if old exports should
   still load. `RequestDef` is an import/export shape, so compatibility matters.
3. Teach `prepare()` what the field means. If it only affects the UI, it still
   belongs on the model so it survives export.
4. Add a test in `prepare.rs` that asserts the request that would be sent — the
   `PreparedRequest` is the boundary, not the `RequestDef` you passed in.
5. Add the control to the app editor (see the UI guide's "add an editor tab").

### Change what a request sends

Everything is in `prepare.rs`. Keep it pure: no `reqwest`, no file reads beyond
what a multipart part names, no clocks. Write the test first, then run

```sh
cargo test -p restbook-core
cargo run -p restbook-devcli -- send ... # against httpbin for the real thing
```

### Add a store

Implement three methods:

```rust
fn append_history(&self, entry: &HistoryEntry) -> Result<i64>;
fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>>;
fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>>;
```

`MemoryStore` is the reference implementation; `JsonlStore` shows file locking;
`SqlcipherStore` shows schema + the keyring boundary. Return `Error::Store` for
storage failures instead of panicking, and never decide *whether* to encrypt
inside the store — that is the app's policy, passed in as a path/key.

Empty `query` means "most recent", and ordering is newest-first in every
implementation, because the history pane does not sort. `get_history` answers
`Ok(None)` for an unknown id; the UI treats that as a stale row, not a failure.

### What a history row restores

`search_history` returns `HistoryHit`s — enough to draw the list, not to
re-send. The full request comes from `Store::get_history(id)`, which returns the
`HistoryEntry` the row was recorded from.

`SqlcipherStore` persists the whole `RequestDef` as JSON in
`requests.request_json`. Older databases are migrated with an `ALTER TABLE ...
ADD COLUMN` whose duplicate-column error is ignored; rows written before the
column existed fall back to method, URL, and body, so a restore degrades instead
of failing. If you add a field to `RequestDef`, nothing here changes — the JSON
carries it, provided `#[serde(default)]` is set (see "Add a field to the model").

On the app side the request is written into live widgets by
`RequestPanel::adopt_request`, and the stored response is shown without sending.

### Add a transport

Implement `Transport::execute`. The one hard constraint is runtime isolation:
reqwest needs a tokio reactor and GPUI has its own executor, so the transport
owns the runtime and returns plain futures. Cancellation is cooperative —
`CancelToken` is polled between chunks; `ReqwestTransport` polls every 25 ms.

Progress is optional: if you can report it, override `execute_with_progress` and
bump the `Progress` sink as bytes move; if not, implement only `execute`, which is
what the default `execute_with_progress` calls.

### Test the pipeline without a network

`core::testing::FakeTransport` is the offline half of the `Transport` seam. Give
it a `Reply` or a handler, hand it to whatever drives the engine, and assert on
what it recorded:

```rust
use restbook_core::{testing::{FakeTransport, Reply}, prepare, CancelToken, Transport, VarContext};

let transport = FakeTransport::always(Reply::Json("{\"ok\":true}".to_owned()));
let prepared = prepare(&request, &VarContext::new())?;
let record = transport.execute(prepared, CancelToken::new()).await?;
assert_eq!(transport.send_count(), 1); // what prepare() handed the wire
```

Use it for anything above the socket; keep a real-listener test (in
`crates/transport/src/lib.rs`) for the send path itself. `core`'s fake-transport
tests need `tokio` with `rt` + `macros`, which is a dev-dependency there.

### Environments and variables

`ScopedVars` (in `crates/core/src/vars.rs`) is the resolver. It holds one layer
per scope, fixed precedence lowest-first — Global, Collection, Environment,
Request, Live — and answers three questions:

- `lookup(name)` — the winning value *and* the scope that provided it, so the UI
  can explain precedence;
- `context()` — a `VarContext` for `prepare()`, built so it agrees with
  `lookup()`;
- `preview(text)` — non-strict rendering: known names are substituted, unknown
  names stay literal and come back as `VarPreview { value: None }`.

`crates/app/src/variables.rs` owns the state above the store: environments,
globals, and the active selection. It pushes Global and Environment; nothing
produces the Collection, Request, or Live layers yet, so they stay empty. Every
mutation is store-first, mirroring the store's answer into memory only after it
succeeds.

The UI is two layers of the same design: the Environment pane edits the rows,
and its inspector walks the live request (`reference_fields` / `references` in
`variables.rs`), previews each field, and labels every name with the scope that
won — or `unresolved`, in the danger color. The request pane reuses the same
helpers to disable Send and name what is missing before `prepare()` can fail.

`ScopedVars::names()` returns every name in every non-empty layer, narrowest
first then alphabetical, as `ResolvedVar { name, value, scope }` — so the
completion list can say *where* a name comes from (and mask a secret value)
rather than only its label. It only lists Global and Environment for the same
reason the layers are empty: nothing produces the other three yet.
`crates/app/src/completion.rs::suggestions` is the one function both completion
surfaces call; it appends `Generators::names()` after the variables, and skips
variables entirely when the typed query starts with `$`, because a `$`-token can
only ever be a generator.

### Generators

A generator is not a stored value: it is the clock plus a seeded counter.
`Generators` (`crates/core/src/generator.rs`) holds `now: DateTime<Utc>`, a
`seed`, and a `Cell<u64>` of draws, so one instance serves a whole request and
still advances between two `{{$uuid}}` references. `prepare()` builds
`Generators::now()`; `prepare_with()`/`resolve_with()` take one, which is the
test seam for a fixed clock (`Generators::at(instant, seed)`).

Randomness is a SplitMix64 loop, not a crate: the values only have to be
distinct within a request, and a new dependency for that is not worth it. A
`$`-token never consults a scope, so a user variable cannot shadow it. Adding a
generator is one arm each in `call`, `describe`, and `names`; the tests assert
that every listed name is callable and described, so the three cannot drift.

### Network settings

```rust
let config = NetworkConfig {
    proxy: Proxy::FromEnvironment,
    extra_roots: Some(anchors), // PEM text of extra CA certificates
    client_cert: Some(ClientCert { cert_pem, key_pem }),
    ..NetworkConfig::default()
};

transport.set_config(config)?;
```

`validate()` is what a settings pane calls before saving, so a bad PEM is
reported at the dialog rather than at the next Send. It only does real work
under the `network-validation` feature (which the transport enables), so `core`
builds without reqwest otherwise. `set_config` builds the next client lazily on
the following send; there is nothing else to call.

### Import a format

Add the file to `import/`, return a `BulkImport`, and declare the module in
`import.rs`. Keep two rules:

- do not touch the `Store` — the caller saves what came back;
- report a bad item as `Skip { at, reason }`, and only refuse the whole file when
  there is nothing to import at all.

```rust
let import = import::parse(ImportFormat::Har, &text, Options::default())?;
for collection in &import.collections {
    store.save_collection(collection)?;
}
println!("{}", import.summary());
```

A format that can also write gets an `export` in its module plus a variant in
`ExportFormat`, and a round-trip test — export then import, asserting the
requests survive. Add a `devcli import` run to the plan's "exercised by devcli"
check while you are there.

## Conventions and pitfalls

- Error strings are rendered directly in the window; write them for a person
  ("invalid url: ...", not `ParseError { .. }`).
- Tests are behavior sentences (`rejects_header_injection`), not method names.
- `core` tests must not need a network or a window. The one exception is the
  `SqlcipherStore` test, which is feature-gated and touches the keyring.
- JSONL is plaintext. It is the devcli format and the app's *fallback* when the
  `sqlcipher` feature is off; if you ship a change that touches history, make
  sure the encrypted path is the one you exercised (`--features sqlcipher`).
- `Body::Multipart` file parts are paths resolved at prepare time; the transport
  opens them. Keep new body kinds on the same pattern.
- The `sqlcipher` build needs `OPENSSL_DIR`. `scripts/dev.sh` detects it; an
  explicit value in the shell still wins. The store crate keeps the feature
  opt-in, so a root `cargo test` never touches OpenSSL.
- **`keyring` 3 has no default backend.** Without one it compiles `pub use mock`,
  an in-memory store: the master key changes on every launch and the encrypted
  database becomes unreadable on the next start. `crates/store/Cargo.toml`
  enables `windows-native` / `apple-native` / `sync-secret-service` per target.
  If history suddenly fails to decrypt, check that first — the key lives in
  Windows Credential Manager under service `restbook`, account `master-key`.
- Windows needs the GNU toolchain and MinGW on `PATH` (README). Native TLS is
  deliberate; `rustls` would need a C compiler here.
