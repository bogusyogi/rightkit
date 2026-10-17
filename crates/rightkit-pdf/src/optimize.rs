//! Size reduction: unused-object clean-up, stream compression and conservative image downsampling
//! (feature `optimize`).
//!
//! Design reference: pdfcraft `crates/optimize` (MIT OR Apache-2.0; no code is copied). This is the
//! first step the issue asks for: it does **not** merge page-printer strips, convert colour spaces
//! or re-encode JPEG.
//!
//! # What it does
//!
//! 1. **Measures** the effective resolution of every image: the page content (and every form it
//!    paints, up to [`MAX_FORM_DEPTH`] deep) is walked with the CTM, and each `Do` of an image gives
//!    pixels per inch along both axes. The smallest value over all draws is the image's effective
//!    resolution (the draw that shows it largest).
//! 2. **Resamples** an image only when that resolution is known and exceeds the target by
//!    [`OptimizeOptions::resample_threshold`]: it is box-filtered (area average) to
//!    `target / effective` of its size, so the draw that shows it largest is at the target
//!    resolution and every other draw is above it. The new data is Flate-compressed and used only if
//!    it is smaller than what the file held.
//! 3. **Removes unused objects** (anything not reachable from the trailer) and Flate-compresses
//!    other uncompressed streams.
//!
//! # When it refuses to resample (always reported in [`OptimizeReport::skipped`])
//!
//! The image's resolution is *unknown* ([`SkipReason::NotMeasurable`]) when any of these holds, and
//! it is then left byte for byte as it was:
//!
//! - it is referenced from anywhere other than an `/XObject` resource dictionary (as a `/SMask`,
//!   `/Mask`, in an array, in a pattern);
//! - a resource dictionary that lists it belongs to something the walk did not execute: a form that
//!   no page paints (an annotation appearance), a pattern, a Type 3 font, a form nested deeper than
//!   the limit;
//! - a draw has a singular or non-finite matrix, or the walk hit its work limit or could not read a
//!   content stream (then no image is resampled at all);
//! - it is never drawn.
//!
//! Also left alone ([`SkipReason::UnsupportedFormat`]): anything but 8-bit `DeviceGray` or
//! `DeviceRGB` with no `/Decode`, `/Mask` or `/ImageMask`, and any stream whose filter is not empty
//! or Flate or LZW or ASCII85 (a JPEG, CCITT, JBIG2 or JPX image is not re-encoded). `/SMask` of a
//! resampled image is untouched (its size is independent of the image's).

use std::collections::{BTreeSet, HashMap, HashSet};

use lopdf::content::Operation;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::Serialize;

use crate::inspect::{deref_obj, load_pdf, object_number, resolve_inherited, ContentBudget};
use crate::model::Matrix;

/// Deepest chain of forms walked to measure an image. A form below this is not walked, and an
/// image that only it lists is not resampled.
pub const MAX_FORM_DEPTH: usize = 12;
const MAX_INVOCATIONS: usize = 20_000;
const MAX_Q_DEPTH: usize = 1 << 12;

#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    #[error("PDF could not be parsed: {0}")]
    Parse(String),
    #[error("encrypted PDF cannot be optimized")]
    Encrypted,
    #[error("invalid options: {0}")]
    InvalidOptions(&'static str),
    #[error("cancelled")]
    Cancelled,
    #[error("serialize failed: {0}")]
    Write(String),
}

/// What to do. [`OptimizeOptions::standard`] and [`OptimizeOptions::mobile`] are the presets.
#[derive(Clone, Debug)]
pub struct OptimizeOptions {
    /// Target resolution of the draw that shows an image largest, in pixels per inch (36..=600).
    pub target_ppi: f64,
    /// Resample only when the measured resolution is above `target_ppi * resample_threshold`
    /// (at least 1.0).
    pub resample_threshold: f64,
    /// Skip images whose decoded size is larger than this many bytes.
    pub max_image_bytes: usize,
    pub resample_images: bool,
    /// Flate-compress streams that have no filter.
    pub compress_streams: bool,
}

impl OptimizeOptions {
    /// 150 ppi, for screen and ordinary print.
    pub fn standard() -> Self {
        OptimizeOptions {
            target_ppi: 150.0,
            resample_threshold: 1.25,
            max_image_bytes: 64 * 1024 * 1024,
            resample_images: true,
            compress_streams: true,
        }
    }

