# restbook — Rust primer

The Rust this repository actually uses, for someone arriving from TypeScript,
Python, Java, C#, or Go. It is not a language reference; it is the handful of
ideas that explain why `crates/core` looks the way it does. Each snippet names
the file it comes from, so you can read the real thing when a section ends.

Read this, then [`03-backend-plan.md`](03-backend-plan.md) for the pipeline and
crate boundaries, and [`04-backend-guide.md`](04-backend-guide.md) for recipes.

## Ownership: values move by default

In most languages assignment copies a reference. In Rust a plain assignment
*moves* the value: the old binding is no longer usable.

```rust
let entry = HistoryEntry { /* ... */ };
let hit = entry.hit();      // borrows `entry` for the call
let same = entry.clone();   // an explicit, visible copy
```

Four forms you will see everywhere:

| Form | Meaning | Example |
|---|---|---|
| `T` | take ownership | `async fn execute(&self, request: PreparedRequest, …)` |
| `&T` | look, don't take | `prepare(request: &RequestDef, …)` |
| `&mut T` | look and change | `fn set_zoom(&mut self, …)` |
| `.clone()` | make a second copy | `self.url.clone()` in `HistoryEntry::hit` |

`prepare()` borrows the `RequestDef` because it must not consume the editor's
state; the editor stays usable after the send. When you cannot borrow (a value
has to outlive the call, or cross a thread), clone it deliberately.

## `Option` instead of null, `Result` instead of exceptions

There is no `null`. A value that may be absent is `Option<T>`, which is
`Some(value)` or `None`, and the compiler makes you handle the `None`.

```rust
// crates/core/src/model.rs
pub struct ResponseRecord {
    pub status: u16,
    // ...
    /// The URL that actually answered, after any redirects. `None` on records
    /// written before this field existed.
    pub final_url: Option<String>,
}
```

There are no exceptions. A function that can fail returns `Result<T, E>`, which
is `Ok(value)` or `Err(error)`. The `?` operator unwraps `Ok` and returns early
on `Err`, so error handling reads like a straight line:

```rust
// crates/core/src/prepare.rs
let raw_url = resolve_with(&request.url, ctx, generators)?;
let mut url = Url::parse(&raw_url)
    .map_err(|error| Error::InvalidUrl(format!("{raw_url}: {error}")))?;
```

`Result<T>` in this repo is an alias for `Result<T, crate::error::Error>`; see
`crates/core/src/error.rs`.

## Enums: "one of these, with data"

When a value can be one of several shapes, Rust uses an enum whose variants may
carry fields. This is how the model expresses `Body` and `Auth`:

```rust
// crates/core/src/model.rs
#[derive(Debug, Clone, Default)]
pub enum Body {
    #[default]
    None,
    Raw { mime: String, text: String },
    Form(Vec<Kv>),
    Multipart(Vec<FormPart>),
    Binary { path: String },
}
```

`match` has to cover every variant, so adding a body kind turns into a
compile-error list of every place that must change. That is the point: the
compiler walks you through the edit. The same idea appears in `Origin`, which
records who owned a send:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Standalone,
    Saved { node: String },
    Draft { draft: i64 },
}
```

## Traits: the seam between crates

A trait is an interface. The engine defines two, and nothing in `core`
implements them, which is what keeps `core` free of reqwest, tokio, and the
filesystem:

```rust
// crates/core/src/store.rs
pub trait Store: Send + Sync + 'static {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64>;
    // ...
}
```

```rust
// crates/core/src/transport.rs
pub trait Transport: Send + Sync + 'static {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord>;
    // ...
}
```

`crates/store` implements `Store` four ways (`MemoryStore`, `JsonlStore`,
`FolderStore`, `SqlcipherStore`). `crates/transport` implements `Transport` with
`ReqwestTransport`. The app depends on the traits, never on a concrete store or
client, which is why a test can swap in `FakeTransport` (`core::testing`).

## `Arc<dyn Trait>`: one implementation, many owners

The workspace holds a shared store and transport:

```rust
// crates/app/src/workspace/mod.rs
pub(crate) struct Workspace {
    transport: Arc<ReqwestTransport>,
    store: Arc<dyn Store>,
    // ...
}
```

- `Arc<T>` is a thread-safe reference count: many owners, freed when the last
  one drops.
- `dyn Store` means "whatever concrete type implements `Store`". The workspace
  does not know which one it got.
- `Arc<dyn Store>` combines them: one shared object, chosen at startup and
  handed to panes.

The `Send + Sync + 'static` on the trait is the compiler's permission for that
shared value to cross threads and outlive a stack frame. It is not decoration;
removing it stops `Arc<dyn Store>` from being stored in the app.

## Closures and iterators

Rust closures are `|x| …`, and they compose with iterators:

