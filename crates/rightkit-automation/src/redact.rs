//! Screenshot redaction for QA evidence. A capture of a window that shows a
//! secure field, a credential-labelled field, or any region the caller's
//! [`Redactor`] names is rewritten with those regions painted solid black
//! **before** the file is returned. Fail closed: if a region must be masked and
//! the PNG cannot be decoded or rewritten, the capture is deleted and the call
//! errors; an unmasked capture is never handed back.

use crate::model::*;

/// What a redaction hook can see about one capture.
pub struct ShotContext<'a> {
    pub pid: i32,
    pub app_id: &'a str,
    pub window_id: u64,
    /// Window bounds in screen points.
    pub window: Rect,
    /// The accessibility tree read at capture time (frames are screen points).
    pub snapshot: &'a Snapshot,
}

/// Caller hook: extra screen-space regions (in points) to mask, e.g. a billing
/// panel or an account name. Called for every capture.
pub trait Redactor: Send + Sync {
    fn regions(&self, ctx: &ShotContext) -> Vec<Rect>;
}

/// Mask every node whose accessible name or value matches `needle` (case-insensitive).
pub struct MaskText(pub Vec<String>);
impl Redactor for MaskText {
    fn regions(&self, ctx: &ShotContext) -> Vec<Rect> {
        let mut out = vec![];
        fn walk(n: &Node, needles: &[String], out: &mut Vec<Rect>) {
            let hit = |t: &Option<String>| t.as_deref().is_some_and(|t| needles.iter().any(|x| t.to_lowercase().contains(&x.to_lowercase())));
            if hit(&n.name) || hit(&n.value) || hit(&n.description) {
                if let Some(f) = n.frame {
                    out.push(f);
                }
            }
            n.children.iter().for_each(|c| walk(c, needles, out));
        }
        walk(&ctx.snapshot.root, &self.0, &mut out);
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShotReport {
    pub window_id: u64,
    pub width: u32,
    pub height: u32,
    /// Regions painted black (built-in secure/credential fields plus hook regions).
    pub masked: usize,
}

/// Built-in regions: secure fields and credential-labelled editable fields.
pub(crate) fn builtin_regions(root: &Node) -> Vec<Rect> {
    let mut out = vec![];
    fn walk(n: &Node, out: &mut Vec<Rect>) {
        let editable = matches!(n.role.as_str(), "textfield" | "textarea" | "combobox");
        let label = [&n.name, &n.description, &n.identifier].iter().filter_map(|x| x.as_deref()).collect::<Vec<_>>().join(" ");
        if n.secure || (editable && crate::policy::credential_label(&label)) {
            if let Some(f) = n.frame {
                out.push(f);
            }
        }
        n.children.iter().for_each(|c| walk(c, out));
    }
    walk(root, &mut out);
    out
}

/// Rewrite the PNG at `path` with `regions` (screen points) painted black.
/// `window` is the capture's bounds in points; pixel scale comes from the PNG.
pub(crate) fn mask_file(path: &std::path::Path, window: Rect, regions: &[Rect]) -> std::result::Result<(u32, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (w, h, mut rgba) = crate::png::decode_rgba(&bytes).ok_or("PNG is not a decodable 8-bit image")?;
    if regions.is_empty() {
        return Ok((w, h));
    }
    if window.w <= 0.0 || window.h <= 0.0 {
        return Err("window bounds unknown".into());
    }
    let (sx, sy) = (f64::from(w) / window.w, f64::from(h) / window.h);
    // Pad by one point: AX frames are rounded and focus rings sit outside them.
    let px: Vec<(i64, i64, i64, i64)> = regions
        .iter()
        .map(|r| {
            (
                ((r.x - window.x - 1.0) * sx).floor() as i64,
                ((r.y - window.y - 1.0) * sy).floor() as i64,
                ((r.x + r.w - window.x + 1.0) * sx).ceil() as i64,
                ((r.y + r.h - window.y + 1.0) * sy).ceil() as i64,
            )
        })
        .collect();
    crate::png::fill_rects(w, h, &mut rgba, &px);
    let out = crate::png::encode_rgba(w, h, &rgba).ok_or("PNG re-encode failed")?;
    let tmp = path.with_extension("redacting");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok((w, h))
}
