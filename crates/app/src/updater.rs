//! Update checks: what version is published, and whether it is newer than this
//! build. Phase 1 is read-only: a check fetches a manifest, parses it, compares
//! and reports; nothing is downloaded or replaced. The types live here so the
//! installer phase can reuse them.
//!
//! A packaged app is launched from Explorer or Finder with no useful
//! environment, so the manifest URL is derived from this build's release
//! channel at compile time. `RESTBOOK_UPDATE_URL` still overrides it, at build
//! time or at runtime, which is how a test or a preview build points one binary
//! somewhere else.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use semver::Version;
use serde::Deserialize;

/// The version compiled into this binary, as shown in Settings and sent in the
/// `User-Agent` of a check.
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Overrides the release manifest URL. Empty or unset means the channel's own
/// URL is used, which is the normal case for a packaged build.
pub(crate) const MANIFEST_URL_ENV: &str = "RESTBOOK_UPDATE_URL";

/// The repository whose releases this build polls. `RESTBOOK_UPDATE_REPO`
/// replaces it at build time; CI passes `github.repository`, so a fork's own
/// builds poll the fork instead of this one.
const RELEASE_REPO: &str = match option_env!("RESTBOOK_UPDATE_REPO") {
    Some(repo) => repo,
    None => "denisaditya0/restbook",
};

/// Manifest asset names, one per channel. `docs/09-release-and-updates.md`
/// owns the convention; the release workflow writes these files.
const STABLE_MANIFEST: &str = "latest.json";
const BETA_MANIFEST: &str = "latest-beta.json";

/// How long a check may take. This runs off the UI thread, but a manifest is a
/// few hundred bytes: a stalled server should become an error, not a task that
/// lives for the session.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// A parsed release manifest.
///
/// `assets` is keyed by [`platform_key`]. Future fields can be added to the
/// manifest without breaking older apps: Serde ignores keys it does not know,
/// so a build only has to understand the fields it reads.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Manifest {
    /// The published version, as semver.
    pub(crate) version: String,
    /// One short paragraph for the notification. Optional so a manifest can be
    /// published before its notes are written.
    #[serde(default)]
    pub(crate) notes: Option<String>,
    #[serde(default)]
    pub(crate) assets: BTreeMap<String, Asset>,
}

/// One downloadable build. Phase 1 selects the entry for this OS and
/// architecture; the installer phase consumes `url`, `sha256`, and `signature`
/// before writing anything.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Asset {
    pub(crate) url: String,
    // Read by the installer phase. Kept in the format from the start so a
    // manifest published today does not have to be replaced when that lands.
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) sha256: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) signature: Option<String>,
}

/// The result of a check, with the network and error paths already flattened
/// into what the UI needs to say.
#[derive(Debug, Clone)]
pub(crate) enum Outcome {
    /// No manifest URL is configured for this build.
    NotConfigured,
    /// The running build is at least as new as the published one.
    UpToDate { current: String },
    /// A newer version exists. Phase 1 only reports it; nothing is installed.
    Available {
        latest: String,
        notes: Option<String>,
        /// `None` when the manifest publishes no build for this platform. That
        /// is still "newer exists", just not one this machine can install.
        asset: Option<Asset>,
    },
}

/// The manifest URL for this build: the runtime or build-time override when one
/// is set, otherwise the URL of the channel the compiled version belongs to.
pub(crate) fn manifest_url() -> Option<String> {
    runtime_override()
        .or_else(build_override)
        .or_else(channel_url)
}

/// `RESTBOOK_UPDATE_URL` in the process environment. A developer or a test can
/// point one binary anywhere without rebuilding it.
fn runtime_override() -> Option<String> {
    std::env::var(MANIFEST_URL_ENV).ok().and_then(non_blank)
}

/// `RESTBOOK_UPDATE_URL` from the environment the crate was compiled in. Cargo
/// rebuilds when it changes, so the value is part of the binary.
fn build_override() -> Option<String> {
    option_env!("RESTBOOK_UPDATE_URL").and_then(|url| non_blank(url.to_owned()))
}

/// The channel this build belongs to: a version with a prerelease suffix is
/// beta, everything else is stable.
fn channel_url() -> Option<String> {
    let version = Version::parse(CURRENT_VERSION).ok()?;
    Some(manifest_url_for(RELEASE_REPO, &version))
}

/// Where `repo` publishes the manifest for `version`'s channel.
///
/// `releases/latest` resolves to the newest non-prerelease release, which is
/// exactly stable. It can never serve a prerelease, so beta is a permanent
/// `beta-latest` release whose single asset is replaced in place.
fn manifest_url_for(repo: &str, version: &Version) -> String {
    if version.pre.is_empty() {
        format!("https://github.com/{repo}/releases/latest/download/{STABLE_MANIFEST}")
    } else {
        format!("https://github.com/{repo}/releases/tags/beta-latest/download/{BETA_MANIFEST}")
    }
}

/// `None` for a URL that is empty or only whitespace.
fn non_blank(url: String) -> Option<String> {
    let url = url.trim();
    (!url.is_empty()).then(|| url.to_owned())
}

