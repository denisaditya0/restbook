//! The changelog is the release body, so a version that ships without an entry
//! would publish a release page and an update offer with no description.
//!
//! `release.yml` fails the publish in that case, but the publish is the last
//! place to find out: by then the tag exists and the artifacts are built. This
//! runs on the pull request, so the version bump and its changelog entry arrive
//! in the same review.

use std::path::Path;

/// The version stated by a manifest, resolved from this crate so the test does
/// not care what directory `cargo test` runs in.
fn manifest_version(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let manifest = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .and_then(|rest| rest.trim().strip_prefix('"'))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no version in {}", path.display()))
}

/// The body of one `## <version>` section, from its heading to the next one.
fn section(version: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    let mut inside = false;
    let mut body = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if inside {
                break;
            }
            inside = rest.trim() == version;
            continue;
        }
        if inside {
            body.push(line);
        }
    }
    inside.then(|| body.join("\n"))
}

#[test]
fn the_current_version_has_a_changelog_entry() {
    let version = manifest_version("../../Cargo.toml");
    let body = section(&version).unwrap_or_else(|| {
        panic!("CHANGELOG.md has no '## {version}' section; add one before tagging")
    });
    assert!(
        body.lines().any(|line| line.trim_start().starts_with("- ")),
        "CHANGELOG.md's '## {version}' section lists nothing a user would read",
    );
}

/// The entry is what the user sees in the update dialog, so it has to read as
/// prose. A code fence would render as raw source there.
#[test]
fn the_changelog_entry_is_plain_prose() {
    let version = manifest_version("../../Cargo.toml");
    let body = section(&version).expect("the entry is asserted by the test above");
    assert!(
        !body.contains("```"),
        "CHANGELOG.md's '## {version}' section has a code fence; it is shown as body text",
    );
}

/// The release page is one line per merged pull request: what changed, its
/// kind, the author, and the PR number. The manifest's `notes` stays the
/// changelog entry, so the in-app dialog keeps the user's words.
#[test]
fn the_release_page_is_a_whats_new_list() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml");
    let workflow = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    assert!(
        !workflow.contains("release-install"),
        "the install block was removed from the release page; do not bring it back",
    );
    assert!(
        workflow.contains("## What's New"),
        "the release page must be a What's New list",
    );
    assert!(
        workflow.contains("($kind) by @$author #$n"),
        "each What's New line is '- <detail> (<kind>) by @<author> #<pr>'",
    );
    assert!(
        !workflow.contains("## Contributors"),
        "the page attributes each change instead of listing contributors separately",
    );

    // The `--arg notes` must be given the changelog entry, not the page body.
    let binding = workflow
        .lines()
        .find(|line| line.contains("--arg notes"))
        .expect("the manifest's jq expression moved; update this test");
    assert!(
        binding.contains("--arg notes \"$notes\""),
        "the manifest's notes must be the changelog entry ($notes), not the release \
         page's What's New body",
    );
}

/// Every version reads the same way on the release page and in the update
/// dialog: one bullet per user-visible change, each led by a bold sentence.
/// That is the template in the file's header, and it only holds if every
/// section follows it.
#[test]
fn every_changelog_entry_follows_the_template() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    let mut version = String::new();
    let mut in_section = false;
    let mut bullets = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            assert!(
                !in_section || bullets > 0,
                "CHANGELOG.md's '## {version}' section lists nothing a user would read",
            );
            version = rest.trim().to_owned();
            in_section = true;
            bullets = 0;
            continue;
        }
        if in_section && line.starts_with("- ") {
            bullets += 1;
            assert!(
                line.starts_with("- **"),
                "CHANGELOG.md's '## {version}' section has a bullet that is not \
                 led by a bold sentence: {line}",
            );
        }
    }
    assert!(
        !in_section || bullets > 0,
        "CHANGELOG.md's '## {version}' section lists nothing a user would read",
    );
}

/// The app's own manifest must agree with the root workspace, or the release
/// publishes artifacts whose `latest.json` version the updater cannot match.
#[test]
fn both_manifests_carry_the_same_version() {
    assert_eq!(
        manifest_version("Cargo.toml"),
        manifest_version("../../Cargo.toml"),
        "crates/app and the root workspace disagree; bump both",
    );
}
