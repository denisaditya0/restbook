# restbook — scope

A local-first desktop HTTP client, inspired by Postman, Insomnia, Yaak, and
Bruno. Original code; no forked or vendored sources. Zaku is a reference for
interaction and layout only — none of its AGPL code is used (see the ledger).

## Product principles

1. **Local-first, encrypted by default.** One SQLCipher database, key held in the
   OS keyring, no account, no telemetry. History is never opt-in.
2. **GUI-first, text-friendly.** Requests are created by clicking, but every URL
   and body is plain text you can paste in and copy out. Pasting a `curl` command
   produces a request.
3. **Small surface, finished edges.** HTTP only. No scripting, no test runner, no
   mocks, no cloud.

## In scope (Tier 1)

- Methods: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS
- URL bar, query-parameter table, `:path` variables
- Headers table with per-row enable/disable
- Bodies: none, raw (JSON/XML/text), form-urlencoded, multipart with files, binary
- Auth: none, basic, bearer, API key (header or query), OAuth 2.0 (PKCE + client
  credentials)
- Response: status, timing, size, pretty/raw body, headers, cookies
- Environments, secret variables, `{{var}}` interpolation with scope precedence
- Collections with nested folders and search
- Encrypted, searchable history of every request and response
- Import: Postman v2.1, OpenAPI 3, HAR, curl. Export: Postman/OpenAPI JSON
- Cookie jar, redirects, timeout, proxy, custom CA, mTLS
- Tabs, keyboard shortcuts, light/dark

## Explicitly out of scope

- JavaScript scripting and the `pm.*` API
- Collection runner, assertions, CLI reports
- Accounts, cloud sync, team workspaces, RBAC, SSO
- Mock servers, monitors, docs publishing, load testing
- Plugin system (would require embedding a JS engine)

## Milestones

The detailed, kept-current tables live in one place:

- [`01-milestones.md`](01-milestones.md) — the `U` (UI), `B` (engine) and `R`
  (release) tables with the release lane each item belongs to, the open decisions,
  and the reference-parity list. This is where a ticket is cut from.
- [`02-ui-plan.md`](02-ui-plan.md) owns the rules behind a `U` item (the pane
  contract, visual grammar), [`03-backend-plan.md`](03-backend-plan.md) owns the
  pipeline and crate boundaries behind a `B` item.

The scaffold milestones below are the origin of both, kept for history.

| # | Deliverable | State |
|---|---|---|
| M1 | Workspace, core model, templating, preparation | done in scaffold |
| M2 | reqwest transport + `devcli` harness | done in scaffold |
| M3 | SQLCipher store + keyring unlock + search | done, feature-gated |
| M4 | GPUI shell wired to the engine | done: docked panes, send, history |
| M5 | Import/export, curl paste, code generation | import/export done (B13); curl paste in the UI |

The engine's kept-current milestones are B1..B18, with B19..B24 planned, in
[`03-backend-plan.md`](03-backend-plan.md).

## Inspiration ledger

| Borrowed | From | Kept | Rejected |
|---|---|---|---|
| Collections, profiles, templating | Slumber | The model | YAML as the system of record |
| Offline, data ownership | Bruno | No account, local secrets | `.bru` plain text, built-in Git *(deferred; `1.0.0` at most)* |
| Paste-curl, local-first UX | Yaak | Interaction ideas | Plugin runtime |
| Feature vocabulary | Postman | Parameter tables, env switcher | Cloud, workspaces-as-a-service |
| Widgets, dock layout | `gpui-kit` | Components | Building a widget system |
| Dense pane layout, request tabs, progress UX | Zaku | Interaction and layout ideas; a progress sink on the transport | Its Zed-derived crate graph, AGPL code, `.toml`-as-request files |

## Known gaps in this scaffold

- The app prefers SQLCipher, which is a default feature: `cargo run` needs an
  OpenSSL dev tree, so `scripts/dev.sh` detects `OPENSSL_DIR` for it (see the
  README). Without one, `--no-default-features` falls back to plaintext JSONL
  in the same per-user data directory.
- No OAuth 2.0 authorization-code browser flow yet: the engine builds the
  authorize URL and exchanges a code, and the app fetches client-credentials
  tokens, but the local redirect listener is not wired.
- `devcli` persists plaintext JSONL by design; the app's history is encrypted,
  in the per-user data directory and in a workspace alike.
- A `restbook.json` workspace keeps its tracked files free of secrets, and its
  gitignored `.restbook/` history is on the encrypted store like the per-user
  one. A workspace never falls back to a plaintext history file: without a
  keyring, that session's history stays in memory, because a workspace is the
  tree a user shares.
