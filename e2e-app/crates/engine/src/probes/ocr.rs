//! rightkit-ocr: recognise the crate's own canary image through the platform's native backend (Apple Vision / Windows.Media.Ocr)
//! as the router selects it, and check the text came back.
use rightkit_ocr::{canary_png, Ocr, OcrOptions};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let mut ocr = Ocr::new(OcrOptions::default());
    let result = ocr.recognize_encoded(canary_png()).map_err(|e| format!("recognize the canary: {e}"))?;
    let text = result.text();
    if text.trim().is_empty() {
        return Err("the native backend returned no text for the canary image".into());
    }
    Ok(json!({ "text": text.trim() }))
}
