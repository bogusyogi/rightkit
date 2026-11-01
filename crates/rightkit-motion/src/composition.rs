//! The render core's input: a composition document. Every frame is a pure
//! function of the frame index (`n / fps`), never of wall-clock time. An HTML
//! project is one `html` layer; beat templates (text, logo, phone, window,
//! input, chat, cards, list, chart) register in `hf-runtime.js`
//! (`window.__hf_templates`) and in [`KNOWN_TEMPLATES`], and need no change to
//! the capture, encode or protocol code.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Template kinds the runtime registers. Add a kind here and in the runtime.
pub const KNOWN_TEMPLATES: &[&str] = &["html"];
pub const MAX_FRAMES: u64 = 120_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub template: String,
    #[serde(default)]
    pub props: Value,
    pub start_frame: u64,
    pub end_frame: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionDoc {
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    /// 0 means "take the length from the HTML composition" (legacy requests).
    #[serde(default)]
    pub duration_frames: u64,
    pub layers: Vec<Layer>,
}

impl CompositionDoc {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=120).contains(&self.fps) {
            return Err("invalid_document: fps must be between 1 and 120".into());
        }
        if self.width < 16 || self.height < 16 || self.width > 7680 || self.height > 7680 {
            return Err("invalid_document: width and height must be between 16 and 7680".into());
        }
        if self.duration_frames > MAX_FRAMES {
            return Err(format!(
                "invalid_document: {} frames exceeds the {MAX_FRAMES} frame limit",
                self.duration_frames
            ));
        }
        if self.layers.is_empty() {
            return Err("invalid_document: at least one layer is required".into());
        }
        for (i, l) in self.layers.iter().enumerate() {
            if !KNOWN_TEMPLATES.contains(&l.template.as_str()) {
                return Err(format!(
                    "unknown_template: layer {i} uses \"{}\"; registered kinds: {}",
                    l.template,
                    KNOWN_TEMPLATES.join(", ")
                ));
            }
            if l.end_frame <= l.start_frame {
                return Err(format!(
                    "invalid_document: layer {i} end_frame must be after start_frame"
                ));
            }
            if self.duration_frames > 0 && l.end_frame > self.duration_frames {
                return Err(format!(
                    "invalid_document: layer {i} ends after duration_frames"
                ));
            }
        }
        if self.layers.iter().filter(|l| l.template == "html").count() > 1 {
            return Err("unsupported_layers: one html layer per document".into());
        }
        Ok(())
    }

    /// The single HTML layer's `(project_dir, entry_file)`.
    pub fn html_source(&self) -> Result<(String, String), String> {
        let l = self
            .layers
            .iter()
            .find(|l| l.template == "html")
            .ok_or("unsupported_layers: the document has no html layer to load")?;
        let dir = l.props["project_dir"]
            .as_str()
            .ok_or("invalid_document: html layer needs props.project_dir")?;
        let entry = l.props["entry_file"].as_str().unwrap_or("index.html");
        Ok((dir.to_string(), entry.to_string()))
    }

    /// Document for a plain HTML project (legacy request shape).
    pub fn from_html(
        project_dir: &str,
        entry: &str,
        fps: u32,
        width: u32,
        height: u32,
        duration_frames: u64,
    ) -> Self {
        Self {
            fps,
            width,
            height,
            duration_frames,
            layers: vec![Layer {
                template: "html".into(),
                props: serde_json::json!({ "project_dir": project_dir, "entry_file": entry }),
                start_frame: 0,
                end_frame: duration_frames.max(1),
            }],
        }
    }

    /// Seconds for frame `n`, from the index alone.
    pub fn time_of(&self, n: u64) -> f64 {
        n as f64 / self.fps as f64
    }
}
