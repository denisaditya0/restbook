//! The Updates page's state: what the last check found, whether a download is
//! running, and where the package landed.
//!
//! An entity rather than fields in the dialog builder, because that builder is a
//! plain closure the kit runs again on every frame: anything stored in it is
//! rebuilt, and a check that reports back later would have nothing to report to.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use crate::updater::{self, Asset};

/// How long the app waits before closing after it opens a Windows installer.
/// Long enough for the dialog to paint the sentence that says it is closing.
const QUIT_GRACE: Duration = Duration::from_millis(1500);

/// What the page is showing. One value, so the button, the sentence and the
/// enabled state cannot disagree.
#[derive(Clone)]
pub(crate) enum Status {
    /// Nothing has been checked in this session yet.
    Idle,
    Checking,
    UpToDate {
        current: String,
    },
    Available {
        latest: String,
        notes: Option<String>,
        /// `None` when the release published nothing for this platform: newer
        /// exists, but not one this machine can install.
        asset: Option<Asset>,
    },
    Downloading {
        latest: String,
    },
    /// Downloaded, verified, and handed to the operating system.
    Ready {
        latest: String,
        path: PathBuf,
        closes_app: bool,
    },
    Failed(String),
}

pub(crate) struct Updates {
    status: Status,
}

impl Updates {
    pub(crate) fn new() -> Self {
        Self {
            status: Status::Idle,
        }
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }

    /// The asset to fetch, when the last check found one for this platform.
    pub(crate) fn asset(&self) -> Option<Asset> {
        match &self.status {
            Status::Available {
                asset: Some(asset), ..
            } => Some(asset.clone()),
            _ => None,
        }
    }

    /// Fetch the manifest and compare it with this build. A second click while a
    /// check or a download is in flight is ignored rather than started.
    pub(crate) fn check(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Checking | Status::Downloading { .. }) {
            return;
        }
        self.status = Status::Checking;
        cx.notify();

        let url = updater::manifest_url();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match url {
                        Some(url) => updater::check(&url),
                        None => Ok(updater::Outcome::NotConfigured),
                    }
                })
                .await;

            this.update(cx, |updates, cx| updates.checked(outcome, cx))
                .ok();
        })
        .detach();
    }

    fn checked(&mut self, outcome: anyhow::Result<updater::Outcome>, cx: &mut Context<Self>) {
        self.status = match outcome {
            Ok(updater::Outcome::NotConfigured) => Status::Failed(
                "This build has no update channel, so it cannot check for releases.".to_owned(),
            ),
            Ok(updater::Outcome::UpToDate { current }) => Status::UpToDate { current },
            Ok(updater::Outcome::Available {
                latest,
                notes,
                asset,
            }) => Status::Available {
                latest,
                notes,
                asset,
            },
            Err(error) => Status::Failed(format!("Couldn’t check for updates. {error}")),
        };
        cx.notify();
    }

    /// Adopt an asset the launch offer found, then install it.
    ///
    /// [`Self::install`] reads the version out of `Status::Available`, which is
    /// only set by a check run in this entity. The offer runs its check in the
    /// workspace's task, so the status is seeded here first and the rest of the
    /// path — download, verify, hand to the OS — stays the one implementation.
    pub(crate) fn install_offered(&mut self, latest: String, asset: Asset, cx: &mut Context<Self>) {
        self.status = Status::Available {
            latest,
            notes: None,
            asset: Some(asset.clone()),
        };
        self.install(asset, cx);
    }

    /// Download the asset for this platform, verify it, and hand it to the OS.
    pub(crate) fn install(&mut self, asset: Asset, cx: &mut Context<Self>) {
        let Status::Available { latest, .. } = &self.status else {
            return;
        };
        let latest = latest.clone();
        self.status = Status::Downloading {
            latest: latest.clone(),
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let package = updater::download(&asset)?;
                    updater::open(&package)?;
                    Ok::<_, anyhow::Error>(package)
                })
                .await;

            match result {
                Ok(package) => {
                    let closes_app = package.closes_app;
                    this.update(cx, |updates, cx| {
                        updates.status = Status::Ready {
                            latest,
                            path: package.path,
                            closes_app,
                        };
                        cx.notify();
                    })
                    .ok();

                    if closes_app {
                        // The installer replaces this exe, so the app has to go.
                        // Inno's `CloseApplications` covers the race if it starts
                        // before the quit lands.
                        cx.background_executor().timer(QUIT_GRACE).await;
                        cx.update(|cx| cx.quit());
                    }
                }
                Err(error) => {
                    this.update(cx, |updates, cx| {
                        updates.status =
                            Status::Failed(format!("Couldn’t install the update. {error}"));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{Status, Updates};

    fn with(status: Status) -> Updates {
        Updates { status }
    }

    /// Only a check that found this platform's build may offer an install: the
    /// "no build published for this platform" state is still `Available`.
    #[test]
    fn only_an_available_asset_can_be_installed() {
        assert!(with(Status::Idle).asset().is_none());
        assert!(with(Status::Failed("offline".to_owned())).asset().is_none());
        assert!(with(Status::Checking).asset().is_none());
        assert!(with(Status::Available {
            latest: "0.2.0".to_owned(),
            notes: None,
            asset: None,
        })
        .asset()
        .is_none());
        assert!(with(Status::Downloading {
            latest: "0.2.0".to_owned(),
        })
        .asset()
        .is_none());
    }
}
