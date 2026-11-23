//! Fail-closed routing for events a shell emits from Rust to renderer webviews.
//!
//! A webview's `listen` permission is unscoped, so a tracked route manifest is the authority that
//! prevents an event from being broadcast to a webview that does not consume it. This module owns
//! the manifest rules (schema, allowed targets, no duplicate or empty entries), the encoded
//! dispatch script and the delivery loop; the shell owns the manifest file, its schema version, its
//! window labels, the dispatch event name and the window lookup.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A parsed, validated route manifest: event name to target window labels.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteManifest {
    schema_version: u32,
    routes: BTreeMap<String, Vec<String>>,
}

impl RouteManifest {
    /// Parse and validate `json`. `schema_version` is the version the shell supports and
    /// `allowed_targets` the window labels it may route to.
    pub fn parse(
        json: &str,
        schema_version: u32,
        allowed_targets: &[&str],
    ) -> Result<Self, String> {
        let manifest: RouteManifest = serde_json::from_str(json)
            .map_err(|error| format!("invalid renderer event route manifest: {error}"))?;
        if manifest.schema_version != schema_version {
            return Err(format!(
                "unsupported renderer event route schema {}; expected {schema_version}",
                manifest.schema_version
            ));
        }
        if manifest.routes.is_empty() {
            return Err("renderer event route manifest must not be empty".into());
        }

        let allowed_targets = allowed_targets.iter().copied().collect::<BTreeSet<_>>();
        for (event, targets) in &manifest.routes {
            if event.trim().is_empty() {
                return Err("renderer event route contains an empty event name".into());
            }
            if targets.is_empty() {
                return Err(format!("renderer event {event:?} has no target windows"));
            }
            let mut unique_targets = BTreeSet::new();
            for target in targets {
                if !allowed_targets.contains(target.as_str()) {
                    return Err(format!(
                        "renderer event {event:?} targets unsupported window {target:?}"
                    ));
                }
                if !unique_targets.insert(target) {
                    return Err(format!(
                        "renderer event {event:?} repeats target window {target:?}"
                    ));
                }
            }
        }

        Ok(manifest)
    }

    /// The windows `event` is routed to, or an error for an event the manifest does not know.
    pub fn targets(&self, event: &str) -> Result<&[String], String> {
        self.routes
            .get(event)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("unknown renderer event {event:?}"))
    }
}

#[derive(Serialize)]
struct RendererEventEnvelope<'a, S: ?Sized> {
    event: &'a str,
    payload: &'a S,
}

/// The script that dispatches `event` with `payload` as a `CustomEvent` named `channel` in the
/// window it is evaluated in. The envelope travels base64-encoded so no payload byte can break out
/// of the script.
pub fn eval_script<S>(channel: &str, event: &str, payload: &S) -> Result<String, String>
where
    S: Serialize + ?Sized,
{
    let envelope = serde_json::to_vec(&RendererEventEnvelope { event, payload })
        .map_err(|error| format!("could not encode renderer event {event:?}: {error}"))?;
    let encoded = rightkit_crypto::base64_encode(envelope.as_ref());
    let encoded_channel = serde_json::to_string(channel)
        .map_err(|_| "could not encode renderer channel".to_string())?;
    Ok(format!(
        "window.dispatchEvent(new CustomEvent({encoded_channel},{{detail:JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(\"{encoded}\"),char=>char.charCodeAt(0))))}}));"
    ))
}

/// Call `deliver` for every target; every target is tried and the first error is returned.
pub fn deliver_to_targets<F>(targets: &[String], mut deliver: F) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
{
    let mut first_error = None;
    for target in targets {
        if let Err(error) = deliver(target) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// A drag position in physical pixels.
#[derive(Debug, Serialize, PartialEq)]
pub struct NativeDragDropPosition {
    pub x: f64,
    pub y: f64,
}

/// A native drag and drop event in the shape the renderer's JavaScript API produced.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NativeDragDropPayload {
    Enter {
        paths: Vec<String>,
        position: NativeDragDropPosition,
    },
    Over {
        position: NativeDragDropPosition,
    },
    Drop {
        paths: Vec<String>,
        position: NativeDragDropPosition,
    },
    Leave,
}

/// Dropped paths as the renderer receives them (lossy UTF-8).
pub fn drag_drop_paths(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}
