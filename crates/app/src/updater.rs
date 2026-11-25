//! Update checks and installs: the published version, whether it is newer than
//! this build, and — once the user asks — the download that replaces it. The
//! install verifies the asset against the manifest's digest.
//!
//! A packaged app has no useful environment, so the channel URL is baked in at
//! compile time; `RESTBOOK_UPDATE_URL` overrides it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// The version compiled into this binary, as shown in Settings and sent in the
/// `User-Agent` of a check.
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Overrides the release manifest URL. Empty or unset means the baked-in URL is
/// used, which is the normal case for a packaged build.
pub(crate) const MANIFEST_URL_ENV: &str = "RESTBOOK_UPDATE_URL";

/// The repository whose releases this build polls. `RESTBOOK_UPDATE_REPO`
/// replaces it at build time; CI passes `github.repository`, so a fork's own
/// builds poll the fork instead of this one.
const RELEASE_REPO: &str = match option_env!("RESTBOOK_UPDATE_REPO") {
    Some(repo) => repo,
    None => "denisaditya0/restbook",
};

/// The manifest asset name. `docs/10-release-and-updates.md` owns the
/// convention; the release workflow writes this file.
const MANIFEST: &str = "latest.json";

/// How long a check may take. This runs off the UI thread, but a manifest is a
/// few hundred bytes: a stalled server should become an error, not a task that
/// lives for the session.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a download may take. Bigger than the manifest's budget by three
/// orders of magnitude, because a release asset is megabytes rather than a few
/// hundred bytes, and a slow link is not a failed update.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

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

/// One downloadable build. The check selects the entry for this OS and
/// architecture; [`download`] consumes `url` and `sha256` before writing
/// anything, and `signature` is reserved for when releases are signed.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Asset {
    pub(crate) url: String,
    // Checked by `download`: a file that does not match is never opened.
    #[serde(default)]
    pub(crate) sha256: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) signature: Option<String>,
}

impl Asset {
    /// The file name the release published: the last path segment of the URL,
    /// without a query string. `None` when that segment is empty or names a
    /// directory, so a malformed manifest cannot choose where the write lands.
    fn file_name(&self) -> Option<&str> {
        let segment = self.url.split('/').next_back()?;
        let segment = segment.split(['?', '#']).next()?;
        (!segment.is_empty() && segment != "." && segment != ".." && !segment.contains('\\'))
            .then_some(segment)
    }
}

/// What [`download`] leaves behind, and what the caller has to do with it.
pub(crate) struct Package {
    pub(crate) path: PathBuf,
    /// Windows hands the file to its own installer and quits so the exe can be
    /// replaced. macOS and Linux open the package and stay running: replacing a
    /// `.app` or a `.deb` from inside an unsigned build is not something this
    /// app attempts.
    pub(crate) closes_app: bool,
}

/// The result of a check, with the network and error paths already flattened
/// into what the UI needs to say.
#[derive(Debug, Clone)]
pub(crate) enum Outcome {
    /// No manifest URL is configured for this build.
    NotConfigured,
    /// The running build is at least as new as the published one.
    UpToDate { current: String },
    /// A newer version exists, and this is the build for it.
    Available {
        latest: String,
        notes: Option<String>,
        /// `None` when the manifest publishes no build for this platform. That
        /// is still "newer exists", just not one this machine can install.
        asset: Option<Asset>,
    },
}

