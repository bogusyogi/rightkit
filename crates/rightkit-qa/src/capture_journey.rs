//! The bookkeeping of a native-overlay visual journey: settle polling, stills with PNG checks, frame
//! sequences with manifest and validity checks, a duplicate-capture check and the index file.
//!
//! It builds on `crate::desktop_capture` (window discovery, grabbing, PNG statistics) and records
//! problems instead of stopping at the first one, so a run reports every capture that failed. The
//! journey (which states, which triggers, which sizes it expects) stays with the host; this module is
//! the machinery.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::desktop_capture::{self as capture, Grabber, Obs, Rect, Sig};
use crate::harness::Scenario;
use crate::live_app::wait_until;
use crate::page_audit::os_name;

/// Wait until `accept` holds for `streak` consecutive polls with an unchanged signature, and (with
/// `from`) the signature differs from it. `observe` reads the overlay windows now. Panics at
/// `timeout`, naming the last observation.
pub fn settle(
    what: &str,
    from: Option<&Sig>,
    accept: &dyn Fn(&[Obs]) -> bool,
    streak: u32,
    timeout: Duration,
    observe: &mut dyn FnMut() -> Result<Vec<Obs>, String>,
) -> Vec<Obs> {
    let mut last: Option<Sig> = None;
    let mut run = 0u32;
    wait_until(what, timeout, || {
        let obs = observe()?;
        let now = capture::sig(&obs);
        let usable = accept(&obs) && from.is_none_or(|f| &now != f);
        if usable && last.as_ref() == Some(&now) {
            run += 1;
        } else {
            run = u32::from(usable);
        }
        if usable && run >= streak {
            return Ok(Some(obs));
        }
        let seen = format!("observed {now:?}, need change from {from:?}");
        last = if usable { Some(now) } else { None };
        Err(seen)
    })
}

/// Where to park the pointer so it is away from an overlay `pill` (its settled idle window): Windows
/// the top-left corner, macOS well above the overlay.
pub fn park_point(pill: Rect) -> (f64, f64) {
    if cfg!(windows) {
        (20.0, 20.0)
    } else {
        let (cx, _) = pill.center();
        (cx, (pill.y - 400.0).max(80.0))
    }
}

/// A native-overlay capture journey in progress.
pub struct CaptureJourney<'a> {
    /// The scenario whose evidence folder receives the captures.
    pub sc: &'a Scenario,
    /// The evidence folder.
    pub root: PathBuf,
    /// Windows: the fixed capture region and the app's scale (`capture::fixed_region`).
    pub fixed: Option<(Rect, f64)>,
    /// The overlay windows as last observed.
    pub obs: Vec<Obs>,
    /// One row per still.
    pub stills: Vec<Value>,
    /// One brief manifest per sequence.
    pub sequences: Vec<Value>,
    /// File hash per first-window still, for the duplicate check.
    pub hashes: Vec<(String, u64)>,
    /// Every problem found so far.
    pub problems: Vec<String>,
    /// The previous still's pixel size (macOS: two states must not share it).
    pub last_dims: Option<(u32, u32)>,
    /// Frames per second of sequences.
    pub fps: u32,
}

impl<'a> CaptureJourney<'a> {
    /// A journey writing into `sc`'s evidence folder, capturing sequences at `fps`.
    pub fn new(sc: &'a Scenario, fps: u32) -> Self {
        CaptureJourney {
            sc,
            root: sc.dir().to_path_buf(),
            fixed: None,
            obs: Vec::new(),
            stills: Vec::new(),
            sequences: Vec::new(),
            hashes: Vec::new(),
            problems: Vec::new(),
            last_dims: None,
            fps,
        }
    }

    /// Record and print a problem.
    pub fn problem(&mut self, text: String) {
        eprintln!("PROBLEM {text}");
        self.problems.push(text);
    }

