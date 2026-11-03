//! Social packaging: a final render + captions + thumbnail + metadata per
//! platform, in a folder (and optionally a zip).
//!
//! Locked invariant: packaging requires receipt verification. A render is
//! packaged only when the project holds a QA receipt for its exact BLAKE3, the
//! receipt and report verify, and the report passed. Otherwise a typed error:
//! [`QaError::QaMissing`], [`QaError::ReceiptInvalid`] or [`QaError::QaFailed`].
//! The finished package carries copies of the report and receipt and its own
//! manifest; [`verify_package`] re-checks all of it without the project.

use std::fs;
use std::path::{Path, PathBuf};

use rightkit_ffmpeg::FfmpegCommand;
use rightkit_video_project::worker::{spawn_ffmpeg, wait_with_output, StdioMode, StdioPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::render_qa::error::QaError;
use crate::render_qa::fsutil::atomic_write;
use crate::render_qa::probe::QaTools;
use crate::render_qa::receipt::{
    judged_under_current_policy, policy_drift, record_report, verify_receipt, verify_receipt_files,
};
use crate::render_qa::report::QaReport;
use crate::render_qa::run::run_qa;
use crate::render_qa::spec::{QaExpect, QaSpec};
use rightkit_fs::digest::blake3_file;
use rightkit_fs::zip::write_zip;

const PLATFORMS_JSON: &str = include_str!("../../data/render_qa/platforms.json");
pub const MANIFEST_KIND: &str = "cutright.social-package";
pub const MANIFEST_FILE: &str = "package-manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    // One word each, as the package folders are named. The two-word spellings an earlier
    // build wrote into manifests are still read.
    #[serde(rename = "youtube", alias = "you_tube")]
    YouTube,
    Shorts,
    Reels,
    #[serde(rename = "tiktok", alias = "tik_tok")]
    TikTok,
}

