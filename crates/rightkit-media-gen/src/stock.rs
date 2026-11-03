//! Stock and archive acquisition (issue #91): Pexels, Pixabay, Internet Archive, NASA images and
//! Wikimedia Commons, brought into the library with their rights, licence, attribution and source URL
//! recorded on the asset's `facts.json`.
//!
//! Sources (fetched 2026-10-17; see the report for what could not be fetched):
//! - Pexels: <https://www.pexels.com/api/documentation/>. `Authorization: <key>` header (no scheme);
//!   `page`, `per_page` (max 80); photo fields `id, url, photographer, photographer_url, src, alt,
//!   width, height`; "All content is available free of charge"; attribution asked for; 200 req/h.
//!   The search query parameter is `query` (not confirmed on the fetched page; see report).
//! - Pixabay: <https://pixabay.com/api/docs/>. `key` query parameter (required); `q` (max 100 chars),
//!   `image_type`, `page`, `per_page` (3 to 200), `safesearch`; hit fields `id, pageURL, user, tags,
//!   webformatURL, largeImageURL, imageWidth, imageHeight`; Content License; hotlinking not allowed,
//!   so bytes are downloaded; 100 req per 60 s per key.
//! - Internet Archive: <https://archive.org/advancedsearch.php> (`q`, `fl[]`, `rows`, `page`,
//!   `output=json`) and <https://archive.org/developers/md-read.html> (`https://archive.org/metadata/{id}`,
//!   `files[].name`). Item `licenseurl` is the item's stated licence. No key.
//! - NASA: <https://images.nasa.gov/docs/images.nasa.gov_api_docs.pdf> (v1.22.0): `GET /search`
//!   (`q`, `media_type`, `page`, `page_size`), Collection+JSON `collection.items[].data[0]`
//!   (`nasa_id, title, description, keywords, photographer, center`), `GET /asset/{nasa_id}` manifest
//!   links. No key in the document. NASA's guidelines say NASA content "generally [is] not subject to
//!   copyright in the United States" and third-party material is "marked", so NASA rights stay
//!   `unresolved` per item.
//! - Wikimedia Commons: <https://www.mediawiki.org/wiki/API:Imageinfo> (`prop=imageinfo`, `iiprop`
//!   `url|size|mime|extmetadata`, `iiextmetadatafilter`); extmetadata `LicenseShortName`, `LicenseUrl`,
//!   `Artist`, `Copyrighted`, `AttributionRequired`. Generator `search` with `gsrsearch`/`gsrnamespace`
//!   is the standard MediaWiki form (the fetched page did not show it; see report).
//!
//! Every network call goes through [`Fetch`], which the engine implements with the one outbound
//! policy in [`crate::host::http::Http`]. Tests inject mocked responses through the same trait.
//! Provider keys come from the credentials store (`credentials::get`) and are never echoed in errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::host::asset_facts::{
    AssetFacts, EvidenceKind, Origin, OriginKind, Rights, RightsStatus,
};
use crate::host::http::{Http, Request};
use serde_json::{json, Value};

use crate::store::{self, io_err};
use crate::{credentials, library, media, provenance, Context, GenError, GenResult};

/// Providers `asset.acquire` accepts, in the order the issue names them.
pub const PROVIDERS: &[&str] = &["pexels", "pixabay", "archive", "nasa", "wikimedia"];

const USER_AGENT: &str = "RightKit-media-gen/0.1 (asset acquisition)";
const PEXELS_LICENCE: &str = "https://www.pexels.com/license/";
const PEXELS_DOCS: &str =
    "https://www.pexels.com/api/documentation/ (All content is available free of charge)";
const PIXABAY_TERMS: &str = "https://pixabay.com/service/terms/";
const PIXABAY_DOCS: &str =
    "https://pixabay.com/api/docs/ (released under the Pixabay Content License)";
const NASA_GUIDE: &str = "https://www.nasa.gov/nasa-brand-center/images-and-media/ (NASA content generally not subject to copyright in the United States; third-party material is marked)";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];

fn bad(message: impl Into<String>) -> GenError {
    GenError::new("invalid_request", message)
}

// ---------------------------------------------------------------------------------------------
// Transport seam
// ---------------------------------------------------------------------------------------------

