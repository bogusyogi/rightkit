//! A [`FrameSource`] over a managed Chrome page (feature `browser-source`; rightkit#1144).
//!
//! The page loads a composition hosted by [`crate::project_server`], whose injected runtime
//! defines `window.__hf_seek(t)` and `window.__hf_meta`. [`BrowserFrameSource::open`] sizes the
//! page, waits for the metadata and returns the source plus [`CompositionMeta`]; `seek` runs the
//! runtime's seek and lets two animation frames (at most 250 ms) pass so the paint matches the
//! seek; `capture` takes the CDP PNG screenshot, with an omit-background capture over a transparent
//! page for alpha renders.
//!
//! The source blocks on the tokio runtime handle it is given (the render driver calls it from the
//! writer thread `run_with_stdin` provides, never from a runtime worker), and owns the per-frame
//! timeout, as the driver adds none. The runtime must have its time driver enabled and keep the
//! browser connection alive on its own workers while a call blocks (a multi-thread runtime).
//!
//! The caller launches the session. Loopback is blocked by default in `rightkit-browser`'s network
//! policy; allow exactly the project server's own host and port:
//!
//! ```ignore
//! let port = handle.port();
//! let network = rightkit_browser::NetworkPolicy::default()
//!     .allow_with(move |r: &rightkit_browser::BrowserRequest| r.host == "127.0.0.1" && r.port == Some(port));
//! ```

use std::sync::Arc;
use std::time::Duration;

use rightkit_browser::{BrowserPage, BrowserSession, ScreenshotOptions};
use serde_json::Value;
use tokio::runtime::Handle;

use crate::render::{FrameError, FrameSource};

/// How long one seek or the first metadata may take before the render fails.
pub const DEFAULT_PAGE_TIMEOUT: Duration = Duration::from_secs(45);

/// What the runtime reported about the composition once it loaded (`window.__hf_meta`).
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionMeta {
    /// Length of the root composition, seconds (`data-duration`); positive.
    pub duration: f64,
    /// Root composition width, pixels.
    pub width: u32,
    /// Root composition height, pixels.
    pub height: u32,
    /// The raw metadata object, for the audio list and timelines.
    pub raw: Value,
}

/// A [`FrameSource`] driving one loaded composition page.
pub struct BrowserFrameSource {
    rt: Handle,
    page: Arc<BrowserPage>,
    alpha: bool,
    timeout: Duration,
    frame: u64,
}

fn failed(error: impl std::fmt::Display) -> FrameError {
    FrameError(format!("browser_failed: {error}"))
}

/// The page expression for frame time `t`: seek everything, then let two animation frames (or
/// 250 ms, if the page gets none) pass so the paint matches the seek before the screenshot.
fn seek_script(t: f64) -> String {
    format!(
        "window.__hf_seek({t:.6}).then(function(){{return Promise.race([new Promise(function(r){{requestAnimationFrame(function(){{requestAnimationFrame(function(){{r(true)}})}})}}),new Promise(function(r){{setTimeout(function(){{r(true)}},250)}})])}})"
    )
}

impl BrowserFrameSource {
    /// Open `url` (the [`crate::project_server::Handle::url`]) in a new page of `session`, size the
    /// viewport to `width` x `height` at scale 1, make the background transparent when `alpha`,
    /// and wait for the runtime's metadata. Call from async code on the runtime `rt` belongs to.
    pub async fn open(
        rt: Handle,
        session: &BrowserSession,
        url: &str,
        width: u32,
        height: u32,
        alpha: bool,
        timeout: Duration,
    ) -> Result<(Self, CompositionMeta), FrameError> {
        let page = session.new_page(url).await.map_err(failed)?;
        page.set_viewport(width, height, 1.0)
            .await
            .map_err(failed)?;
        if alpha {
            // Fully transparent default background, so uncovered page area captures as alpha 0.
            page.set_transparent_background(true)
                .await
                .map_err(failed)?;
        }
        page.wait_eval("!!window.__hf_meta", timeout)
            .await
            .map_err(|e| {
                FrameError(format!(
                    "page_timeout: the composition did not respond ({e})"
                ))
            })?;
        let raw = page.eval("window.__hf_meta").await.map_err(failed)?;
        let duration = raw["duration"]
            .as_f64()
            .filter(|d| *d > 0.0)
            .ok_or_else(|| {
                FrameError("no_duration: the root composition needs data-duration".into())
            })?;
        let dimension = |key: &str| raw[key].as_f64().filter(|v| *v >= 1.0).unwrap_or(0.0) as u32;
        let meta = CompositionMeta {
            duration,
            width: dimension("width"),
            height: dimension("height"),
            raw,
        };
        Ok((
            Self {
                rt,
                page,
                alpha,
                timeout,
                frame: 0,
            },
            meta,
        ))
    }

    /// The page, for the caller's own reads (console lines, exceptions).
    pub fn page(&self) -> &Arc<BrowserPage> {
        &self.page
    }
}

impl FrameSource for BrowserFrameSource {
    fn seek(&mut self, time_s: f64) -> Result<(), FrameError> {
        let (page, timeout, frame) = (&self.page, self.timeout, self.frame);
        self.rt.block_on(async {
            let seek = tokio::time::timeout(timeout, page.eval(&seek_script(time_s)))
                .await
                .map_err(|_| {
                    FrameError(format!("page_timeout: frame {frame} did not become ready"))
                })?;
            seek.map(|_| ()).map_err(failed)
        })
    }

    fn capture(&mut self) -> Result<Vec<u8>, FrameError> {
        let (page, alpha) = (&self.page, self.alpha);
        let png = self.rt.block_on(async {
            let shot = if alpha {
                page.screenshot_with(ScreenshotOptions {
                    omit_background: true,
                    ..Default::default()
                })
                .await
            } else {
                page.screenshot().await
            };
            shot.map_err(failed)
        })?;
        self.frame += 1;
        Ok(png)
    }
}
