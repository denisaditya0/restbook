//! The Updates page's state: what the last check found, whether a download is
//! running, and where the package landed.
//!
//! An entity rather than fields in the dialog builder, because that builder is a
//! plain closure the kit runs again on every frame: anything stored in it is
//! rebuilt, and a check that reports back later would have nothing to report to.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{v_flex, WindowExt as _};
use gpui_kit::*;

use crate::theme::Metrics;
use crate::updater::{self, Asset};
use crate::workspace::settings;
use crate::workspace::Workspace;

/// How long the app waits before closing after it opens a Windows installer.
/// Long enough for the dialog to paint the sentence that says it is closing.
const QUIT_GRACE: Duration = Duration::from_millis(1500);

/// A newer release the launch check found, held until render can open the
/// dialog offering it. The `Asset` is what [`Updates::install`] downloads, so an
/// offer with no asset for this platform is never parked.
pub(crate) struct PendingUpdate {
    pub(crate) latest: String,
    pub(crate) notes: Option<String>,
    pub(crate) asset: Asset,
}

impl Workspace {
    /// Ask whether to install a newer release, without sending the user to
    /// Settings first.
    ///
    /// A plain dialog with a composed footer, not an alert: an alert's
    /// confirm/cancel pair has no room for *Skip this version*, and nothing here
    /// is destructive. The download, the `sha256` check, and handing the package
    /// to the OS are [`Updates::install`]'s job; this decides whether to start it.
    pub(crate) fn open_update_offer_dialog(
        &mut self,
        offer: PendingUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings.clone();
        let metrics = Metrics::current(cx);
        // The download runs in an entity the dialog does not own, so it survives
        // the dialog closing the moment the button is pressed.
        let updates = cx.new(|_| Updates::new());

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let PendingUpdate {
                latest,
                notes,
                asset,
            } = &offer;

            let skip = Button::new("update-offer-skip")
                .label("Skip this version")
                .ghost()
                .on_click({
                    let settings = settings.clone();
                    let latest = latest.clone();
                    move |_, window, cx| {
                        settings.update(cx, |settings, cx| {
                            let mut next = settings.current().clone();
                            next.skipped_update = Some(latest.clone());
                            settings.update(next, cx);
                        });
                        window.close_dialog(cx);
                    }
                });
            let later = Button::new("update-offer-later")
                .label("Later")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let install = Button::new("update-offer-install")
                .label("Install now")
                .primary()
                .on_click({
                    let updates = updates.clone();
                    let latest = latest.clone();
                    let asset = asset.clone();
                    move |_, window, cx| {
                        updates.update(cx, |updates, cx| {
                            updates.install_offered(latest.clone(), asset.clone(), cx)
                        });
                        window.close_dialog(cx);
                    }
                });

            let detail = match notes.as_deref().map(str::trim) {
                Some(notes) if !notes.is_empty() => v_flex()
                    .gap(px(metrics.space_sm))
                    .child(
                        div()
                            .text_sm()
                            .child(format!("restbook {latest} is available.")),
                    )
                    // The notes are the changelog entry, which is Markdown. A
                    // plain `div` printed `**` and `##` literally — 0.1.4's
                    // release page showed the install block's HTML comment in
                    // the dialog for exactly that reason. A scrollable band
                    // keeps a long entry from stretching the dialog.
                    .child(
                        div().id("update-offer-notes").max_h(px(240.)).child(
                            TextView::markdown("update-offer-notes-text", notes.to_owned())
                                .scrollable(true),
                        ),
                    ),
                _ => v_flex().child(
                    div()
                        .text_sm()
                        .child(format!("restbook {latest} is available.")),
                ),
            };

            dialog
                .title("Update available")
                .w_112()
                .child(v_flex().py(px(metrics.space_sm)).child(detail))
                .footer(DialogFooter::new().child(skip).child(later).child(install))
        });
    }
}

/// Whether a found release should be offered to the user.
///
/// Two ways a newer version is still not worth interrupting for: the user
/// already said *Skip this version*, or the manifest publishes no build for
/// this platform, in which case an offer could not be acted on. The second is
/// the caller's business — it has the `Option<Asset>` — so this is the skip
/// rule alone, kept apart from the task so it can be tested without a manifest.
pub(crate) fn should_offer(current: &settings::AppSettings, latest: &str) -> bool {
    current.skipped_update.as_deref() != Some(latest)
}

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
                // The offer dialog is handed the notes straight from the
                // outcome (`PendingUpdate`); the status is read by the Settings
                // page, which needs only the version and whether a build
                // exists. Carrying the prose here too made it dead weight.
                notes: _,
                asset,
            }) => Status::Available { latest, asset },
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