/// One outbound GET. `auth` is a secret header; `redact` lists secret values (a key in the URL)
/// that must never appear in an error message.
#[derive(Debug, Clone)]
pub struct Req {
    pub url: String,
    pub auth: Option<(&'static str, String)>,
    pub redact: Vec<String>,
    /// Allow a large body (a media download) instead of a metadata-sized one.
    pub large: bool,
}

impl Req {
    fn plain(url: String) -> Self {
        Self {
            url,
            auth: None,
            redact: Vec::new(),
            large: false,
        }
    }
}

/// The network seam. The engine uses [`Live`]; tests use a mock.
pub trait Fetch {
    fn get(&self, req: &Req) -> GenResult<Vec<u8>>;
}

/// Live transport on the shared RightKit HTTP policy (no retries, public addresses only).
pub struct Live(Http);

impl Live {
    pub fn new() -> Self {
        Self(Http::new(Duration::from_secs(10), Duration::from_secs(120)))
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for Live {
    fn get(&self, req: &Req) -> GenResult<Vec<u8>> {
        let mut r = Request::get(req.url.clone()).header("User-Agent", USER_AGENT);
        if let Some((name, value)) = &req.auth {
            r = r.secret_header(*name, value.as_str());
        }
        r = if req.large {
            r.max_response_bytes(80 << 20)
        } else {
            r.max_response_bytes(8 << 20)
        };
        let resp = self.0.send(r).map_err(|e| {
            GenError::new(
                "transport",
                redact(&format!("stock transport error: {e}"), &req.redact),
            )
        })?;
        if !(200..300).contains(&resp.status) {
            return Err(GenError::new(
                "provider_http",
                format!("provider returned HTTP {}", resp.status),
            ));
        }
        Ok(resp.body.to_vec())
    }
}

/// Replace every secret value in `message` with a placeholder.
pub fn redact(message: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(message.to_string(), |m, s| {
            m.replace(s.as_str(), "[redacted]")
        })
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `base?k=v&...` with values percent-encoded. Keys are written as given (`fl[]` stays literal).
fn url(base: &str, params: &[(&str, String)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    format!("{base}?{}", query.join("&"))
}

fn json_body(bytes: &[u8]) -> GenResult<Value> {
    serde_json::from_slice(bytes).map_err(|e| {
        GenError::new(
            "invalid_response",
            format!("provider sent invalid JSON: {e}"),
        )
    })
}

fn require_key(ctx: &Context, provider: &str) -> GenResult<String> {
    credentials::get(ctx, provider)?
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            GenError::new(
                "missing_credential",
                format!("no {provider} API key is stored; set it with credentials.set"),
            )
        })
}

// ---------------------------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------------------------

/// What must be fetched before the bytes can be downloaded.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolve {
    /// `download_url` is already known.
    None,
    /// Fetch `/asset/{nasa_id}` to find the largest image link.
    NasaAsset(String),
    /// Fetch `/metadata/{identifier}` to find the first image file.
    ArchiveItem(String),
}

/// One search result, normalised across providers.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub provider: &'static str,
    pub provider_id: String,
    pub title: String,
    pub creator: Option<String>,
    pub source_url: String,
    pub download_url: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub licence: Option<String>,
    pub licence_url: Option<String>,
    pub licence_evidence: Vec<String>,
    pub attribution: Option<String>,
    pub rights: RightsStatus,
    pub resolve: Resolve,
}

fn first_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()).filter(|s| !s.is_empty()),
        Value::Array(a) => a
            .iter()
            .find_map(|x| x.as_str().filter(|s| !s.is_empty()).map(str::to_string)),
        _ => None,
    }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' if inside => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn has_image_ext(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Pexels `GET /v1/search` body → candidates.
pub fn parse_pexels(v: &Value) -> Vec<Candidate> {
    v["photos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let creator = p["photographer"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(Candidate {
                provider: "pexels",
                provider_id: p["id"].as_u64()?.to_string(),
                title: p["alt"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Pexels photo")
                    .to_string(),
                source_url: p["url"].as_str()?.to_string(),
                download_url: p["src"]["large"]
                    .as_str()
                    .or(p["src"]["original"].as_str())
                    .map(str::to_string),
                width: p["width"].as_u64(),
                height: p["height"].as_u64(),
                licence: Some("Pexels License".into()),
                licence_url: Some(PEXELS_LICENCE.into()),
                licence_evidence: vec![PEXELS_DOCS.into()],
                attribution: creator.as_ref().map(|n| format!("Photo by {n} on Pexels")),
                creator,
                rights: RightsStatus::Cleared,
                resolve: Resolve::None,
            })
        })
        .collect()
}

/// Pixabay `GET /api/` body → candidates.
pub fn parse_pixabay(v: &Value) -> Vec<Candidate> {
    v["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let user = h["user"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(Candidate {
                provider: "pixabay",
                provider_id: h["id"].as_u64()?.to_string(),
                title: h["tags"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Pixabay image")
                    .to_string(),
                source_url: h["pageURL"].as_str()?.to_string(),
                download_url: h["largeImageURL"].as_str().map(str::to_string),
                width: h["imageWidth"].as_u64(),
                height: h["imageHeight"].as_u64(),
                licence: Some("Pixabay Content License".into()),
                licence_url: Some(PIXABAY_TERMS.into()),
                licence_evidence: vec![PIXABAY_DOCS.into()],
                attribution: user.as_ref().map(|u| format!("Image by {u} from Pixabay")),
                creator: user,
                rights: RightsStatus::Cleared,
                resolve: Resolve::None,
            })
        })
        .collect()
}

/// NASA `GET /search` body → candidates (images only). The download link comes from the asset manifest.
pub fn parse_nasa_search(v: &Value) -> Vec<Candidate> {
    v["collection"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let d = &item["data"][0];
            if d["media_type"].as_str()? != "image" {
                return None;
            }
            let nasa_id = d["nasa_id"].as_str()?.to_string();
            let creator = d["photographer"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(Candidate {
                provider: "nasa",
                provider_id: nasa_id.clone(),
                title: d["title"].as_str().unwrap_or("NASA image").to_string(),
                source_url: format!("https://images.nasa.gov/details/{nasa_id}"),
                download_url: None,
                width: None,
                height: None,
                licence: None,
                licence_url: None,
                licence_evidence: vec![NASA_GUIDE.into()],
                attribution: Some(match &creator {
                    Some(c) => format!("Credit: {c}, NASA"),
                    None => "Credit: NASA".to_string(),
                }),
                creator,
                rights: RightsStatus::Unresolved,
                resolve: Resolve::NasaAsset(nasa_id),
            })
        })
        .collect()
}

/// NASA `GET /asset/{nasa_id}` body → the largest image link (`~orig`, then `~large`, then `~medium`).
pub fn nasa_asset_url(v: &Value) -> Option<String> {
    let hrefs: Vec<&str> = v["collection"]["items"]
        .as_array()?
        .iter()
        .filter_map(|i| i["href"].as_str())
        .collect();
    ["~orig", "~large", "~medium"].iter().find_map(|marker| {
        hrefs
            .iter()
            .find(|h| h.contains(marker) && has_image_ext(h))
            .map(|h| h.to_string())
    })
}