    /// 96 ppi, for small screens.
    pub fn mobile() -> Self {
        OptimizeOptions {
            target_ppi: 96.0,
            ..Self::standard()
        }
    }
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The effective resolution is unknown (see the module documentation).
    NotMeasurable,
    /// Not a format this module resamples.
    UnsupportedFormat,
    /// Already at or near the target.
    BelowTarget,
    /// The decoded image is larger than `max_image_bytes`, or its data does not match its header.
    BadData,
    /// The resampled data would not be smaller than what the file holds.
    NotSmaller,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageReport {
    pub object: (u32, u16),
    pub width: u32,
    pub height: u32,
    /// Effective resolution when it was measured.
    pub effective_ppi: Option<f64>,
    pub new_width: Option<u32>,
    pub new_height: Option<u32>,
    pub skipped: Option<SkipReason>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeReport {
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub objects_removed: usize,
    pub streams_compressed: usize,
    pub images: Vec<ImageReport>,
    pub images_resampled: usize,
    /// True when the optimised file was not smaller, and the input is returned unchanged.
    pub kept_original: bool,
}

#[derive(Debug)]
pub struct OptimizeOutcome {
    pub bytes: Vec<u8>,
    pub report: OptimizeReport,
}

/// Where [`optimize_with_progress`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Measuring,
    Resampling,
    Cleaning,
    Writing,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub stage: Stage,
    /// Items done and in total for the stage (images for `Resampling`, otherwise 0 of 1 or 1 of 1).
    pub done: usize,
    pub total: usize,
}

/// [`optimize_with_progress`] without a callback.
pub fn optimize(
    source: &[u8],
    options: &OptimizeOptions,
) -> Result<OptimizeOutcome, OptimizeError> {
    optimize_with_progress(source, options, &mut |_| true)
}

/// Optimises `source`. `progress` is called between steps; returning `false` cancels with
/// [`OptimizeError::Cancelled`] and nothing is returned.
pub fn optimize_with_progress(
    source: &[u8],
    options: &OptimizeOptions,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<OptimizeOutcome, OptimizeError> {
    if !(36.0..=600.0).contains(&options.target_ppi) || !options.target_ppi.is_finite() {
        return Err(OptimizeError::InvalidOptions(
            "target_ppi must be within 36..=600",
        ));
    }
    if options.resample_threshold.is_nan() || options.resample_threshold < 1.0 {
        return Err(OptimizeError::InvalidOptions(
            "resample_threshold must be at least 1",
        ));
    }
    let mut step = |stage: Stage, done: usize, total: usize| -> Result<(), OptimizeError> {
        if progress(&Progress { stage, done, total }) {
            Ok(())
        } else {
            Err(OptimizeError::Cancelled)
        }
    };
    let mut doc = load_pdf(source).map_err(|e| OptimizeError::Parse(e.to_string()))?;
    if doc.is_encrypted() {
        return Err(OptimizeError::Encrypted);
    }
    let mut report = OptimizeReport {
        bytes_before: source.len(),
        ..OptimizeReport::default()
    };

    step(Stage::Measuring, 0, 1)?;
    let plan = if options.resample_images {
        plan_images(&doc, options)
    } else {
        Vec::new()
    };
    step(Stage::Measuring, 1, 1)?;

    let total = plan.len();
    for (index, entry) in plan.into_iter().enumerate() {
        step(Stage::Resampling, index, total)?;
        let mut image = entry.report;
        if let Some(scale) = entry.scale {
            match resample(&mut doc, entry.id, scale, options) {
                Ok((w, h)) => {
                    image.new_width = Some(w);
                    image.new_height = Some(h);
                    report.images_resampled += 1;
                }
                Err(reason) => image.skipped = Some(reason),
            }
        }
        report.images.push(image);
    }
    step(Stage::Resampling, total, total)?;

    step(Stage::Cleaning, 0, 1)?;
    report.objects_removed = doc.prune_objects().len();
    if options.compress_streams {
        let before = count_filtered(&doc);
        doc.compress();
        report.streams_compressed = count_filtered(&doc).saturating_sub(before);
    }
    step(Stage::Cleaning, 1, 1)?;

    step(Stage::Writing, 0, 1)?;
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| OptimizeError::Write(e.to_string()))?;
    step(Stage::Writing, 1, 1)?;
    if bytes.len() >= source.len() {
        report.kept_original = true;
        report.bytes_after = source.len();
        return Ok(OptimizeOutcome {
            bytes: source.to_vec(),
            report,
        });
    }
    report.bytes_after = bytes.len();
    Ok(OptimizeOutcome { bytes, report })
}

fn count_filtered(doc: &Document) -> usize {
    doc.objects
        .values()
        .filter(|o| o.as_stream().is_ok_and(|s| s.dict.has(b"Filter")))
        .count()
}

// ---------------------------------------------------------------------------------------------
// Census: where every image is listed and referenced
// ---------------------------------------------------------------------------------------------

fn pointer(dict: &Dictionary) -> usize {
    dict as *const Dictionary as usize
}

fn is_image_stream(stream: &Stream) -> bool {
    stream
        .dict
        .get(b"Subtype")
        .ok()
        .and_then(|s| s.as_name().ok())
        .is_some_and(|n| n == &b"Image"[..])
}

#[derive(Default)]
struct Census {
    images: BTreeSet<ObjectId>,
    /// Addresses of the dictionaries that are the value of an `/XObject` key.
    xobject_dicts: HashSet<usize>,
    /// Image id -> addresses of the `/XObject` dictionaries that list it.
    containers: HashMap<ObjectId, HashSet<usize>>,
    /// Images referenced from anywhere other than an `/XObject` dictionary.
    other_refs: HashSet<ObjectId>,
    /// `/XObject` dictionaries of resources owned by something other than a page or a form.
    foreign: HashSet<usize>,
    /// (form stream, address of the `/XObject` dictionary of its resources).
    form_owned: Vec<(ObjectId, usize)>,
}

fn xobject_dict_of<'a>(doc: &'a Document, resources: &'a Object) -> Option<&'a Dictionary> {
    deref_obj(doc, resources)?
        .as_dict()
        .ok()?
        .get(b"XObject")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
}

fn find_xobject_dicts(doc: &Document, object: &Object, census: &mut Census, depth: usize) {
    if depth > 64 {
        return;
    }
    match object {
        Object::Array(items) => {
            for item in items {
                find_xobject_dicts(doc, item, census, depth + 1);
            }
        }
        Object::Dictionary(dict) => find_in_dict(doc, dict, census, depth),
        Object::Stream(stream) => find_in_dict(doc, &stream.dict, census, depth),
        _ => {}
    }
}

fn find_in_dict(doc: &Document, dict: &Dictionary, census: &mut Census, depth: usize) {
    if let Some(x) = dict
        .get(b"XObject")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
    {
        census.xobject_dicts.insert(pointer(x));
    }
    for (_, value) in dict.iter() {
        find_xobject_dicts(doc, value, census, depth + 1);
    }
}

fn census_object(object: &Object, census: &mut Census, depth: usize) {
    if depth > 64 {
        return;
    }
    match object {
        Object::Reference(id) => {
            if census.images.contains(id) {
                census.other_refs.insert(*id);
            }
        }
        Object::Array(items) => {
            for item in items {
                census_object(item, census, depth + 1);
            }
        }
        Object::Dictionary(dict) => census_dict(dict, census, depth),
        Object::Stream(stream) => census_dict(&stream.dict, census, depth),
        _ => {}
    }
}

fn census_dict(dict: &Dictionary, census: &mut Census, depth: usize) {
    let is_xobject_dict = census.xobject_dicts.contains(&pointer(dict));
    for (_, value) in dict.iter() {
        if is_xobject_dict {
            if let Object::Reference(id) = value {
                if census.images.contains(id) {
                    census
                        .containers
                        .entry(*id)
                        .or_default()
                        .insert(pointer(dict));
                    continue;
                }
            }
        }
        census_object(value, census, depth + 1);
    }
}

fn take_census(doc: &Document) -> Census {
    let mut census = Census::default();
    for (id, object) in &doc.objects {
        if let Object::Stream(stream) = object {
            if is_image_stream(stream) {
                census.images.insert(*id);
            }
        }
    }
    for object in doc.objects.values() {
        find_xobject_dicts(doc, object, &mut census, 0);
    }
    for (id, object) in &doc.objects {
        census_object_top(doc, *id, object, &mut census);
    }
    census
}

/// A top-level object: census its contents and note who owns its resources.
fn census_object_top(doc: &Document, id: ObjectId, object: &Object, census: &mut Census) {
    let dict = match object {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return,
    };
    if let Ok(resources) = dict.get(b"Resources") {
        let container = xobject_dict_of(doc, resources).map(pointer);
        let page_type = dict
            .get(b"Type")
            .ok()
            .and_then(|t| t.as_name().ok())
            .unwrap_or_default();
        let is_form = object
            .as_stream()
            .ok()
            .and_then(|s| s.dict.get(b"Subtype").ok())
            .and_then(|s| s.as_name().ok())
            .is_some_and(|n| n == &b"Form"[..]);
        if let Some(container) = container {
            if page_type == b"Page" || page_type == b"Pages" {
                // Owners the walk reaches through the page tree.
            } else if is_form {
                census.form_owned.push((id, container));
            } else {
                census.foreign.insert(container);
            }
        }
    }
    census_object(object, census, 0);
    // A nested dictionary that carries its own /Resources (a Type 3 font's, a pattern's) is not walked.
    mark_nested_owners(doc, dict, census, 0);
}

fn mark_nested_owners(doc: &Document, dict: &Dictionary, census: &mut Census, depth: usize) {
    if depth > 16 {
        return;
    }
    for (_, value) in dict.iter() {
        let nested: Option<&Dictionary> = match value {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        };
        if let Some(nested) = nested {
            if let Ok(resources) = nested.get(b"Resources") {
                if let Some(x) = xobject_dict_of(doc, resources) {
                    census.foreign.insert(pointer(x));
                }
            }
            mark_nested_owners(doc, nested, census, depth + 1);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Measuring: the page walk
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Gs {
    ctm: Matrix,
}

#[derive(Default)]
struct Measured {
    /// Image id -> (width, height) in points of each draw.
    draws: HashMap<ObjectId, Vec<(f64, f64)>>,
    unmeasurable: HashSet<ObjectId>,
    visited_forms: HashSet<ObjectId>,
    visited_containers: HashSet<usize>,
    /// The walk could not finish: nothing is known.
    exhausted: bool,
}

struct Walker<'a> {
    doc: &'a Document,
    budget: ContentBudget,
    invocations: usize,
    out: Measured,
}

fn matrix_of(operands: &[Object]) -> Option<Matrix> {
    let n: Vec<f64> = operands.iter().filter_map(object_number).collect();
    if n.len() != 6 || n.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(Matrix {
        a: n[0],
        b: n[1],
        c: n[2],
        d: n[3],
        e: n[4],
        f: n[5],
    })
}

fn measure(doc: &Document) -> Measured {
    let mut walker = Walker {
        doc,
        budget: ContentBudget::new(),
        invocations: 0,
        out: Measured::default(),
    };
    for page_id in doc.page_iter() {
        // One budget per page, as inspection charges it.
        walker.budget = ContentBudget::new();
        if !walker.page(page_id) {
            walker.out.exhausted = true;
            break;
        }
    }
    walker.out
}

impl<'a> Walker<'a> {
    fn page(&mut self, page_id: ObjectId) -> bool {
        let doc = self.doc;
        let Some(page) = doc.get_object(page_id).ok().and_then(|o| o.as_dict().ok()) else {
            return false;
        };
        let resources = resolve_inherited(doc, page, b"Resources")
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_dict().ok());
        if !page.has(b"Contents") {
            return true;
        }
        let Ok(ids) = crate::inspect::page_content_stream_ids(doc, page_id) else {
            return false;
        };
        let mut gs = Gs {
            ctm: Matrix::IDENTITY,
        };
        let mut stack: Vec<Gs> = Vec::new();
        for id in ids {
            let Ok(content) = self.budget.content_to_read(doc, id) else {
                return false;
            };
            if !self.run(&content.operations, resources, &mut gs, &mut stack, 0) {
                return false;
            }
        }
        true
    }

    fn run(
        &mut self,
        ops: &[Operation],
        resources: Option<&'a Dictionary>,
        gs: &mut Gs,
        stack: &mut Vec<Gs>,
        depth: usize,
    ) -> bool {
        let doc = self.doc;
        if let Some(x) = resources.and_then(|r| xobject_dict_of_dict(doc, r)) {
            self.out.visited_containers.insert(pointer(x));
        }
        for op in ops {
            match op.operator.as_str() {
                "q" => {
                    if stack.len() >= MAX_Q_DEPTH {
                        return false;
                    }
                    stack.push(gs.clone());
                }
                "Q" => {
                    if let Some(previous) = stack.pop() {
                        *gs = previous;
                    }
                }
                "cm" => {
                    if let Some(m) = matrix_of(&op.operands) {
                        gs.ctm = m.then(gs.ctm);
                    }
                }
                "Do" => {
                    self.invocations += 1;
                    if self.invocations > MAX_INVOCATIONS {
                        return false;
                    }
                    if !self.do_xobject(op, resources, gs, depth) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn do_xobject(
        &mut self,
        op: &Operation,
        resources: Option<&'a Dictionary>,
        gs: &Gs,
        depth: usize,
    ) -> bool {
        let doc = self.doc;
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            return true;
        };
        let Some(entry) = resources
            .and_then(|r| xobject_dict_of_dict(doc, r))
            .and_then(|x| x.get(name).ok())
        else {
            return true;
        };
        let Some(id) = entry.as_reference().ok() else {
            return true;
        };
        let Some(stream) = doc.get_object(id).ok().and_then(|o| o.as_stream().ok()) else {
            return true;
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .unwrap_or_default();
        match subtype {
            b"Image" => {
                let ctm = gs.ctm;
                let width = ctm.a.hypot(ctm.b);
                let height = ctm.c.hypot(ctm.d);
                let det = ctm.a * ctm.d - ctm.b * ctm.c;
                if !(width.is_finite() && height.is_finite())
                    || width < 1e-6
                    || height < 1e-6
                    || det.abs() < 1e-9
                {
                    self.out.unmeasurable.insert(id);
                } else {
                    self.out.draws.entry(id).or_default().push((width, height));
                }
                true
            }
            b"Form" => {
                if depth >= MAX_FORM_DEPTH {
                    // Not walked: what only it lists stays unmeasured and is not resampled.
                    return true;
                }
                self.out.visited_forms.insert(id);
                let matrix = stream
                    .dict
                    .get(b"Matrix")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .and_then(|a| matrix_of(a))
                    .unwrap_or(Matrix::IDENTITY);
                let form_resources = stream
                    .dict
                    .get(b"Resources")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_dict().ok())
                    .or(resources);
                let Ok(content) = self.budget.content_to_read(doc, id) else {
                    return false;
                };
                let mut inner = Gs {
                    ctm: matrix.then(gs.ctm),
                };
                let mut inner_stack: Vec<Gs> = Vec::new();
                self.run(
                    &content.operations,
                    form_resources,
                    &mut inner,
                    &mut inner_stack,
                    depth + 1,
                )
            }
            _ => true,
        }
    }
}

fn xobject_dict_of_dict<'a>(
    doc: &'a Document,
    resources: &'a Dictionary,
) -> Option<&'a Dictionary> {
    resources
        .get(b"XObject")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
}

// ---------------------------------------------------------------------------------------------
// Deciding and resampling
// ---------------------------------------------------------------------------------------------

struct PlannedImage {
    id: ObjectId,
    report: ImageReport,
    /// `Some(scale)` when the image is to be resampled to `scale` of its size per axis.
    scale: Option<f64>,
}

fn plan_images(doc: &Document, options: &OptimizeOptions) -> Vec<PlannedImage> {
    let census = take_census(doc);
    let measured = measure(doc);
    let mut foreign: HashSet<usize> = census.foreign.clone();
    for (form, container) in &census.form_owned {
        if !measured.visited_forms.contains(form) {
            foreign.insert(*container);
        }
    }
    let mut plan = Vec::new();
    for id in &census.images {
        let Some(stream) = doc.get_object(*id).ok().and_then(|o| o.as_stream().ok()) else {
            continue;
        };
        let width = dict_u32(doc, &stream.dict, b"Width").unwrap_or(0);
        let height = dict_u32(doc, &stream.dict, b"Height").unwrap_or(0);
        let mut report = ImageReport {
            object: (id.0, id.1),
            width,
            height,
            effective_ppi: None,
            new_width: None,
            new_height: None,
            skipped: None,
        };
        let skip = |mut report: ImageReport, reason: SkipReason| {
            report.skipped = Some(reason);
            PlannedImage {
                id: *id,
                report,
                scale: None,
            }
        };

        // Known resolution first: an image whose draws are unknown is reported as such whatever its format.
        let containers = census.containers.get(id);
        let measurable = !measured.exhausted
            && !census.other_refs.contains(id)
            && !measured.unmeasurable.contains(id)
            && containers.is_some_and(|set| {
                !set.is_empty()
                    && set
                        .iter()
                        .all(|p| measured.visited_containers.contains(p) && !foreign.contains(p))
            })
            && measured.draws.get(id).is_some_and(|d| !d.is_empty());
        if !measurable || width == 0 || height == 0 {
            plan.push(skip(report, SkipReason::NotMeasurable));
            continue;
        }
        let ppi = measured
            .draws
            .get(id)
            .into_iter()
            .flatten()
            .map(|(w_pt, h_pt)| {
                let across = f64::from(width) / (w_pt / 72.0);
                let down = f64::from(height) / (h_pt / 72.0);
                across.min(down)
            })
            .fold(f64::INFINITY, f64::min);
        report.effective_ppi = Some(ppi);
        if !ppi.is_finite() {
            plan.push(skip(report, SkipReason::NotMeasurable));
            continue;
        }
        if image_format(doc, &stream.dict).is_none() {
            plan.push(skip(report, SkipReason::UnsupportedFormat));
            continue;
        }
        if ppi <= options.target_ppi * options.resample_threshold {
            plan.push(skip(report, SkipReason::BelowTarget));
            continue;
        }
        plan.push(PlannedImage {
            id: *id,
            report,
            scale: Some(options.target_ppi / ppi),
        });
    }
    plan
}

fn dict_u32(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<u32> {
    dict.get(key)
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_i64().ok())
        .and_then(|n| u32::try_from(n).ok())
}

/// Components per pixel for the formats this module resamples (8-bit `DeviceGray` / `DeviceRGB`,
/// no `/Decode`, `/Mask`, `/ImageMask`), else `None`.
fn image_format(doc: &Document, dict: &Dictionary) -> Option<usize> {
    if dict.has(b"Decode") || dict.has(b"Mask") {
        return None;
    }
    if dict
        .get(b"ImageMask")
        .ok()
        .and_then(|o| o.as_bool().ok())
        .unwrap_or(false)
    {
        return None;
    }
    if dict_u32(doc, dict, b"BitsPerComponent")? != 8 {
        return None;
    }
    let components = match deref_obj(doc, dict.get(b"ColorSpace").ok()?)?
        .as_name()
        .ok()?
    {
        b"DeviceGray" => 1,
        b"DeviceRGB" => 3,
        _ => return None,
    };
    // No filter, or filters this crate's decoder undoes (the stream is then re-encoded as Flate).
    match dict.get(b"Filter") {
        Err(_) => {}
        Ok(filter) => {
            let names: Vec<&[u8]> = match deref_obj(doc, filter)? {
                Object::Name(n) => vec![n.as_slice()],
                Object::Array(items) => items
                    .iter()
                    .map(|i| deref_obj(doc, i).and_then(|o| o.as_name().ok()))
                    .collect::<Option<Vec<_>>>()?,
                _ => return None,
            };
            let supported = |n: &[u8]| {
                matches!(
                    n,
                    b"FlateDecode" | b"Fl" | b"LZWDecode" | b"LZW" | b"ASCII85Decode" | b"A85"
                )
            };
            if !names.iter().all(|n| supported(n)) {
                return None;
            }
        }
    }
    Some(components)
}

/// Area-average weights mapping `source` samples onto `target`: for each target sample the first
/// source index and the weights of the run starting there.
fn axis_weights(source: usize, target: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = source as f64 / target as f64;
    (0..target)
        .map(|i| {
            let start = i as f64 * ratio;
            let end = ((i + 1) as f64 * ratio).min(source as f64);
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(source);
            let mut weights = Vec::with_capacity(last.saturating_sub(first));
            let mut total = 0.0f64;
            for j in first..last {
                let overlap = (j as f64 + 1.0).min(end) - (j as f64).max(start);
                let overlap = overlap.max(0.0);
                weights.push(overlap as f32);
                total += overlap;
            }
            if total > 0.0 {
                for w in &mut weights {
                    *w = (f64::from(*w) / total) as f32;
                }
            } else if weights.is_empty() {
                weights.push(1.0);
            }
            (first.min(source - 1), weights)
        })
        .collect()
}

/// Box-filters `source` (`width` x `height`, `components` interleaved 8-bit samples) to
/// `new_width` x `new_height`.
fn downsample(
    source: &[u8],
    width: usize,
    height: usize,
    components: usize,
    new_width: usize,
    new_height: usize,
) -> Vec<u8> {
    let columns = axis_weights(width, new_width);
    let rows = axis_weights(height, new_height);
    // Horizontal pass: height rows of new_width pixels.
    let mut middle = vec![0.0f32; new_width * height * components];
    for y in 0..height {
        let row = &source[y * width * components..(y + 1) * width * components];
        for (x, (first, weights)) in columns.iter().enumerate() {
            for c in 0..components {
                let mut sum = 0.0f32;
                for (k, w) in weights.iter().enumerate() {
                    sum += *w * f32::from(row[(first + k) * components + c]);
                }
                middle[(y * new_width + x) * components + c] = sum;
            }
        }
    }
    // Vertical pass.
    let mut out = vec![0u8; new_width * new_height * components];
    for (y, (first, weights)) in rows.iter().enumerate() {
        for x in 0..new_width {
            for c in 0..components {
                let mut sum = 0.0f32;
                for (k, w) in weights.iter().enumerate() {
                    sum += *w * middle[((first + k) * new_width + x) * components + c];
                }
                out[(y * new_width + x) * components + c] = sum.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Resamples image `id` to `scale` of its size. `Err` leaves the image untouched.
fn resample(
    doc: &mut Document,
    id: ObjectId,
    scale: f64,
    options: &OptimizeOptions,
) -> Result<(u32, u32), SkipReason> {
    let (width, height, components, data, stored_len) = {
        let stream = doc
            .get_object(id)
            .ok()
            .and_then(|o| o.as_stream().ok())
            .ok_or(SkipReason::BadData)?;
        let components = image_format(doc, &stream.dict).ok_or(SkipReason::UnsupportedFormat)?;
        let width = dict_u32(doc, &stream.dict, b"Width").ok_or(SkipReason::BadData)? as usize;
        let height = dict_u32(doc, &stream.dict, b"Height").ok_or(SkipReason::BadData)? as usize;
        let expected = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(components))
            .ok_or(SkipReason::BadData)?;
        if expected == 0 || expected > options.max_image_bytes {
            return Err(SkipReason::BadData);
        }
        let data = stream
            .get_plain_content_with_limit(options.max_image_bytes)
            .map_err(|_| SkipReason::BadData)?;
        if data.len() != expected {
            return Err(SkipReason::BadData);
        }
        (width, height, components, data, stream.content.len())
    };
    let new_width = ((width as f64 * scale).round() as usize).clamp(1, width);
    let new_height = ((height as f64 * scale).round() as usize).clamp(1, height);
    if new_width >= width && new_height >= height {
        return Err(SkipReason::BelowTarget);
    }
    let pixels = downsample(&data, width, height, components, new_width, new_height);
    let mut replacement = Stream::new(Dictionary::new(), pixels);
    replacement.compress().map_err(|_| SkipReason::BadData)?;
    if replacement.content.len() >= stored_len {
        return Err(SkipReason::NotSmaller);
    }
    match doc.get_object_mut(id) {
        Ok(Object::Stream(stream)) => {
            // Only the keys that describe the sample data change; everything else (`/SMask`,
            // `/Interpolate`, `/Intent`, `/Metadata`, ...) stays.
            stream.dict.remove(b"DecodeParms");
            stream.dict.remove(b"DP");
            stream.dict.remove(b"F");
            stream.dict.set("Width", Object::Integer(new_width as i64));
            stream
                .dict
                .set("Height", Object::Integer(new_height as i64));
            match replacement.dict.get(b"Filter") {
                Ok(filter) => stream.dict.set("Filter", filter.clone()),
                Err(_) => {
                    stream.dict.remove(b"Filter");
                }
            }
            stream.set_content(replacement.content);
            Ok((new_width as u32, new_height as u32))
        }
        _ => Err(SkipReason::BadData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn gray_image(doc: &mut Document, side: usize) -> ObjectId {
        let data: Vec<u8> = (0..side * side).map(|i| ((i * 7) % 256) as u8).collect();
        doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => side as i64,
                "Height" => side as i64, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            data,
        ))
    }

    /// A page drawing `/Im0` at `points` x `points`, plus extras the caller wires in.
    fn page_with_image(
        side: usize,
        points: f64,
        extra: impl FnOnce(&mut Document, ObjectId),
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let image = gray_image(&mut doc, side);
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            format!("q {points} 0 0 {points} 50 50 cm /Im0 Do Q").into_bytes(),
        ));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => Object::Array(vec![Object::Reference(page)]), "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog));
        extra(&mut doc, image);
        // Unused object the clean-up must drop.
        doc.add_object(Stream::new(Dictionary::new(), vec![1u8; 5000]));
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn image_dims(bytes: &[u8]) -> (i64, i64) {
        let doc = load_pdf(bytes).unwrap();
        let stream = doc
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .find(|s| is_image_stream(s))
            .unwrap();
        (
            stream.dict.get(b"Width").unwrap().as_i64().unwrap(),
            stream.dict.get(b"Height").unwrap().as_i64().unwrap(),
        )
    }

    #[test]
    fn a_measured_high_resolution_image_is_resampled_to_the_target() {
        // 400 px over 100 pt is 288 ppi; the 150 ppi target keeps 150/288 of each side.
        let source = page_with_image(400, 100.0, |_, _| {});
        let outcome = optimize(&source, &OptimizeOptions::standard()).unwrap();
        assert_eq!(outcome.report.images_resampled, 1);
        assert_eq!(image_dims(&outcome.bytes), (208, 208));
        assert!(outcome.report.bytes_after < outcome.report.bytes_before);
        assert!(outcome.report.objects_removed >= 1);
        let ppi = outcome.report.images[0].effective_ppi.unwrap();
        assert!((ppi - 288.0).abs() < 0.01);
    }

    #[test]
    fn an_image_already_near_the_target_is_left_alone() {
        let source = page_with_image(200, 100.0, |_, _| {});
        let outcome = optimize(&source, &OptimizeOptions::standard()).unwrap();
        assert_eq!(outcome.report.images_resampled, 0);
        assert_eq!(
            outcome.report.images[0].skipped,
            Some(SkipReason::BelowTarget)
        );
        assert_eq!(image_dims(&outcome.bytes), (200, 200));
    }

    #[test]
    fn an_image_also_used_as_a_mask_is_not_resampled() {
        // Another object points at the image outside an /XObject dictionary: its use is unknown.
        let source = page_with_image(400, 100.0, |doc, image| {
            let other = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 4, "Height" => 4,
                    "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "SMask" => image,
                },
                vec![0u8; 16],
            ));
            let keep = doc.add_object(dictionary! { "Keep" => other });
            let catalog = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
            if let Ok(Object::Dictionary(c)) = doc.get_object_mut(catalog) {
                c.set("Keep", Object::Reference(keep));
            }
        });
        let outcome = optimize(&source, &OptimizeOptions::standard()).unwrap();
        let big = outcome
            .report
            .images
            .iter()
            .find(|i| i.width == 400)
            .unwrap();
        assert_eq!(big.skipped, Some(SkipReason::NotMeasurable));
        assert_eq!(outcome.report.images_resampled, 0);
    }

    #[test]
    fn an_image_listed_by_a_form_no_page_paints_is_not_resampled() {
        let source = page_with_image(400, 100.0, |doc, image| {
            // A form that only an annotation appearance reaches, with its own resources.
            let form = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0), Object::Integer(10), Object::Integer(10)]),
                    "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
                },
                b"/Im0 Do".to_vec(),
            ));
            let annot = doc.add_object(dictionary! {
                "Type" => "Annot", "Subtype" => "Stamp",
                "Rect" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0), Object::Integer(10), Object::Integer(10)]),
                "AP" => dictionary! { "N" => form },
            });
            let page = doc.get_pages()[&1];
            if let Ok(Object::Dictionary(p)) = doc.get_object_mut(page) {
                p.set("Annots", Object::Array(vec![Object::Reference(annot)]));
            }
        });
        let outcome = optimize(&source, &OptimizeOptions::standard()).unwrap();
        assert_eq!(outcome.report.images_resampled, 0);
        assert_eq!(
            outcome.report.images[0].skipped,
            Some(SkipReason::NotMeasurable)
        );
    }

    #[test]
    fn an_image_painted_through_a_form_is_measured_through_its_matrix() {
        let source = page_with_image(400, 100.0, |doc, image| {
            // The page now paints the image only through a form scaled 2x: 200 pt, 144 ppi.
            let form = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0), Object::Integer(1), Object::Integer(1)]),
                    "Matrix" => Object::Array(vec![
                        Object::Integer(2), Object::Integer(0), Object::Integer(0), Object::Integer(2),
                        Object::Integer(0), Object::Integer(0)]),
                    "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
                },
                b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec(),
            ));
            let page = doc.get_pages()[&1];
            let contents = match doc
                .get_object(page)
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Contents")
                .unwrap()
            {
                Object::Reference(id) => *id,
                _ => unreachable!(),
            };
            if let Ok(Object::Stream(s)) = doc.get_object_mut(contents) {
                s.set_content(b"/Fm0 Do".to_vec());
            }
            if let Ok(Object::Dictionary(p)) = doc.get_object_mut(page) {
                p.set(
                    "Resources",
                    dictionary! { "XObject" => dictionary! { "Fm0" => form } },
                );
            }
        });
        let outcome = optimize(&source, &OptimizeOptions::standard()).unwrap();
        // 400 px over 200 pt is 144 ppi, within the threshold of 150: measured, and left alone.
        let ppi = outcome.report.images[0].effective_ppi.unwrap();
        assert!((ppi - 144.0).abs() < 0.01, "{ppi}");
        assert_eq!(
            outcome.report.images[0].skipped,
            Some(SkipReason::BelowTarget)
        );
    }

    #[test]
    fn invalid_options_and_cancel_are_typed() {
        let source = page_with_image(400, 100.0, |_, _| {});
        let bad = OptimizeOptions {
            target_ppi: 10.0,
            ..OptimizeOptions::standard()
        };
        assert!(matches!(
            optimize(&source, &bad),
            Err(OptimizeError::InvalidOptions(_))
        ));
        let cancelled = optimize_with_progress(&source, &OptimizeOptions::standard(), &mut |p| {
            p.stage != Stage::Resampling
        });
        assert!(matches!(cancelled, Err(OptimizeError::Cancelled)));
    }

    #[test]
    fn downsample_averages_areas() {
        // 4x1 to 2x1: pairs average.
        let out = downsample(&[0, 100, 200, 250], 4, 1, 1, 2, 1);
        assert_eq!(out, [50, 225]);
        // Non-integer ratio keeps the mean of a flat image.
        let flat = vec![77u8; 7 * 5 * 3];
        assert!(downsample(&flat, 7, 5, 3, 3, 2).iter().all(|v| *v == 77));
    }
}
