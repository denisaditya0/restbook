# restbook — scope

A local-first desktop HTTP client, inspired by Postman, Insomnia, Yaak, and
Bruno. Original code; no forked or vendored sources.

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
  credentials) — *OAuth deferred past milestone 2*
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

| # | Deliverable | State |
|---|---|---|
| M1 | Workspace, core model, templating, preparation | done in scaffold |
| M2 | reqwest transport + `devcli` harness | done in scaffold |
| M3 | SQLCipher store + keyring unlock + search | done, feature-gated |
| M4 | GPUI shell wired to the engine | shell only |
| M5 | Import/export, curl paste, code generation | not started |

## Inspiration ledger

| Borrowed | From | Kept | Rejected |
|---|---|---|---|
| Collections, profiles, templating | Slumber | The model | YAML as the system of record |
| Offline, data ownership | Bruno | No account, local secrets | `.bru` plain text, built-in Git |
| Paste-curl, local-first UX | Yaak | Interaction ideas | Plugin runtime |
| Feature vocabulary | Postman | Parameter tables, env switcher | Cloud, workspaces-as-a-service |
| Widgets, dock layout | `gpui-kit` | Components | Building a widget system |

## Known gaps in this scaffold

- The SQLCipher store is off by default; enable it with `--features sqlcipher` (needs `OPENSSL_DIR`).
- No OAuth 2.0 flow yet.
- Response cookie view is not modelled beyond the cookie jar.
- `devcli` persists plaintext JSONL by design; only the app path is encrypted.