/// Internet Archive advancedsearch body → candidates. Only items that state a licence URL are
/// `cleared`; the rest stay `unresolved`.
pub fn parse_archive_search(v: &Value) -> Vec<Candidate> {
    v["response"]["docs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let identifier = first_str(&d["identifier"])?;
            let licence_url = first_str(&d["licenseurl"]);
            let creator = first_str(&d["creator"]);
            let title = first_str(&d["title"]).unwrap_or_else(|| identifier.clone());
            let source_url = format!("https://archive.org/details/{identifier}");
            let (rights, licence_evidence) = match &licence_url {
                Some(u) => (
                    RightsStatus::Cleared,
                    vec![format!("archive.org item licenseurl {u}")],
                ),
                None => (RightsStatus::Unresolved, Vec::new()),
            };
            Some(Candidate {
                provider: "archive",
                provider_id: identifier.clone(),
                title,
                attribution: Some(match &creator {
                    Some(c) => format!("{c}, \"{identifier}\", Internet Archive ({source_url})"),
                    None => format!("\"{identifier}\", Internet Archive ({source_url})"),
                }),
                creator,
                source_url,
                download_url: None,
                width: None,
                height: None,
                licence: None,
                licence_url,
                licence_evidence,
                rights,
                resolve: Resolve::ArchiveItem(identifier),
            })
        })
        .collect()
}

/// Internet Archive `GET /metadata/{id}` body → the first image file's download URL.
pub fn archive_file_url(identifier: &str, v: &Value) -> Option<String> {
    v["files"]
        .as_array()?
        .iter()
        .filter_map(|f| f["name"].as_str())
        .find(|n| has_image_ext(n))
        .map(|name| {
            format!(
                "https://archive.org/download/{}/{}",
                enc(identifier),
                enc(name)
            )
        })
}

fn ext_value(meta: &Value, key: &str) -> Option<String> {
    meta[key]["value"]
        .as_str()
        .map(strip_tags)
        .filter(|s| !s.is_empty())
}