impl Platform {
    pub const ALL: [Platform; 4] = [
        Platform::YouTube,
        Platform::Shorts,
        Platform::Reels,
        Platform::TikTok,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Platform::YouTube => "youtube",
            Platform::Shorts => "shorts",
            Platform::Reels => "reels",
            Platform::TikTok => "tiktok",
        }
    }

    pub fn parse(id: &str) -> Result<Self, QaError> {
        Platform::ALL
            .into_iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| QaError::UnknownPreset(id.into()))
    }

    pub fn rules(self) -> PlatformRules {
        let doc: Value = serde_json::from_str(PLATFORMS_JSON).expect("embedded platforms parse");
        serde_json::from_value(doc[self.id()].clone()).expect("embedded platform rules parse")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThumbnailRules {
    pub width_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformRules {
    pub name: String,
    pub aspects: Vec<(u32, u32)>,
    pub min_width: u32,
    pub max_duration_ms: i64,
    pub max_title_chars: usize,
    pub max_description_chars: usize,
    pub max_tags_total_chars: usize,
    pub thumbnail: ThumbnailRules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialMetadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PackageRequest {
    pub project_dir: PathBuf,
    pub render: PathBuf,
    pub platform: Platform,
    pub metadata: SocialMetadata,
    /// The SRT shipped with the render; must be the SRT QA checked.
    pub captions_srt: Option<String>,
    /// Package parent directory; default `<project>/exports`.
    pub out_dir: Option<PathBuf>,
    /// Thumbnail frame time; default 1 s (or the middle of a shorter render).
    pub thumbnail_ms: Option<i64>,
    /// Also write `<out>/<platform>.zip`.
    pub zip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    /// Path relative to the package folder, `/`-separated.
    pub path: String,
    pub role: String,
    pub blake3: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub kind: String,
    pub platform: Platform,
    pub created_at: String,
    pub render_blake3: String,
    pub qa_report: String,
    pub qa_report_blake3: String,
    pub qa_receipt: String,
    pub qa_receipt_blake3: String,
    pub qa_status: String,
    pub captions_blake3: Option<String>,
    pub files: Vec<PackageFile>,
}

#[derive(Debug, Clone)]
pub struct PackageOutcome {
    pub dir: PathBuf,
    pub zip: Option<PathBuf>,
    pub manifest: PackageManifest,
    /// True only after the finished package re-verified (receipt included).
    pub approved: bool,
}

fn within_pct(a: u64, b: u64) -> bool {
    let (a, b) = (a as f64, b as f64);
    (a - b).abs() <= 0.01 * a.max(b)
}

fn check_platform(rules: &PlatformRules, platform: Platform, r: &QaReport) -> Result<(), QaError> {
    let mismatch = |reason: String| QaError::PlatformMismatch {
        platform: platform.id().into(),
        reason,
    };
    let (Some(w), Some(h), Some(dur)) = (r.media.width, r.media.height, r.media.duration_ms) else {
        return Err(mismatch(
            "QA report carries no video size or duration".into(),
        ));
    };
    let fits_aspect = rules
        .aspects
        .iter()
        .any(|&(aw, ah)| within_pct(w as u64 * ah as u64, h as u64 * aw as u64));
    if !fits_aspect {
        return Err(mismatch(format!(
            "{w}x{h} is not one of the {} aspects {:?}",
            rules.name, rules.aspects
        )));
    }
    if w < rules.min_width {
        return Err(mismatch(format!(
            "width {w} is under the {} minimum {}",
            rules.name, rules.min_width
        )));
    }
    if dur > rules.max_duration_ms {
        return Err(mismatch(format!(
            "{dur} ms exceeds the {} limit of {} ms",
            rules.name, rules.max_duration_ms
        )));
    }
    Ok(())
}

fn check_metadata(
    rules: &PlatformRules,
    platform: Platform,
    m: &SocialMetadata,
) -> Result<(), QaError> {
    let bad = |reason: String| QaError::MetadataInvalid {
        platform: platform.id().into(),
        reason,
    };
    if m.title.trim().is_empty() {
        return Err(bad("title is empty".into()));
    }
    if m.title.chars().count() > rules.max_title_chars {
        return Err(bad(format!(
            "title is over {} characters",
            rules.max_title_chars
        )));
    }
    if m.description.chars().count() > rules.max_description_chars {
        return Err(bad(format!(
            "description is over {} characters",
            rules.max_description_chars
        )));
    }
    if m.tags.iter().any(|t| t.trim().is_empty()) {
        return Err(bad("a tag is empty".into()));
    }
    let total: usize = m.tags.iter().map(|t| t.chars().count()).sum();
    if total > rules.max_tags_total_chars {
        return Err(bad(format!(
            "tags are over {} characters in total",
            rules.max_tags_total_chars
        )));
    }
    Ok(())
}

fn extract_thumbnail(
    tools: &QaTools,
    video: &Path,
    at_ms: i64,
    width_max: u32,
    dest: &Path,
) -> Result<(), QaError> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    rightkit_fs::sweep_stale_temps(dir, std::time::Duration::ZERO)
        .map_err(|e| QaError::io("sweeping thumbnail temps", e.into()))?;
    let output = rightkit_fs::AtomicWriter::create(dest, rightkit_fs::AtomicOptions::default())
        .map_err(|e| QaError::io("staging thumbnail", e.into()))?;
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostdin", "-v", "error", "-y", "-ss"])
        .text(format!("{:.3}", at_ms as f64 / 1000.0))
        .input(video)
        .texts(["-frames:v", "1", "-an", "-vf"])
        .text(format!("scale='min({width_max},iw)':-2"))
        .texts(["-q:v", "2", "-c:v", "mjpeg", "-f", "image2", "-update", "1"])
        .path(output.temp_path());
    let out = spawn_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|e| QaError::io("starting ffmpeg", e))?;
    let size = fs::metadata(output.temp_path())
        .map(|m| m.len())
        .unwrap_or(0);
    if !out.status.success() || size == 0 {
        return Err(QaError::ToolFailed {
            tool: "ffmpeg",
            message: format!(
                "thumbnail extraction failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    output
        .commit()
        .map_err(|e| QaError::io("publishing thumbnail", e.into()))
}

fn describe(dir: &Path, rel: &str, role: &str) -> Result<PackageFile, QaError> {
    let p = dir.join(rel);
    Ok(PackageFile {
        path: rel.into(),
        role: role.into(),
        blake3: blake3_file(&p).map_err(|e| QaError::io(format!("hashing {}", p.display()), e))?,
        size_bytes: fs::metadata(&p)
            .map_err(|e| QaError::io(format!("reading {}", p.display()), e))?
            .len(),
    })
}

fn write_json(path: &Path, v: &impl Serialize) -> Result<(), QaError> {
    let mut t = serde_json::to_string_pretty(v)
        .map_err(|e| QaError::BadInput(format!("serialising {}: {e}", path.display())))?;
    t.push('\n');
    atomic_write(path, t.as_bytes())
}

/// Builds the package for one platform. Refuses unless the render's QA verified and passed.
pub fn package_render(tools: &QaTools, req: &PackageRequest) -> Result<PackageOutcome, QaError> {
    // 1. The gate: receipt for these exact bytes, verified, passing.
    let render_hash = blake3_file(&req.render)
        .map_err(|e| QaError::io(format!("hashing {}", req.render.display()), e))?;
    let verified = verify_receipt(&req.project_dir, &render_hash)?;
    if !verified.report.passed() {
        return Err(QaError::QaFailed {
            render_blake3: render_hash,
            failing: verified.report.blocking_ids(),
        });
    }
    let report = &verified.report;

    // 2. Captions are the ones QA checked.
    let packaged_hash = req
        .captions_srt
        .as_ref()
        .map(|s| rightkit_fs::digest::blake3_bytes(s.as_bytes()));
    if packaged_hash != report.captions_blake3 {
        return Err(QaError::CaptionsMismatch {
            packaged: packaged_hash.unwrap_or_else(|| "none".into()),
            checked: report
                .captions_blake3
                .clone()
                .unwrap_or_else(|| "none".into()),
        });
    }

    // 3. The platform and its metadata limits.
    let rules = req.platform.rules();
    check_platform(&rules, req.platform, report)?;
    check_metadata(&rules, req.platform, &req.metadata)?;

    // 4. Build the folder.
    let out_root = req
        .out_dir
        .clone()
        .unwrap_or_else(|| req.project_dir.join("exports"));
    let final_dir = out_root.join(req.platform.id());
    fs::create_dir_all(&out_root)
        .map_err(|e| QaError::io(format!("creating {}", out_root.display()), e))?;
    // Same-volume staging leaves any existing approved package intact on failure.
    let staging = tempfile::Builder::new()
        .prefix(".rightkit-package-")
        .tempdir_in(&out_root)
        .map_err(|e| QaError::io("creating package staging directory", e))?;
    let dir = staging.path().join("package");
    fs::create_dir_all(dir.join("qa"))
        .map_err(|e| QaError::io(format!("creating {}", dir.display()), e))?;

    let ext = req
        .render
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "mp4".into());
    let video_rel = format!("video.{ext}");
    rightkit_fs::copy_verified_with_digest(
        &req.render,
        &dir.join(&video_rel),
        rightkit_fs::AtomicWriteOptions::default(),
        rightkit_fs::DigestAlgorithm::Blake3,
        Some(&render_hash),
        |_, _| {},
    )
    .map_err(|e| match e {
        rightkit_fs::FsError::DigestMismatch { .. } => {
            QaError::PackageInvalid("the render changed while it was being packaged".into())
        }
        e => QaError::io("copying the render", std::io::Error::other(e)),
    })?;
    let mut files = vec![describe(&dir, &video_rel, "video")?];

    if let Some(srt) = &req.captions_srt {
        atomic_write(&dir.join("captions.srt"), srt.as_bytes())?;
        files.push(describe(&dir, "captions.srt", "captions")?);
    }

    let dur = report.media.duration_ms.unwrap_or(0);
    let at = req
        .thumbnail_ms
        .unwrap_or_else(|| 1000.min(dur / 2))
        .clamp(0, dur.max(0));
    extract_thumbnail(
        tools,
        &dir.join(&video_rel),
        at,
        rules.thumbnail.width_max,
        &dir.join("thumbnail.jpg"),
    )?;
    files.push(describe(&dir, "thumbnail.jpg", "thumbnail")?);

    let meta = json!({
        "platform": req.platform.id(),
        "platform_name": rules.name,
        "title": req.metadata.title,
        "description": req.metadata.description,
        "tags": req.metadata.tags,
        "video": {
            "file": video_rel,
            "width": report.media.width,
            "height": report.media.height,
            "duration_ms": report.media.duration_ms,
            "fps": report.media.fps,
            "video_codec": report.media.video_codec,
            "audio_codec": report.media.audio_codec,
        },
        "captions_file": req.captions_srt.as_ref().map(|_| "captions.srt"),
        "thumbnail_file": "thumbnail.jpg",
        "thumbnail_time_ms": at,
    });
    write_json(&dir.join("metadata.json"), &meta)?;
    files.push(describe(&dir, "metadata.json", "metadata")?);

    // QA evidence travels with the package.
    let report_rel = format!("qa/{render_hash}.report.json");
    let receipt_rel = format!("qa/{render_hash}.receipt.json");
    rightkit_fs::copy_verified_with_digest(
        &crate::render_qa::receipt::report_path(&req.project_dir, &render_hash),
        &dir.join(&report_rel),
        rightkit_fs::AtomicWriteOptions::default(),
        rightkit_fs::DigestAlgorithm::Blake3,
        Some(&verified.report_blake3),
        |_, _| {},
    )
    .map_err(|e| QaError::io("copying the QA report", e.into()))?;
    rightkit_fs::copy_verified_with_digest(
        &crate::render_qa::receipt::receipt_path(&req.project_dir, &render_hash),
        &dir.join(&receipt_rel),
        rightkit_fs::AtomicWriteOptions::default(),
        rightkit_fs::DigestAlgorithm::Blake3,
        Some(&verified.receipt_file_blake3),
        |_, _| {},
    )
    .map_err(|e| QaError::io("copying the QA receipt", e.into()))?;
    files.push(describe(&dir, &report_rel, "qa_report")?);
    files.push(describe(&dir, &receipt_rel, "qa_receipt")?);

    let manifest = PackageManifest {
        schema_version: 1,
        kind: MANIFEST_KIND.into(),
        platform: req.platform,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        render_blake3: render_hash.clone(),
        qa_report: report_rel,
        qa_report_blake3: verified.report_blake3.clone(),
        qa_receipt: receipt_rel,
        qa_receipt_blake3: verified.receipt.receipt_blake3.clone(),
        qa_status: "pass".into(),
        captions_blake3: report.captions_blake3.clone(),
        files,
    };
    write_json(&dir.join(MANIFEST_FILE), &manifest)?;

    // 5. Approval is what verification says about the finished folder.
    let manifest = verify_package(&dir)?;

    let zip = if req.zip {
        let zip_path = out_root.join(format!("{}.zip", req.platform.id()));
        let mut entries: Vec<(String, PathBuf)> = manifest
            .files
            .iter()
            .map(|f| {
                (
                    format!("{}/{}", req.platform.id(), f.path),
                    dir.join(&f.path),
                )
            })
            .collect();
        entries.push((
            format!("{}/{MANIFEST_FILE}", req.platform.id()),
            dir.join(MANIFEST_FILE),
        ));
        let staged_zip = staging.path().join("package.zip");
        write_zip(&staged_zip, &entries).map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidInput => QaError::BadInput(e.to_string()),
            _ => QaError::io(format!("writing {}", zip_path.display()), e),
        })?;
        Some((staged_zip, zip_path))
    } else {
        None
    };
    rightkit_fs::publish_dir(&dir, &final_dir)
        .map_err(|e| QaError::io("publishing package directory", std::io::Error::other(e)))?;
    let zip = zip
        .map(|(src, dest)| {
            rightkit_fs::publish_file(&src, &dest)
                .map_err(|e| QaError::io("publishing package zip", std::io::Error::other(e)))?;
            Ok::<_, QaError>(dest)
        })
        .transpose()?;
    Ok(PackageOutcome {
        dir: final_dir,
        zip,
        manifest,
        approved: true,
    })
}

/// Re-verifies a finished package folder on its own: every listed file hashes to
/// its manifest entry, the packaged video is the QA'd render, and the packaged
/// report and receipt verify against that render and passed. Returns the manifest.
pub fn verify_package(dir: &Path) -> Result<PackageManifest, QaError> {
    verify_package_report(dir).map(|v| v.manifest)
}

/// A verified package and what verification noticed without failing on it.
#[derive(Debug, Clone)]
pub struct PackageVerification {
    pub manifest: PackageManifest,
    /// Policy drift or unknown-policy notes. Never a reason verification failed.
    pub warnings: Vec<String>,
}

/// [`verify_package`] with its warnings. The package is verified against the QA policy its
/// receipt RECORDED (preset id and threshold hash, checked against the packaged report).
/// When that differs from the current policy the drift is a warning only; run
/// [`requalify_package`] to judge the package under the current policy. A receipt that
/// predates policy recording is unknown-policy: it warns and must verify under the
/// current policy.
pub fn verify_package_report(dir: &Path) -> Result<PackageVerification, QaError> {
    verify_package_impl(dir, true)
}

/// `current_for_unknown` is false only for [`requalify_package`], whose whole job is to
/// replace a package that no longer verifies under the current policy.
fn verify_package_impl(
    dir: &Path,
    current_for_unknown: bool,
) -> Result<PackageVerification, QaError> {
    let bad = |m: String| QaError::PackageInvalid(m);
    let text = fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| QaError::io(format!("reading {}", dir.join(MANIFEST_FILE).display()), e))?;
    let manifest: PackageManifest =
        serde_json::from_str(&text).map_err(|e| bad(format!("manifest unreadable: {e}")))?;
    if manifest.kind != MANIFEST_KIND {
        return Err(bad("not a social package manifest".into()));
    }
    for f in &manifest.files {
        if f.path.contains("..") || Path::new(&f.path).is_absolute() {
            return Err(bad(format!("unsafe path {}", f.path)));
        }
        let p = dir.join(&f.path);
        if !p.is_file() {
            return Err(bad(format!("{} is missing", f.path)));
        }
        if blake3_file(&p).map_err(|e| QaError::io(format!("hashing {}", p.display()), e))?
            != f.blake3
        {
            return Err(bad(format!("{} was modified", f.path)));
        }
    }
    let video = manifest
        .files
        .iter()
        .find(|f| f.role == "video")
        .ok_or_else(|| bad("manifest lists no video".into()))?;
    if video.blake3 != manifest.render_blake3 {
        return Err(bad("the video is not the render that was QA'd".into()));
    }
    let verified = verify_receipt_files(
        &dir.join(&manifest.qa_report),
        &dir.join(&manifest.qa_receipt),
        &manifest.render_blake3,
    )?;
    if !verified.report.passed() {
        return Err(QaError::QaFailed {
            render_blake3: manifest.render_blake3.clone(),
            failing: verified.report.blocking_ids(),
        });
    }
    if verified.report_blake3 != manifest.qa_report_blake3
        || verified.receipt.receipt_blake3 != manifest.qa_receipt_blake3
    {
        return Err(bad(
            "manifest does not match the packaged QA evidence".into()
        ));
    }
    let captions = manifest.files.iter().find(|f| f.role == "captions");
    if captions.map(|f| f.blake3.as_str()) != verified.report.captions_blake3.as_deref()
        && captions.is_some()
    {
        // The SRT file is hashed as bytes, like the report's captions hash.
        return Err(bad("packaged captions differ from the QA'd captions".into()));
    }
    if captions.is_none() && verified.report.captions_blake3.is_some() {
        return Err(bad("the QA'd captions are missing from the package".into()));
    }
    let mut warnings = Vec::new();
    match (&verified.receipt.policy_id, &verified.receipt.policy_blake3) {
        (Some(id), Some(hash)) => {
            if let Some(why) = policy_drift(&verified.report) {
                warnings.push(format!(
                    "policy drift: verified against the recorded QA policy {id} (blake3:{hash}), \
                     but {why}; requalify the package to judge it under the current policy"
                ));
            }
        }
        _ => {
            warnings.push(
                "unknown policy: the receipt does not record its QA policy; verified under the \
                 current policy"
                    .into(),
            );
            if current_for_unknown {
                judged_under_current_policy(&verified.report)?;
            }
        }
    }
    Ok(PackageVerification { manifest, warnings })
}

/// Explicit re-qualification: runs QA again on the packaged video under the CURRENT policy
/// for its preset, and only if that passes replaces the package's report and receipt (so
/// they record the current policy) and updates the manifest. The package is verified first.
/// Expectations are rebuilt from the package: the recorded duration and the packaged
/// captions, with no exempt black, freeze or silence windows and no frame-rate expectation.
/// The zip, if any, is not rewritten.
pub fn requalify_package(tools: &QaTools, dir: &Path) -> Result<PackageVerification, QaError> {
    let mut manifest = verify_package_impl(dir, false)?.manifest;
    let old = verify_receipt_files(
        &dir.join(&manifest.qa_report),
        &dir.join(&manifest.qa_receipt),
        &manifest.render_blake3,
    )?;
    let video = manifest
        .files
        .iter()
        .find(|f| f.role == "video")
        .ok_or_else(|| QaError::PackageInvalid("manifest lists no video".into()))?;
    let video_path = dir.join(&video.path);
    let captions_srt = manifest
        .files
        .iter()
        .find(|f| f.role == "captions")
        .map(|f| {
            fs::read_to_string(dir.join(&f.path))
                .map_err(|e| QaError::io(format!("reading {}", f.path), e))
        })
        .transpose()?;
    let spec = QaSpec::preset(&old.report.preset)?;
    let expect = QaExpect {
        duration_ms: old.report.media.duration_ms,
        captions_srt,
        ..QaExpect::default()
    };
    let report = run_qa(tools, &video_path, &spec, &expect)?;
    if report.render.blake3 != manifest.render_blake3 {
        return Err(QaError::PackageInvalid(
            "the packaged video is not the render that was QA'd".into(),
        ));
    }
    if !report.passed() {
        return Err(QaError::QaFailed {
            render_blake3: manifest.render_blake3.clone(),
            failing: report.blocking_ids(),
        });
    }
    // The package's qa/ folder has the project's layout, so the same writer fits.
    let outcome = record_report(dir, report)?;
    for (role, rel) in [
        ("qa_report", manifest.qa_report.clone()),
        ("qa_receipt", manifest.qa_receipt.clone()),
    ] {
        let fresh = describe(dir, &rel, role)?;
        match manifest.files.iter_mut().find(|f| f.role == role) {
            Some(slot) => *slot = fresh,
            None => manifest.files.push(fresh),
        }
    }
    manifest.qa_report_blake3 = blake3_file(&outcome.report_path)
        .map_err(|e| QaError::io(format!("hashing {}", outcome.report_path.display()), e))?;
    manifest.qa_receipt_blake3 = outcome.receipt.receipt_blake3.clone();
    manifest.qa_status = "pass".into();
    write_json(&dir.join(MANIFEST_FILE), &manifest)?;
    verify_package_report(dir)
}
