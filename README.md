# restbook

A local-first HTTP client for the desktop: Postman's concepts, no account, no
cloud, and every request you send saved to an encrypted local history.

Status: **engine + first shell**. The HTTP engine, request preparation, and
history stores are in place and exercised by `devcli`. The GPUI desktop app
sends requests and shows responses end to end; collections, environments, and
the encrypted store are still to come.

## Layout

```
crates/core       domain model, {{var}} templating, request preparation, traits
crates/transport  reqwest adapter (owns its own tokio runtime)
crates/store      history stores: JSONL (dev), SQLCipher (feature = "sqlcipher")
crates/devcli     command-line harness for the engine
crates/app        GPUI desktop shell (excluded from the workspace)
```

`core` has no UI dependency and no runtime ownership. That is deliberate: the
UI can be replaced, and the engine can be tested and driven headlessly.

## Quick start

```sh
cargo build
cargo run -p restbook-devcli -- send https://httpbin.org/get
cargo run -p restbook-devcli -- send https://httpbin.org/post -X POST --json '{"hello":"world"}'
cargo run -p restbook-devcli -- send https://httpbin.org/bearer --var TOKEN=abc \
  -H 'Authorization:Bearer {{TOKEN}}'
cargo run -p restbook-devcli -- history httpbin
```

History for the dev harness is stored as plain JSONL in `.restbook/history.jsonl`.
That is a development convenience, not the product storage path.

## Desktop shell

```sh
cd crates/app
cargo run
```

The window has three panes: history on the left, the request editor above the
response viewer on the right.

- Pick a method, type a URL, press **Send**. `prepare()` runs before any I/O, so
  an invalid URL or an unknown `{{variable}}` is reported inline and nothing is
  sent.
- A second request can be cancelled while in flight.
- Successful responses are recorded to `.restbook/history.jsonl` (relative to the
  working directory) and appear in the sidebar; click a row to load it back into
  the editor.
- JSON bodies are pretty-printed. Display is capped at 256 KB; the full bytes
  stay in the response record.

The app crate is a separate workspace so the engine build never depends on GPUI.
It owns one `ReqwestTransport` for the process lifetime, which is what keeps the
tokio runtime and the four pooled clients from being rebuilt per request.

## Design decisions

- **Backend first.** The engine is usable and debuggable without a window.
- **One source of truth.** `RequestDef` -> `prepare()` -> `PreparedRequest` ->
  `Transport` -> `ResponseRecord`. Nothing bypasses it.
- **Runtime isolation.** `reqwest` needs a tokio reactor; GPUI's executor is not
  tokio. `ReqwestTransport` owns a runtime on a dedicated thread and hands plain
  futures back, so the UI never touches tokio.
- **Encryption is the product's job, not the harness's.** The SQLCipher store is
  feature-gated (`--features sqlcipher`); see `docs/00-scope.md`.

## Windows setup (this machine)

Visual Studio Build Tools are not installed, so Rust uses the GNU toolchain
rather than MSVC:

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu   # pinned in rust-toolchain.toml
scoop install mingw                                     # provides dlltool + as
```

Add both directories to your **user PATH** (System Properties > Environment
Variables > User variables > Path):

```
%USERPROFILE%\.cargo\bin
%USERPROFILE%\scoop\apps\mingw\current\bin
```

After that `cargo build` works from a normal shell with no env vars. TLS uses
`native-tls` (SChannel) instead of `rustls`, because `ring` needs a C compiler;
flip the feature in the workspace `Cargo.toml` if MSVC Build Tools get installed.

### Encrypted store (`sqlcipher` feature)

`rusqlite`'s `bundled-sqlcipher` needs OpenSSL headers and import libraries:

```sh
OPENSSL_DIR='C:\Users\Denis\scoop\apps\mingw\current\opt' \
  cargo build -p restbook-store --features sqlcipher
```

Verified on this machine: the roundtrip test creates the keyring entry, opens an
encrypted database, writes a history entry, and searches it back. The database
file contains no plaintext.

### Desktop app

`crates/app/rust-toolchain.toml` pins `stable-x86_64-pc-windows-gnu`, matching the
engine. GPUI compiles and links under GNU, but its build scripts call
`dlltool.exe`, so the MinGW directory above has to be on `PATH`.

If Build Tools with the "Desktop development with C++" workload are ever
installed, the app can be built with MSVC instead by changing the channel in
that file to `stable-x86_64-pc-windows-msvc`.