/// Wikimedia Commons `action=query&generator=search` body → candidates.
pub fn parse_wikimedia(v: &Value) -> Vec<Candidate> {
    let Some(pages) = v["query"]["pages"].as_object() else {
        return Vec::new();
    };
    let mut pages: Vec<&Value> = pages.values().collect();
    pages.sort_by_key(|p| p["index"].as_u64().unwrap_or(u64::MAX));
    pages
        .into_iter()
        .filter_map(|p| {
            let info = &p["imageinfo"][0];
            if !info["mime"].as_str()?.starts_with("image/") {
                return None;
            }
            let meta = &info["extmetadata"];
            let licence = ext_value(meta, "LicenseShortName");
            let licence_url = ext_value(meta, "LicenseUrl");
            let artist = ext_value(meta, "Artist");
            let copyrighted = ext_value(meta, "Copyrighted");
            let source_url = info["descriptionurl"].as_str()?.to_string();
            let title = p["title"]
                .as_str()
                .unwrap_or("Wikimedia Commons file")
                .trim_start_matches("File:")
                .to_string();
            let (rights, licence_evidence) = match &licence {
                Some(name) if copyrighted.as_deref() != Some("True") => (
                    RightsStatus::Cleared,
                    vec![format!(
                        "Wikimedia Commons extmetadata LicenseShortName: {name}"
                    )],
                ),
                _ => (RightsStatus::Unresolved, Vec::new()),
            };
            let attribution = match (&artist, &licence) {
                (Some(a), Some(l)) => {
                    Some(format!("{a}, {l}, via Wikimedia Commons ({source_url})"))
                }
                (Some(a), None) => Some(format!("{a}, via Wikimedia Commons ({source_url})")),
                _ => None,
            };
            Some(Candidate {
                provider: "wikimedia",
                provider_id: p["pageid"].as_u64()?.to_string(),
                title,
                creator: artist,
                source_url,
                download_url: info["url"].as_str().map(str::to_string),
                width: info["width"].as_u64(),
                height: info["height"].as_u64(),
                licence,
                licence_url,
                licence_evidence,
                attribution,
                rights,
                resolve: Resolve::None,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Search and resolve
// ---------------------------------------------------------------------------------------------

/// One search page from `provider`.
pub fn search(
    fetch: &dyn Fetch,
    ctx: &Context,
    provider: &str,
    query: &str,
    page: u64,
    per_page: u64,
) -> GenResult<Vec<Candidate>> {
    let page_s = page.to_string();
    let per_s = per_page.to_string();
    match provider {
        "pexels" => {
            let key = require_key(ctx, "pexels")?;
            let req = Req {
                url: url(
                    "https://api.pexels.com/v1/search",
                    &[
                        ("query", query.into()),
                        ("page", page_s),
                        ("per_page", per_s),
                    ],
                ),
                auth: Some(("Authorization", key.clone())),
                redact: vec![key],
                large: false,
            };
            Ok(parse_pexels(&json_body(&fetch.get(&req)?)?))
        }
        "pixabay" => {
            let key = require_key(ctx, "pixabay")?;
            let req = Req {
                url: url(
                    "https://pixabay.com/api/",
                    &[
                        ("key", key.clone()),
                        ("q", query.into()),
                        ("image_type", "photo".into()),
                        ("page", page_s),
                        ("per_page", per_s),
                        ("safesearch", "true".into()),
                    ],
                ),
                auth: None,
                redact: vec![key],
                large: false,
            };
            Ok(parse_pixabay(&json_body(&fetch.get(&req)?)?))
        }
        "archive" => {
            let req = Req::plain(url(
                "https://archive.org/advancedsearch.php",
                &[
                    ("q", format!("({query}) AND mediatype:image")),
                    ("fl[]", "identifier".into()),
                    ("fl[]", "title".into()),
                    ("fl[]", "creator".into()),
                    ("fl[]", "licenseurl".into()),
                    ("rows", per_s),
                    ("page", page_s),
                    ("output", "json".into()),
                ],
            ));
            Ok(parse_archive_search(&json_body(&fetch.get(&req)?)?))
        }
        "nasa" => {
            let req = Req::plain(url(
                "https://images-api.nasa.gov/search",
                &[
                    ("q", query.into()),
                    ("media_type", "image".into()),
                    ("page", page_s),
                    ("page_size", per_s),
                ],
            ));
            Ok(parse_nasa_search(&json_body(&fetch.get(&req)?)?))
        }
        "wikimedia" => {
            let offset = (page.saturating_sub(1) * per_page).to_string();
            let req = Req::plain(url(
                "https://commons.wikimedia.org/w/api.php",
                &[
                    ("action", "query".into()),
                    ("format", "json".into()),
                    ("generator", "search".into()),
                    ("gsrsearch", query.into()),
                    ("gsrnamespace", "6".into()),
                    ("gsrlimit", per_s),
                    ("gsroffset", offset),
                    ("prop", "imageinfo".into()),
                    ("iiprop", "url|size|mime|extmetadata".into()),
                    (
                        "iiextmetadatafilter",
                        "LicenseShortName|LicenseUrl|Artist|Copyrighted|AttributionRequired".into(),
                    ),
                ],
            ));
            Ok(parse_wikimedia(&json_body(&fetch.get(&req)?)?))
        }
        other => Err(bad(format!("unknown provider {other}"))),
    }
}

/// Fill in the download URL a candidate needs an extra call for.
fn resolve_download(fetch: &dyn Fetch, cand: &Candidate) -> GenResult<String> {
    let missing = || {
        GenError::new(
            "no_download",
            format!(
                "{} {} has no downloadable image",
                cand.provider, cand.provider_id
            ),
        )
    };
    match (&cand.download_url, &cand.resolve) {
        (Some(u), _) => Ok(u.clone()),
        (None, Resolve::NasaAsset(id)) => {
            let v = json_body(&fetch.get(&Req::plain(format!(
                "https://images-api.nasa.gov/asset/{}",
                enc(id)
            )))?)?;
            nasa_asset_url(&v).ok_or_else(missing)
        }
        (None, Resolve::ArchiveItem(id)) => {
            let v = json_body(&fetch.get(&Req::plain(format!(
                "https://archive.org/metadata/{}",
                enc(id)
            )))?)?;
            archive_file_url(id, &v).ok_or_else(missing)
        }
        (None, Resolve::None) => Err(missing()),
    }
}

// ---------------------------------------------------------------------------------------------
// Acquisition
// ---------------------------------------------------------------------------------------------

enum Outcome {
    Added(Value),
    Duplicate(String),
}

/// Download one candidate, dedup it by content hash, and store it with its facts.
fn acquire_one(ctx: &Context, fetch: &dyn Fetch, cand: &Candidate) -> GenResult<Outcome> {
    let download = resolve_download(fetch, cand)?;
    let bytes = fetch.get(&Req {
        url: download,
        auth: None,
        redact: Vec::new(),
        large: true,
    })?;
    let mime = media::sniff_mime(&bytes).ok_or_else(|| {
        GenError::new(
            "unsupported_media",
            "the download is not a supported media type",
        )
    })?;
    let sha256 = library::sha256_hex(&bytes);
    if let Some(existing) = library::find_by_sha256(ctx, &sha256)? {
        return Ok(Outcome::Duplicate(existing));
    }
    let asset_id = uuid::Uuid::new_v4().to_string();
    let dir = ctx.library_dir().join(&asset_id);
    fs::create_dir_all(&dir).map_err(io_err)?;
    let cleanup = |e: GenError| {
        let _ = fs::remove_dir_all(&dir);
        e
    };
    let facts = AssetFacts {
        origin: Origin {
            kind: OriginKind::Acquired,
            provider: Some(cand.provider.to_string()),
            source: Some(cand.source_url.clone()),
            provider_id: Some(cand.provider_id.clone()),
            content_sha256: Some(sha256.clone()),
            ..Origin::default()
        },
        // A stock photo is not evidence that the depicted event happened as shown; left unknown.
        evidence: EvidenceKind::Unknown,
        rights: Rights {
            status: cand.rights,
            licence: cand.licence.clone(),
            licence_url: cand.licence_url.clone(),
            licence_evidence: cand.licence_evidence.clone(),
            source: Some(cand.source_url.clone()),
            attribution: cand.attribution.clone(),
            permitted_uses: Vec::new(),
            consent_refs: Vec::new(),
        },
    };
    let acquisition = json!({
        "title": cand.title,
        "creator": cand.creator,
        "width": cand.width,
        "height": cand.height,
        "mime": mime,
        "retrieved_at": crate::store::now(),
    });
    provenance::write_facts_file(
        &dir,
        &facts,
        &Value::Null,
        Some(json!({"acquisition": acquisition})),
    )
    .map_err(cleanup)?;
    let path: PathBuf = dir.join(format!("{asset_id}.{}", library::ext_for_mime(mime)));
    rightkit_fs::atomic_write(&path, &bytes)
        .map_err(|e| cleanup(io_err(std::io::Error::other(e))))?;
    // The library row: same job-backed record an import writes, so library search finds the asset with
    // its origin and rights. A failure here removes the folder, so no unindexed copy is left behind.
    let row = json!({
        "asset_id": asset_id,
        "path": path.to_string_lossy(),
        "sha256": sha256,
        "bytes": bytes.len(),
        "mime": mime,
        "facts": facts.to_value(),
    });
    let request = json!({
        "kind": "asset_acquire", "provider": cand.provider, "provider_id": cand.provider_id,
        "source_url": cand.source_url, "title": cand.title,
    });
    store::open(ctx)
        .and_then(|db| {
            store::record_standalone_asset(
                &db,
                "asset_acquire",
                cand.provider,
                "stock",
                &request,
                row,
            )
        })
        .map_err(cleanup)?;
    Ok(Outcome::Added(json!({
        "asset_id": asset_id,
        "path": path.to_string_lossy(),
        "sha256": sha256,
        "bytes": bytes.len(),
        "mime": mime,
        "provider": cand.provider,
        "provider_id": cand.provider_id,
        "source_url": cand.source_url,
        "title": cand.title,
        "creator": cand.creator,
        "licence": cand.licence,
        "licence_url": cand.licence_url,
        "attribution": cand.attribution,
        "rights_status": serde_json::to_value(cand.rights).unwrap_or(Value::Null),
    })))
}

fn candidate_json(c: &Candidate) -> Value {
    json!({
        "provider": c.provider,
        "provider_id": c.provider_id,
        "title": c.title,
        "source_url": c.source_url,
        "width": c.width,
        "height": c.height,
        "licence": c.licence,
        "licence_url": c.licence_url,
        "attribution": c.attribution,
        "rights_status": serde_json::to_value(c.rights).unwrap_or(Value::Null),
    })
}

fn bounded(input: &Value, key: &str, default: u64, min: u64, max: u64) -> GenResult<u64> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| bad(format!("{key} must be a whole number from {min} to {max}"))),
    }
}

fn save_state(dir: &Path, path: &Path, state: &Value) -> GenResult<()> {
    fs::create_dir_all(dir).map_err(io_err)?;
    rightkit_fs::atomic_write(path, state.to_string().as_bytes())
        .map_err(|e| io_err(std::io::Error::other(e)))
}

/// `asset.acquire` `{provider, query, limit?, max_pages?, per_page?, run?, dry_run?}`.
///
/// Searches `provider` page by page, downloads up to `limit` new assets, and stores each with its
/// rights, licence, attribution and source URL. Duplicates by content hash reuse the existing asset.
/// Progress is saved after every candidate under `<data_dir>/acquire/<run>.json`, so re-running with
/// the same `run` resumes at the next unprocessed result and never re-downloads a finished one.
/// `dry_run` lists candidates and writes nothing.
pub fn acquire_command(ctx: &Context, input: &Value, dry_run: bool) -> GenResult<Value> {
    acquire_with(ctx, &Live::new(), input, dry_run)
}

/// [`acquire_command`] over an injected transport.
pub fn acquire_with(
    ctx: &Context,
    fetch: &dyn Fetch,
    input: &Value,
    dry_run: bool,
) -> GenResult<Value> {
    let provider = input
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("provider is required"))?;
    if !PROVIDERS.contains(&provider) {
        return Err(bad(format!(
            "provider must be one of {}",
            PROVIDERS.join(", ")
        )));
    }
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty() && q.chars().count() <= 100 && !q.chars().any(char::is_control))
        .ok_or_else(|| bad("query is required: 1 to 100 characters of plain text"))?;
    let limit = bounded(input, "limit", 5, 1, 50)?;
    let max_pages = bounded(input, "max_pages", 1, 1, 5)?;
    let per_page = bounded(input, "per_page", 20, 1, 50)?;
    let dry_run = dry_run
        || input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let run = match input.get("run").and_then(Value::as_str) {
        Some(r)
            if !r.is_empty()
                && r.len() <= 64
                && r.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            r.to_string()
        }
        Some(_) => return Err(bad("run must be 1 to 64 letters, digits, '-' or '_'")),
        None => {
            let digest = library::sha256_hex(format!("{provider}\0{query}").as_bytes());
            digest[..16].to_string()
        }
    };

    let state_dir = ctx.data_dir.join("acquire");
    let state_path = state_dir.join(format!("{run}.json"));
    let mut state: Value = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            GenError::new(
                "invalid_state",
                format!("run {run} state is unreadable: {e}"),
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({
            "provider": provider, "query": query, "next_page": 1, "done": [], "assets": []
        }),
        Err(e) => return Err(io_err(e)),
    };
    if state["provider"] != provider || state["query"] != query {
        return Err(GenError::new(
            "invalid_state",
            format!("run {run} belongs to another provider or query"),
        ));
    }
    let mut done: Vec<String> = state["done"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let mut all_assets: Vec<Value> = state["assets"].as_array().cloned().unwrap_or_default();
    let mut page = state["next_page"].as_u64().unwrap_or(1).max(1);

    let mut added: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut listing: Vec<Value> = Vec::new();
    let mut taken = 0u64;
    let mut pages_run = 0u64;
    let mut exhausted = false;

    'pages: while pages_run < max_pages && taken < limit {
        let candidates = search(fetch, ctx, provider, query, page, per_page)?;
        pages_run += 1;
        if candidates.is_empty() {
            exhausted = true;
            break;
        }
        let mut finished_page = true;
        for cand in &candidates {
            if taken >= limit {
                finished_page = false;
                break;
            }
            let key = format!("{}:{}", cand.provider, cand.provider_id);
            if done.contains(&key) {
                continue;
            }
            if dry_run {
                listing.push(candidate_json(cand));
                taken += 1;
                continue;
            }
            match acquire_one(ctx, fetch, cand) {
                Ok(Outcome::Added(asset)) => {
                    all_assets.push(asset.clone());
                    added.push(asset);
                    done.push(key);
                    taken += 1;
                }
                Ok(Outcome::Duplicate(existing)) => {
                    skipped.push(
                        json!({"key": key, "reason": "duplicate", "existing_asset_id": existing}),
                    );
                    done.push(key);
                }
                Err(e) if e.code == "unsupported_media" || e.code == "no_download" => {
                    skipped.push(json!({"key": key, "reason": e.code, "message": e.message}));
                    done.push(key);
                }
                // Transport and provider errors are not final: the item is retried on resume.
                Err(e) => {
                    skipped.push(json!({"key": key, "reason": e.code, "message": e.message}));
                }
            }
            if !dry_run {
                state = json!({
                    "provider": provider, "query": query, "next_page": page, "done": done, "assets": all_assets
                });
                save_state(&state_dir, &state_path, &state)?;
            }
        }
        if finished_page {
            page += 1;
        }
        if !finished_page {
            break 'pages;
        }
    }

    if !dry_run {
        state = json!({
            "provider": provider, "query": query, "next_page": page, "done": done, "assets": all_assets
        });
        save_state(&state_dir, &state_path, &state)?;
    }
    Ok(json!({
        "provider": provider,
        "query": query,
        "run": run,
        "dry_run": dry_run,
        "next_page": page,
        "exhausted": exhausted,
        "assets": added,
        "skipped": skipped,
        "candidates": listing,
    }))
}