/// `os-arch`, the key a manifest uses to name a build. It is derived from
/// `std::env::consts` rather than a hand-written table so a new target cannot
/// silently keep asking for the wrong asset.
pub(crate) fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Fetch the manifest at `url` and interpret it against the running version.
pub(crate) fn check(url: &str) -> Result<Outcome> {
    interpret(fetch(url)?, CURRENT_VERSION)
}

/// Compare a manifest with `current`. Split from [`check`] so the version rules
/// are testable without a server.
fn interpret(manifest: Manifest, current: &str) -> Result<Outcome> {
    let current = Version::parse(current)
        .with_context(|| format!("this build's version `{current}` is not semver"))?;
    let latest = Version::parse(manifest.version.trim()).with_context(|| {
        format!(
            "the manifest's version `{}` is not semver",
            manifest.version
        )
    })?;

    if latest <= current {
        return Ok(Outcome::UpToDate {
            current: current.to_string(),
        });
    }

    Ok(Outcome::Available {
        latest: latest.to_string(),
        notes: manifest.notes,
        asset: manifest.assets.get(&platform_key()).cloned(),
    })
}

fn fetch(url: &str) -> Result<Manifest> {
    // Blocking on purpose: this runs on the background executor, where a
    // blocking call costs one pool thread instead of requiring a reactor that
    // GPUI does not install.
    let client = reqwest::blocking::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .user_agent(format!("restbook/{CURRENT_VERSION}"))
        .build()
        .context("building the update client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("contacting {url}"))?;

    let status = response.status();
    if !status.is_success() {
        bail!("the update server answered {status}");
    }

    response
        .json::<Manifest>()
        .context("reading the update manifest")
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{interpret, manifest_url, manifest_url_for, platform_key, Manifest, Outcome};

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).expect("the test manifest parses")
    }

    fn sample(version: &str) -> Manifest {
        manifest(&format!(
            r#"{{
                "version": "{version}",
                "notes": "Fixes the thing.",
                "assets": {{
                    "{}": {{
                        "url": "https://example.com/restbook-{version}.zip",
                        "sha256": "abc",
                        "signature": "sig"
                    }}
                }}
            }}"#,
            platform_key()
        ))
    }

    #[test]
    fn a_newer_manifest_is_available() {
        let outcome = interpret(sample("0.2.0"), "0.1.0").unwrap();
        let Outcome::Available {
            latest,
            notes,
            asset,
        } = outcome
        else {
            panic!("expected an available update");
        };
        assert_eq!(latest, "0.2.0");
        assert_eq!(notes.as_deref(), Some("Fixes the thing."));
        // The asset for this machine is selected by the derived key.
        assert_eq!(
            asset.as_ref().map(|asset| asset.url.as_str()),
            Some("https://example.com/restbook-0.2.0.zip")
        );
    }

    #[test]
    fn an_equal_or_older_manifest_is_up_to_date() {
        for version in ["0.1.0", "0.0.9"] {
            let outcome = interpret(sample(version), "0.1.0").unwrap();
            assert!(
                matches!(outcome, Outcome::UpToDate { .. }),
                "{version} should not be offered"
            );
        }
    }

    #[test]
    fn a_missing_platform_build_is_still_a_newer_version() {
        // A manifest with no asset for this OS is a real manifest; it just
        // cannot be installed here, and the UI says so instead of pretending
        // the release does not exist.
        let manifest = manifest(r#"{"version": "0.2.0", "assets": {}}"#);
        let Outcome::Available { asset, .. } = interpret(manifest, "0.1.0").unwrap() else {
            panic!("expected an available update");
        };
        assert!(asset.is_none());
    }

    #[test]
    fn a_nonsense_published_version_is_an_error() {
        let error = interpret(sample("release-2"), "0.1.0").unwrap_err();
        assert!(error.to_string().contains("not semver"));
    }

    #[test]
    fn unknown_manifest_keys_are_ignored() {
        // A manifest from a newer release may carry fields this build does not
        // know about; ignoring them is what keeps a check working.
        let manifest = manifest(r#"{"version": "0.2.0", "min_app": "0.1.0", "assets": {}}"#);
        assert!(interpret(manifest, "0.1.0").is_ok());
    }

    #[test]
    fn a_packaged_build_always_has_a_manifest_url() {
        // The whole point of the baked-in channel: a GUI app started with no
        // environment still knows where to look.
        assert!(manifest_url().is_some());
    }

    #[test]
    fn a_stable_build_reads_the_stable_manifest() {
        let url = manifest_url_for("owner/repo", &Version::parse("0.1.0").unwrap());
        assert_eq!(
            url,
            "https://github.com/owner/repo/releases/latest/download/latest.json"
        );
    }

    #[test]
    fn a_prerelease_build_reads_the_beta_channel() {
        let url = manifest_url_for("owner/repo", &Version::parse("0.2.0-beta.1").unwrap());
        assert_eq!(
            url,
            "https://github.com/owner/repo/releases/tags/beta-latest/download/latest-beta.json"
        );
    }

    #[test]
    fn the_platform_key_is_os_and_arch() {
        assert_eq!(
            platform_key(),
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        );
    }
}