/// The manifest URL for this build: the runtime or build-time override when one
/// is set, otherwise the one release channel's URL.
pub(crate) fn manifest_url() -> Option<String> {
    runtime_override()
        .or_else(build_override)
        .or_else(release_url)
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

/// The one release channel.
///
/// A build whose version does not parse has no channel at all: it cannot say
/// which release it belongs to, and `release.yml` refuses to publish such a
/// version anyway.
fn release_url() -> Option<String> {
    Version::parse(CURRENT_VERSION).ok()?;
    Some(manifest_url_for(RELEASE_REPO))
}

/// Where `repo` publishes the manifest.
///
/// `releases/latest` resolves to the newest non-prerelease release, which is the
/// only kind this project publishes. A prerelease tag is refused by `verify`,
/// so there is no second URL to fall back to.
fn manifest_url_for(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/latest/download/{MANIFEST}")
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
        .user_agent(user_agent())
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

fn user_agent() -> String {
    format!("restbook/{CURRENT_VERSION}")
}

/// Download `asset` into a temp directory and check it against the digest the
/// manifest publishes. Blocking for the same reason [`fetch`] is.
pub(crate) fn download(asset: &Asset) -> Result<Package> {
    let name = asset
        .file_name()
        .with_context(|| format!("the manifest's asset URL names no file: {}", asset.url))?;

    let dir = std::env::temp_dir().join("restbook-update");
    std::fs::create_dir_all(&dir).context("creating the update directory")?;
    let path = dir.join(name);

    let client = reqwest::blocking::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(user_agent())
        .build()
        .context("building the download client")?;

    let response = client
        .get(&asset.url)
        .send()
        .with_context(|| format!("downloading {}", asset.url))?;
    let status = response.status();
    if !status.is_success() {
        bail!("the download answered {status}");
    }
    let bytes = response.bytes().context("reading the download")?;

    // A manifest may omit the digest (docs/09 allows publishing a channel
    // before signing exists). That is worth a log line, not a refusal.
    match asset.sha256.as_deref() {
        Some(expected) => verify(&bytes, expected)
            .with_context(|| format!("{name} did not match the published digest"))?,
        None => log::warn!("update asset {name} publishes no sha256; writing it unverified"),
    }

    std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;

    Ok(Package {
        path,
        closes_app: cfg!(target_os = "windows"),
    })
}

/// Hand a downloaded package to the operating system.
pub(crate) fn open(package: &Package) -> Result<()> {
    let mut command = if cfg!(target_os = "windows") {
        Command::new(&package.path)
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(&package.path);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(&package.path);
        command
    };

    command
        .spawn()
        .with_context(|| format!("opening {}", package.path.display()))?;
    Ok(())
}

/// Compare a download with the hex digest the manifest publishes.
fn verify(bytes: &[u8], expected: &str) -> Result<()> {
    let digest = Sha256::digest(bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        bail!("expected sha256 {}, downloaded {actual}", expected.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        interpret, manifest_url, manifest_url_for, platform_key, verify, Asset, Manifest, Outcome,
    };

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
        // The whole point of the baked-in URL: a GUI app started with no
        // environment still knows where to look.
        assert!(manifest_url().is_some());
    }

    /// One channel: every build asks `releases/latest`, which is the only place
    /// a release is published now that `verify` refuses prerelease tags.
    #[test]
    fn every_build_reads_the_one_manifest() {
        assert_eq!(
            manifest_url_for("owner/repo"),
            "https://github.com/owner/repo/releases/latest/download/latest.json"
        );
    }

    #[test]
    fn the_platform_key_is_os_and_arch() {
        assert_eq!(
            platform_key(),
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        );
    }

    fn asset(url: &str) -> Asset {
        Asset {
            url: url.to_owned(),
            sha256: None,
            signature: None,
        }
    }

    /// The name a download is written under comes from the release's own URL, so
    /// the file on disk is the file the manifest described.
    #[test]
    fn an_asset_url_names_the_file_to_write() {
        assert_eq!(
            asset("https://example.com/a/b/restbook-0.2.0-windows-x86_64.exe?token=1").file_name(),
            Some("restbook-0.2.0-windows-x86_64.exe")
        );
    }

    /// A malformed manifest must not be able to pick a directory, or the write
    /// would land wherever it said.
    #[test]
    fn an_asset_url_cannot_choose_a_directory() {
        for url in [
            "https://example.com/",
            "https://example.com/a/..",
            "https://example.com/a/.",
            r"https://example.com/..\windows\system32",
        ] {
            assert!(
                asset(url).file_name().is_none(),
                "{url} was accepted as a file name"
            );
        }
    }

    /// The digest is what stands between a fetched file and running it.
    #[test]
    fn a_download_is_refused_when_the_digest_differs() {
        // sha256("abc")
        let published = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify(b"abc", published).is_ok());
        assert!(verify(b"abc", &published.to_uppercase()).is_ok());
        assert!(verify(b"abd", published).is_err());
    }
}