// ---------------------------------------------------------------------------------------------
// Tests (mocked responses only; no network)
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialSource, MemoryCredentials};
    use std::cell::RefCell;
    use std::sync::Arc;

    /// Returns the body of the first route whose needle appears in the URL; `Err(status)` for HTTP errors.
    struct Mock {
        routes: Vec<(&'static str, Result<Vec<u8>, u16>)>,
        seen: RefCell<Vec<String>>,
    }

    impl Mock {
        fn new(routes: Vec<(&'static str, Result<Vec<u8>, u16>)>) -> Self {
            Self {
                routes,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetch for Mock {
        fn get(&self, req: &Req) -> GenResult<Vec<u8>> {
            self.seen.borrow_mut().push(req.url.clone());
            for (needle, body) in &self.routes {
                if req.url.contains(needle) {
                    return body.clone().map_err(|s| {
                        GenError::new("provider_http", format!("provider returned HTTP {s}"))
                    });
                }
            }
            Err(GenError::new(
                "transport",
                format!("no mock route for {}", req.url),
            ))
        }
    }

    fn png(tag: u8) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        v.extend([0, 0, 0, 13, tag]);
        v
    }

    fn ctx_with_keys() -> (Context, PathBuf) {
        let dir = std::env::temp_dir().join(format!("stock-test-{}", uuid::Uuid::new_v4()));
        let creds = Arc::new(MemoryCredentials::new());
        creds.set("pexels", "PEXELS-TEST-KEY").unwrap();
        creds.set("pixabay", "PIXABAY-TEST-KEY").unwrap();
        (Context::new(dir.clone()).with_credentials(creds), dir)
    }

    fn pexels_page(ids: &[u64]) -> Vec<u8> {
        let photos: Vec<Value> = ids
            .iter()
            .map(|id| {
                json!({
                    "id": id, "url": format!("https://www.pexels.com/photo/{id}/"), "photographer": "Jane Roe",
                    "photographer_url": "https://www.pexels.com/@jane", "alt": "a red bicycle",
                    "width": 4000, "height": 3000,
                    "src": {"original": format!("https://images.pexels.test/{id}.jpg"), "large": format!("https://images.pexels.test/photos/{id}.jpg")}
                })
            })
            .collect();
        json!({"photos": photos, "page": 1, "per_page": 20})
            .to_string()
            .into_bytes()
    }

    #[test]
    fn pexels_maps_licence_attribution_and_source() {
        let v = json!({"photos": [{"id": 7, "url": "https://www.pexels.com/photo/7/", "photographer": "Jane Roe",
            "alt": "dog", "width": 10, "height": 20, "src": {"large": "https://images.pexels.test/7.jpg"}}]});
        let c = parse_pexels(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].provider_id, "7");
        assert_eq!(
            c[0].attribution.as_deref(),
            Some("Photo by Jane Roe on Pexels")
        );
        assert_eq!(c[0].licence.as_deref(), Some("Pexels License"));
        assert_eq!(c[0].rights, RightsStatus::Cleared);
        assert_eq!(
            c[0].download_url.as_deref(),
            Some("https://images.pexels.test/7.jpg")
        );
    }

    #[test]
    fn pixabay_maps_hits_and_content_licence() {
        let v = json!({"hits": [{"id": 99, "pageURL": "https://pixabay.com/photos/x-99/", "user": "bob",
            "tags": "tree, green", "imageWidth": 1920, "imageHeight": 1080, "largeImageURL": "https://cdn.pixabay.test/99.jpg"}]});
        let c = parse_pixabay(&v);
        assert_eq!(c[0].provider_id, "99");
        assert_eq!(c[0].licence_url.as_deref(), Some(PIXABAY_TERMS));
        assert_eq!(
            c[0].attribution.as_deref(),
            Some("Image by bob from Pixabay")
        );
        assert_eq!(c[0].width, Some(1920));
    }

    #[test]
    fn nasa_search_then_asset_manifest_prefers_orig() {
        let search = json!({"collection": {"items": [{"data": [{"nasa_id": "as11-40-5874", "title": "Apollo 11",
            "media_type": "image", "photographer": "NASA/JSC"}]}, {"data": [{"nasa_id": "vid-1", "media_type": "video"}]}]}});
        let c = parse_nasa_search(&search);
        assert_eq!(c.len(), 1, "video results are dropped");
        assert_eq!(c[0].rights, RightsStatus::Unresolved);
        assert_eq!(
            c[0].source_url,
            "https://images.nasa.gov/details/as11-40-5874"
        );
        let asset = json!({"collection": {"items": [
            {"href": "https://images-assets.nasa.gov/image/as11-40-5874/as11-40-5874~medium.jpg"},
            {"href": "https://images-assets.nasa.gov/image/as11-40-5874/as11-40-5874~orig.tif"},
            {"href": "https://images-assets.nasa.gov/image/as11-40-5874/as11-40-5874~orig.jpg"}]}});
        assert_eq!(
            nasa_asset_url(&asset).as_deref(),
            Some("https://images-assets.nasa.gov/image/as11-40-5874/as11-40-5874~orig.jpg")
        );
    }

    #[test]
    fn archive_licence_url_clears_and_metadata_picks_image() {
        let search = json!({"response": {"docs": [
            {"identifier": "item-a", "title": "Plate A", "creator": ["Smith", "Jones"], "licenseurl": "https://creativecommons.org/publicdomain/mark/1.0/"},
            {"identifier": "item-b", "title": "Plate B"}]}});
        let c = parse_archive_search(&search);
        assert_eq!(c[0].rights, RightsStatus::Cleared);
        assert_eq!(
            c[0].licence_url.as_deref(),
            Some("https://creativecommons.org/publicdomain/mark/1.0/")
        );
        assert_eq!(c[1].rights, RightsStatus::Unresolved);
        let meta =
            json!({"files": [{"name": "notes.txt"}, {"name": "plate a.JPG"}, {"name": "b.png"}]});
        assert_eq!(
            archive_file_url("item-a", &meta).as_deref(),
            Some("https://archive.org/download/item-a/plate%20a.JPG")
        );
    }

    #[test]
    fn wikimedia_extmetadata_clears_and_strips_html() {
        let v = json!({"query": {"pages": {"55": {"pageid": 55, "title": "File:Owl.jpg", "index": 1,
            "imageinfo": [{"url": "https://upload.wikimedia.org/owl.jpg", "descriptionurl": "https://commons.wikimedia.org/wiki/File:Owl.jpg",
              "mime": "image/jpeg", "width": 800, "height": 600, "extmetadata": {
                "LicenseShortName": {"value": "CC BY-SA 4.0"},
                "LicenseUrl": {"value": "https://creativecommons.org/licenses/by-sa/4.0"},
                "Artist": {"value": "<a href=\"//x\">Ann Lee</a>"},
                "Copyrighted": {"value": "True"}}}]}}}});
        let c = parse_wikimedia(&v);
        assert_eq!(c[0].title, "Owl.jpg");
        assert_eq!(c[0].creator.as_deref(), Some("Ann Lee"));
        // Copyrighted True overrides the licence name: rights stay unresolved, never guessed as cleared.
        assert_eq!(c[0].rights, RightsStatus::Unresolved);
        assert!(c[0]
            .attribution
            .as_deref()
            .unwrap()
            .starts_with("Ann Lee, CC BY-SA 4.0, via Wikimedia Commons"));
    }

    #[test]
    fn redaction_removes_key_from_messages() {
        let msg = redact(
            "GET https://pixabay.com/api/?key=PIXABAY-TEST-KEY failed",
            &["PIXABAY-TEST-KEY".to_string()],
        );
        assert!(!msg.contains("PIXABAY-TEST-KEY"));
    }

    #[test]
    fn pixabay_error_status_is_reported_without_key() {
        let (ctx, dir) = ctx_with_keys();
        let mock = Mock::new(vec![("pixabay.com/api", Err(429))]);
        let input = json!({"provider": "pixabay", "query": "tree"});
        let err = acquire_with(&ctx, &mock, &input, false).unwrap_err();
        assert_eq!(err.code, "provider_http");
        assert!(!err.message.contains("PIXABAY-TEST-KEY"));
        assert!(
            mock.seen.borrow()[0].contains("key=PIXABAY-TEST-KEY"),
            "the key travels in the query string only"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_key_is_refused_before_any_request() {
        let dir = std::env::temp_dir().join(format!("stock-test-{}", uuid::Uuid::new_v4()));
        let ctx = Context::new(dir.clone()).with_credentials(Arc::new(MemoryCredentials::new()));
        let mock = Mock::new(vec![]);
        let err = acquire_with(
            &ctx,
            &mock,
            &json!({"provider": "pexels", "query": "bike"}),
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, "missing_credential");
        assert!(mock.seen.borrow().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn acquire_stores_facts_and_dedups_by_content_hash() {
        let (ctx, dir) = ctx_with_keys();
        let pexels = Mock::new(vec![
            ("api.pexels.com/v1/search", Ok(pexels_page(&[1]))),
            ("images.pexels.test/photos/1.jpg", Ok(png(1))),
        ]);
        let first = acquire_with(
            &ctx,
            &pexels,
            &json!({"provider": "pexels", "query": "bike", "limit": 1}),
            false,
        )
        .unwrap();
        assert_eq!(first["assets"].as_array().unwrap().len(), 1);
        let asset_id = first["assets"][0]["asset_id"].as_str().unwrap().to_string();
        let facts: Value = serde_json::from_slice(
            &fs::read(ctx.library_dir().join(&asset_id).join("facts.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(facts["facts"]["origin"]["kind"], "acquired");
        assert_eq!(facts["facts"]["origin"]["provider"], "pexels");
        assert_eq!(facts["facts"]["origin"]["provider_id"], "1");
        assert_eq!(
            facts["facts"]["origin"]["content_sha256"],
            library::sha256_hex(&png(1)).as_str()
        );
        assert_eq!(facts["facts"]["rights"]["licence_url"], PEXELS_LICENCE);
        assert_eq!(
            facts["facts"]["rights"]["attribution"],
            "Photo by Jane Roe on Pexels"
        );
        assert_eq!(facts["facts"]["rights"]["status"], "cleared");

        // The library row exists and carries the same origin and rights as facts.json.
        let db = store::open(&ctx).unwrap();
        let row = store::get_asset(&db, &asset_id)
            .unwrap()
            .expect("acquired asset has a library row");
        assert_eq!(row.sha256, library::sha256_hex(&png(1)));
        let (recorded, _) = store::asset_facts(&db, &asset_id).unwrap();
        assert_eq!(recorded.origin.provider.as_deref(), Some("pexels"));
        assert_eq!(recorded.rights.status, RightsStatus::Cleared);
        assert_eq!(recorded.rights.licence_url.as_deref(), Some(PEXELS_LICENCE));

        // Same bytes from Wikimedia Commons: no second asset.
        let wiki = Mock::new(vec![
            ("commons.wikimedia.org/w/api.php", Ok(json!({"query": {"pages": {"9": {"pageid": 9, "title": "File:Same.png", "index": 1,
                "imageinfo": [{"url": "https://upload.wikimedia.org/same.png", "descriptionurl": "https://commons.wikimedia.org/wiki/File:Same.png",
                "mime": "image/png", "extmetadata": {"LicenseShortName": {"value": "Public domain"}}}]}}}}).to_string().into_bytes())),
            ("upload.wikimedia.org/same.png", Ok(png(1))),
        ]);
        let second = acquire_with(
            &ctx,
            &wiki,
            &json!({"provider": "wikimedia", "query": "bike", "limit": 1}),
            false,
        )
        .unwrap();
        assert!(second["assets"].as_array().unwrap().is_empty());
        assert_eq!(second["skipped"][0]["reason"], "duplicate");
        assert_eq!(second["skipped"][0]["existing_asset_id"], asset_id.as_str());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unresolved_nasa_asset_is_recorded_with_unresolved_rights() {
        let (ctx, dir) = ctx_with_keys();
        let mock = Mock::new(vec![
            (
                "images-api.nasa.gov/search",
                Ok(
                    json!({"collection": {"items": [{"data": [{"nasa_id": "n1", "title": "Probe",
                "media_type": "image", "photographer": "NASA/JPL"}]}]}})
                    .to_string()
                    .into_bytes(),
                ),
            ),
            (
                "images-api.nasa.gov/asset/n1",
                Ok(json!({"collection": {"items": [
                {"href": "https://images-assets.nasa.gov/image/n1/n1~orig.jpg"}]}})
                .to_string()
                .into_bytes()),
            ),
            ("images-assets.nasa.gov/image/n1/n1~orig.jpg", Ok(png(3))),
        ]);
        let out = acquire_with(
            &ctx,
            &mock,
            &json!({"provider": "nasa", "query": "probe", "limit": 1}),
            false,
        )
        .unwrap();
        let asset_id = out["assets"][0]["asset_id"].as_str().unwrap().to_string();
        assert_eq!(out["assets"][0]["rights_status"], "unresolved");
        let db = store::open(&ctx).unwrap();
        assert!(
            store::get_asset(&db, &asset_id).unwrap().is_some(),
            "row is recorded"
        );
        let (facts, _) = store::asset_facts(&db, &asset_id).unwrap();
        assert_eq!(facts.origin.provider.as_deref(), Some("nasa"));
        assert_eq!(facts.rights.status, RightsStatus::Unresolved);
        assert_eq!(
            facts.rights.licence_url, None,
            "no licence is invented for NASA items"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dry_run_lists_without_downloading_or_writing() {
        let (ctx, dir) = ctx_with_keys();
        let mock = Mock::new(vec![("api.pexels.com/v1/search", Ok(pexels_page(&[1, 2])))]);
        let out = acquire_with(
            &ctx,
            &mock,
            &json!({"provider": "pexels", "query": "bike", "limit": 2}),
            true,
        )
        .unwrap();
        assert_eq!(out["candidates"].as_array().unwrap().len(), 2);
        assert!(mock
            .seen
            .borrow()
            .iter()
            .all(|u| !u.contains("images.pexels.test")));
        assert!(!ctx.library_dir().exists());
        assert!(!dir.join("acquire").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_continues_after_the_last_saved_candidate() {
        let (ctx, dir) = ctx_with_keys();
        let mock = Mock::new(vec![
            ("api.pexels.com/v1/search", Ok(pexels_page(&[1, 2]))),
            ("images.pexels.test/photos/1.jpg", Ok(png(1))),
            ("images.pexels.test/photos/2.jpg", Ok(png(2))),
        ]);
        let input =
            json!({"provider": "pexels", "query": "bike", "limit": 1, "run": "resume-test"});
        let first = acquire_with(&ctx, &mock, &input, false).unwrap();
        assert_eq!(first["assets"][0]["provider_id"], "1");
        assert_eq!(
            first["next_page"], 1,
            "the page is unfinished, so it stays the resume point"
        );

        let second = acquire_with(&ctx, &mock, &input, false).unwrap();
        assert_eq!(second["assets"][0]["provider_id"], "2");
        assert!(
            mock.seen
                .borrow()
                .iter()
                .filter(|u| u.contains("photos/1.jpg"))
                .count()
                == 1,
            "the finished candidate is not downloaded again"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_input_by_name() {
        let (ctx, dir) = ctx_with_keys();
        let mock = Mock::new(vec![]);
        let e = acquire_with(
            &ctx,
            &mock,
            &json!({"provider": "flickr", "query": "x"}),
            false,
        )
        .unwrap_err();
        assert_eq!(e.code, "invalid_request");
        let e = acquire_with(
            &ctx,
            &mock,
            &json!({"provider": "nasa", "query": "x", "limit": 0}),
            false,
        )
        .unwrap_err();
        assert!(e.message.contains("limit"));
        let _ = fs::remove_dir_all(dir);
    }
}
