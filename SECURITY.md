# Security policy

restbook is a local-first HTTP client. There is no account, no cloud, and no
telemetry, so most of its security is the security of the files it writes on
your machine. This page says what is protected, what is not, and how to report a
problem.

## Reporting a vulnerability

Use GitHub's **private vulnerability reporting** for this repository:

<https://github.com/denisaditya0/restbook/security/advisories/new>

Please do not open a public issue for a vulnerability. A useful report includes:

- the app version (Settings › About, or the release tag);
- the operating system and whether the build uses the encrypted history;
- the steps, the expected result, and the actual one;
- how much data is exposed, and to whom.

What to expect: an acknowledgement within about **7 days**, and an assessment
with either a fix or a plan within **90 days**. Credit in the advisory if you
want it. There is no bug bounty.

## Supported versions

The latest published release and the tip of `master`. Older releases are not
patched; upgrade instead.

## What is protected

- **History is encrypted by default.** The app keeps one SQLCipher database and
  generates a random 256-bit key, stored in the OS keyring (Windows Credential
  Manager, macOS Keychain, or the Secret Service on Linux). The database cannot
  be read without that key, and no copy of the key is written next to it.
- **No account, no telemetry.** Nothing about your usage leaves the machine. The
  two outbound paths the app owns are your own requests and the update check
  against the release channel.
- **Workspace files separate structure from secrets.** In a folder workspace the
  tracked files (`collection.json`, `environments/*.json`) hold structure with
  secret values redacted; the real values live in `restbook.local.json` and the
  history in `.restbook/history.jsonl`. The app writes a `.gitignore` that
  excludes both.

## What is not protected

Read this before sharing a file or a log.

- **The plaintext fallback.** A build without SQLCipher, or a run whose keyring
  is unavailable, uses `<data dir>/history.jsonl` unencrypted. The app says so
  on stderr and in the status bar. Treat that file as sensitive.
- **Copying the local files.** `restbook.local.json` and
  `.restbook/history.jsonl` are secrets-bearing. Copying, syncing, or committing
  them exposes the values the tracked files deliberately do not hold.
- **Exports are plain files.** An export writes JSON, YAML, or HAR wherever you
  choose, including whatever values the exported collections and environments
  carry. Treat an export as a secret. Default redaction of secret values is
  planned but not shipped yet — review before sharing.
- **The log.** `restbook.log` records request URLs and a short response-body
  preview, and the UI offers to copy it for bug reports. It can contain tokens or
  personal data.
- **Memory.** Secrets are not zeroized after use. A process that can read the
  app's memory or the keyring can read them.
- **The update channel.** Downloads are verified against the digest published in
  the manifest, but builds are not code-signed yet, so a compromised release
  account is not excluded by the signature.

## Known limitations

Dated to `0.1.5`; the fixes are tracked in
[`docs/01-milestones.md`](docs/01-milestones.md).

- **Redirects carry request credentials.** `Authorization`, `Cookie`, and
  `Proxy-Authorization` set on a request currently follow a redirect to another
  host (`B27`). Until that ships, avoid credential headers on a request that may
  redirect somewhere you do not control.
- **Stored response bodies have no size cap.** Retention bounds age and count,
  not bytes (`B21`), so one very large response can grow the database.
- **Imports have no file-size cap** and a small malformed-input corpus
  (`0.2.7`).
- **The plaintext fallback is a fallback.** It is what the app does when it
  cannot encrypt; it is not a mode to select.

## Scope

In scope: the engine crates (`core`, `transport`, `store`), the desktop app, key
handling, workspace file handling, import and export, and update verification.

Out of scope: anything that requires a compromised OS account or an unlocked
keyring; issues in the pinned GPUI/toolchain (report upstream); third-party
hosts; and publicly known advisories in a dependency (report upstream — we will
still bump it). Please test only against APIs and machines you own or are
authorised to use.

## Disclosure

Disclosure is coordinated: a fix ships in a release and the advisory is published
with it, or after 90 days, whichever comes first. Good-faith research will not be
met with legal action.