    /// One still of `obs` (the windows as observed). `expected` is the size in points the source gives
    /// for the state (recorded, never asserted). A capture that fails, does not decode, is blank, or
    /// (macOS) has the same pixel size as the previous state's capture is a problem.
    pub fn still(
        &mut self,
        case: &str,
        state: &str,
        obs: &[Obs],
        expected: Option<(f64, f64)>,
        min_distinct: usize,
    ) {
        let os = os_name();
        let name = format!("{os}-{case}-{state}");
        let path = self.root.join(format!("{name}.png"));
        let grabber = Grabber::new(obs, self.fixed);
        let at = capture::now_unix_ms();
        let files = match grabber.still(&path) {
            Ok(files) => files,
            Err(e) => {
                self.problem(format!("{case} {state}: {e}"));
                return;
            }
        };
        for (k, file) in files.iter().enumerate() {
            let bytes = std::fs::read(file).unwrap_or_default();
            let hash = capture::fnv64(&bytes);
            let label = if k == 0 {
                name.clone()
            } else {
                format!("{name}-w{k}")
            };
            let rect = obs.get(k).map(|o| o.rect).unwrap_or(grabber.region);
            let mut row = json!({
                "case": format!("{os}/{case}"),
                "state": state,
                "file": file.file_name().map(|n| n.to_string_lossy().into_owned()),
                "timestamp_unix_ms": at,
                "rect_points": rect.to_json(),
                "scale_reported_by_app": obs.get(k).map(|o| o.scale),
                "hash": format!("{hash:016x}"),
                "expected_size_points": expected.map(|(w, h)| json!([w, h])),
            });
            match capture::analyse_png(&bytes) {
                Ok(stats) => {
                    let measured = if cfg!(windows) {
                        grabber.scale
                    } else if rect.w > 0.0 {
                        f64::from(stats.width) / rect.w
                    } else {
                        0.0
                    };
                    row["scale_factor"] = json!(measured);
                    row["png"] = stats.to_json();
                    if cfg!(windows) {
                        row["region_points"] = grabber.region.to_json();
                    }
                    if let Some((ew, eh)) = expected {
                        if measured > 0.0 {
                            row["matches_expected_size"] = json!(
                                (f64::from(stats.width) / measured - ew).abs() <= 4.0
                                    && (f64::from(stats.height) / measured - eh).abs() <= 4.0
                            );
                        }
                    }
                    if stats.is_blank(min_distinct) {
                        self.problem(format!(
                            "{case} {state} {label}: blank capture ({} colours, {:.1}% visible)",
                            stats.distinct,
                            stats.visible_fraction * 100.0
                        ));
                    }
                    if k == 0 {
                        let dims = (stats.width, stats.height);
                        if cfg!(target_os = "macos") && self.last_dims == Some(dims) {
                            self.problem(format!(
                                "{case} {state}: same pixel size {}x{} as the previous state's capture, the overlay did not change",
                                dims.0, dims.1
                            ));
                        }
                        self.last_dims = Some(dims);
                    }
                }
                Err(e) => self.problem(format!("{case} {state} {label}: PNG unreadable: {e}")),
            }
            if k == 0 {
                self.hashes.push((format!("{case}-{state}"), hash));
            }
            self.sc.keep(&label, file);
            self.stills.push(row);
        }
    }

