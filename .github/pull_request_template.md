<!--
One request per pull request. Split it only when a maintainer asks.
Keep every section short: a reviewer reads this instead of a running build.
-->

## Problem

<!-- A sentence or two: what is wrong, or what is missing, and who notices. -->

## Fix

<!-- How this change addresses it. Name the files or modules a reviewer should read first. -->

## Evidence

<!--
A change with a visible surface needs before/after images; motion or timing
needs a short video. Attach them to this pull request — never commit
screenshots or PR-only assets such as `.github/pr-assets/`.

Delete this section when nothing visible changes (`--preview <pane>` in
`crates/app` is the way to capture one).
-->

## Checks

- [ ] `cargo fmt --check` in the root and in `crates/app`
- [ ] `cargo clippy --all-targets -- -D warnings` in both
- [ ] tests in both (`cargo test --workspace`, `cargo test --no-default-features`)
- [ ] no version bump: `ci.yml` rejects one, and the bump belongs to the release commit

---
Model: <!-- e.g. deepseek-v4.1-flash-1017 --> · Harness: <!-- e.g. Zed coding agent -->