```rust
// crates/core/src/prepare.rs
for header in request.headers.iter().filter(|header| header.enabled) {
    // ...
}
```

`iter()` borrows, `filter` keeps what the closure accepts, and the loop consumes
the result lazily. `map`, `find`, `any`, `collect` are the same style. Prefer
them over index loops; a `for` over `0..len` is almost always a missed iterator.

## `async` / `await`

An `async fn` returns a future instead of blocking. Nothing runs until something
awaits it. The transport owns a tokio runtime on a dedicated thread
(`crates/transport/src/lib.rs`) because the UI must never block on I/O:

```rust
// crates/app/src/workspace/send.rs (shape, not verbatim)
let result = transport.execute_with_progress(prepared, cancel, progress).await;
```

The `#[async_trait]` attribute on `Transport` is there because async methods in
traits are still awkward across crate boundaries; it desugars to a boxed future.
You do not need to understand the desugaring to call it.

## Interior mutability: `&self` that still changes

`Store` and `Transport` methods take `&self`, not `&mut self`, because they are
shared through an `Arc`. They still mutate internal state, through `Mutex` or
atomics:

```rust
// crates/core/src/transport.rs
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);
```

`CancelToken` is a cheap clone over an `AtomicBool`; cancelling flips the flag
and every held clone sees it. `Progress` is the same pattern with several
counters, so a transport can report bytes from the I/O thread while the UI reads
them without a lock.

## Modules and visibility

- `mod foo;` declares a module; `foo.rs` (or `foo/mod.rs`) is its body.
- Items are private by default. `pub` exports them; `pub(crate)` shares them
  inside one crate only.
- `Document` is `pub(crate)`: the panes and workspace share it, but it is not
  part of the app's public surface.

The `crates/core/src/lib.rs` file is the public face: it re-exports the pieces
other crates are allowed to name. If a type is missing there, it is internal by
design.

## Cargo workspaces and features

This repository has two Cargo workspaces. `crates/app` is excluded from the root
`Cargo.toml`, so `cargo build` at the root never compiles GPUI.

Features are compile-time switches:

```toml
# crates/store/Cargo.toml
[features]
sqlcipher = ["dep:base64", "dep:chrono", "dep:keyring", "dep:rand", "dep:rusqlite"]
```

Code is gated with `#[cfg(feature = "sqlcipher")]`. The app enables it by
default; `--no-default-features` falls back to plaintext JSONL. That is a real
build, not a test flag, so name which mode you verified.

## serde: compatibility is a feature

Persistence types derive `Serialize` and `Deserialize`. The attributes are not
noise; they are the migration policy:

| Attribute | Effect |
|---|---|
| `#[serde(default)]` | a missing field reads as its `Default`, so old files keep parsing |
| `#[serde(tag = "kind")]` | internally tagged enum: `{"kind": "saved", …}` |
| `#[serde(untagged)]` | try variants in order; a memory body still reads as a bare array |
| `#[serde(skip)]` | never written; used for a token that must not persist |

Adding a field is safe when it has `#[serde(default)]`. Removing or renaming one
without a default breaks every history row written before it — bump a schema only
when you cannot default the new field.

## Errors

`crates/core/src/error.rs` uses `thiserror`:

```rust
#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown variable `{{{{{0}}}}}`")]
    UnknownVariable(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),
    // ...
}
```

The `#[error("…")]` string is the user-facing message. The UI renders it
directly, so write it for a human, not for a log parser.

## Testing

Tests live in the same file as the code, under `#[cfg(test)] mod tests`. The
naming split is deliberate:

- `tests` — logic;
- `render_tests` — the painted tree;
- `enter_tests` — keyboard routing.

Each needs different setup. A test is a behavior sentence (`rejects_header_injection`)
with a doc comment stating the reason. Engine tests never need a network or a
window; `core::testing::FakeTransport` is how a send is proven offline.

## Coming from another language

| You are used to | Rust here |
|---|---|
| `null` / `undefined` | `Option<T>` |
| exceptions / `try` | `Result<T, E>` + `?` |
| `interface` / abstract class | `trait` + `impl Trait for Type` |
| class with methods | `struct` + `impl` block |
| union / discriminated union | `enum` with data-carrying variants |
| garbage collector | ownership, `Arc`, and `Mutex` |
| `final` / `const` | `let` (immutable by default) |
| `async def` | `async fn` + an executor |
| package | crate; monorepo | Cargo workspace |

## Where to go next

- The pipeline and crate boundaries → [`03-backend-plan.md`](03-backend-plan.md).
- How to change the engine, with recipes → [`04-backend-guide.md`](04-backend-guide.md).
- Why the app is a separate workspace → [`11-repo-map-and-dev-cycle.md`](11-repo-map-and-dev-cycle.md).
- The click → effect map → [`07-flows.md`](07-flows.md).