    /// One transition: a frame sequence from `pre_ms` before `trigger` to `post_ms` after it, a manifest
    /// beside the frames, and the checks that frames were taken, decode, are not blank and show a change.
    pub fn transition(
        &mut self,
        id: &str,
        title: &str,
        pre_ms: u64,
        post_ms: u64,
        trigger: impl FnOnce() -> Result<(), String>,
    ) {
        let os = os_name();
        let fps = self.fps;
        let name = format!("{id}-{title}");
        let dir = self.root.join(format!("{os}-{name}"));
        let grabber = Grabber::new(&self.obs, self.fixed);
        let result = match capture::sequence(&grabber, &dir, pre_ms, post_ms, fps, trigger) {
            Ok(result) => result,
            Err(e) => {
                self.problem(format!("{name}: sequence failed: {e}"));
                return;
            }
        };
        let summary = capture::summarise(&name, &dir, &result, fps, pre_ms, post_ms);
        let mut manifest = summary.manifest.clone();
        manifest["os"] = json!(os);
        manifest["grabber"] = json!({
            "windows": grabber.windows,
            "region_points": grabber.region.to_json(),
            "scale": grabber.scale,
        });
        manifest["window_rects_before_trigger_points"] =
            Value::Array(self.obs.iter().map(|o| o.rect.to_json()).collect());
        let manifest_path = dir.join("manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest json"),
        )
        .expect("write sequence manifest");
        self.sc
            .keep(&format!("{os}-{name}-manifest"), &manifest_path);
        for file in &summary.files {
            let stem = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.sc.keep(&format!("{os}-{name}-{stem}"), file);
        }
        let expected = ((pre_ms + post_ms) * u64::from(fps) / 1000) as usize;
        let mut brief = manifest.clone();
        if let Some(obj) = brief.as_object_mut() {
            obj.remove("frames");
        }
        self.sequences.push(brief);
        self.sc.note(format!(
            "{name}: {}/{} frames valid, {} distinct, {:.1} fps achieved (target {fps}), trigger returned after {} ms",
            summary.valid_frames, expected, summary.distinct_frames, summary.achieved_fps, result.trigger_returned_ms
        ));
        if let Some(e) = &result.trigger_error {
            self.problem(format!("{name}: trigger failed: {e}"));
        }
        if summary.valid_frames == 0 || summary.valid_frames * 10 < expected * 4 {
            self.problem(format!(
                "{name}: only {} of {expected} expected frames captured",
                summary.valid_frames
            ));
        }
        if summary.blank_frames * 5 > summary.valid_frames + summary.blank_frames {
            self.problem(format!(
                "{name}: {} blank frames of {}",
                summary.blank_frames,
                summary.valid_frames + summary.blank_frames
            ));
        }
        if summary.distinct_frames < 2 {
            self.problem(format!(
                "{name}: every frame is identical, nothing animated or changed"
            ));
        }
    }

    /// Windows: every capture is the same region, so a region that missed the overlay would give
    /// identical files for different states. Records a problem for each byte-identical pair; does
    /// nothing elsewhere.
    pub fn check_distinct_hashes(&mut self) {
        if !cfg!(windows) {
            return;
        }
        let hashes = self.hashes.clone();
        for (i, (a, ha)) in hashes.iter().enumerate() {
            for (b, hb) in hashes.iter().skip(i + 1) {
                if ha == hb {
                    self.problem(format!(
                        "captures {a} and {b} are byte-identical: the region does not show the overlay"
                    ));
                }
            }
        }
    }

    /// Write `<os>-<stem>-index.json` (`os`, `target_fps`, `stills`, `sequences`, `problems`, plus every
    /// key of the object `extra`), keep it in the evidence, and panic listing the problems when there
    /// are any. Returns the index path.
    pub fn finish(&self, stem: &str, scenario: &str, extra: Value) -> PathBuf {
        let os = os_name();
        let mut index = json!({
            "os": os,
            "scenario": scenario,
            "target_fps": self.fps,
            "stills": self.stills,
            "sequences": self.sequences,
            "problems": self.problems,
        });
        if let (Some(base), Some(more)) = (index.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                base.insert(k.clone(), v.clone());
            }
        }
        let index_path = self.root.join(format!("{os}-{stem}-index.json"));
        std::fs::write(
            &index_path,
            serde_json::to_string_pretty(&index).expect("index json"),
        )
        .expect("write capture index");
        self.sc.keep(&format!("{os}-{stem}-index"), &index_path);
        assert!(
            self.problems.is_empty(),
            "capture problems:\n{}",
            self.problems.join("\n")
        );
        index_path
    }
}
