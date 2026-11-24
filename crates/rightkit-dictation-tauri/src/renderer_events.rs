//! Tauri glue over [`crate::renderer_routes`] (feature `tauri`): fail-closed delivery of an event
//! emitted from Rust to the webview windows its route manifest names, and the conversion of Tauri's
//! native drag and drop event into the renderer payload.
//!
//! The manifest rules, the encoded dispatch script and the delivery loop are
//! [`crate::renderer_routes`]. The host supplies its manifest text, schema version, allowed window
//! labels and the `CustomEvent` name its renderer listens for; nothing here names a product, a
//! window or an event.

use std::sync::OnceLock;

use serde::Serialize;
use tauri::{Manager, Runtime};

use crate::renderer_routes::{self as routes, NativeDragDropPayload, NativeDragDropPosition};

fn drag_drop_position(position: &tauri::PhysicalPosition<f64>) -> NativeDragDropPosition {
    NativeDragDropPosition {
        x: position.x,
        y: position.y,
    }
}

/// Converts Tauri's native drag/drop event into the renderer payload shape its
/// JavaScript convenience API previously produced.
pub fn native_drag_drop_payload(event: &tauri::DragDropEvent) -> Option<NativeDragDropPayload> {
    match event {
        tauri::DragDropEvent::Enter { paths, position } => Some(NativeDragDropPayload::Enter {
            paths: routes::drag_drop_paths(paths),
            position: drag_drop_position(position),
        }),
        tauri::DragDropEvent::Over { position } => Some(NativeDragDropPayload::Over {
            position: drag_drop_position(position),
        }),
        tauri::DragDropEvent::Drop { paths, position } => Some(NativeDragDropPayload::Drop {
            paths: routes::drag_drop_paths(paths),
            position: drag_drop_position(position),
        }),
        tauri::DragDropEvent::Leave => Some(NativeDragDropPayload::Leave),
        _ => None,
    }
}

/// A host's renderer-event router: its route manifest, parsed once and kept. Declare it as a
/// `static` and call [`RendererEvents::emit`].
pub struct RendererEvents {
    manifest_json: &'static str,
    schema_version: u32,
    allowed_targets: &'static [&'static str],
    channel: &'static str,
    manifest: OnceLock<Result<routes::RouteManifest, String>>,
}

impl RendererEvents {
    /// `manifest_json` is the route manifest text, `schema_version` the version this host
    /// supports, `allowed_targets` the window labels it may route to and `channel` the
    /// `CustomEvent` name its renderer listens for.
    pub const fn new(
        manifest_json: &'static str,
        schema_version: u32,
        allowed_targets: &'static [&'static str],
        channel: &'static str,
    ) -> Self {
        Self {
            manifest_json,
            schema_version,
            allowed_targets,
            channel,
            manifest: OnceLock::new(),
        }
    }

    fn manifest(&self) -> Result<&routes::RouteManifest, String> {
        let parsed = self.manifest.get_or_init(|| {
            routes::RouteManifest::parse(
                self.manifest_json,
                self.schema_version,
                self.allowed_targets,
            )
        });
        match parsed {
            Ok(manifest) => Ok(manifest),
            Err(error) => Err(error.clone()),
        }
    }

    /// Delivers a known renderer event only to the exact webview windows in the route manifest.
    /// A missing target window is intentionally a benign no-op; an event the manifest does not
    /// know is an error.
    pub fn emit<R, S>(
        &self,
        app: &tauri::AppHandle<R>,
        event: &str,
        payload: &S,
    ) -> Result<(), String>
    where
        R: Runtime,
        S: Serialize + ?Sized,
    {
        let targets = self.manifest()?.targets(event)?;
        let script = routes::eval_script(self.channel, event, payload)?;
        routes::deliver_to_targets(targets, |target| {
            let Some(window) = app.get_webview_window(target) else {
                return Ok(());
            };
            window.eval(&script).map_err(|error| {
                format!("failed to deliver renderer event {event:?} to {target:?}: {error}")
            })
        })
    }
}
