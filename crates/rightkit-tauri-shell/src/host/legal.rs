//! The native legal receipt (PK10-2): `rightkit_legal_receipt_read`, `_create` and `_reset`, the
//! commands `@rightkit/onboarding`'s `createNativeLegalReceiptPort` calls.
//!
//! What the user agreed to is a fact the app must be able to show later, so the webview is not
//! trusted to write it. The renderer DECLARES what it showed (`version`, `documentsHash`,
//! `documents`, `eligibilityBasis`, `surface`); the service
//!
//! * compares the declaration with the [`LegalManifest`] the app registered natively, field for
//!   field, and refuses a mismatch (`legal_version_mismatch`, `legal_hash_mismatch`,
//!   `legal_documents_mismatch`) before anything is stored;
//! * stamps the acceptance time itself, from its own [`LegalClock`] (UTC, the
//!   `YYYY-MM-DDTHH:MM:SS.mmmZ` form the renderer checks). A declaration that carries a time, or any
//!   field this module does not know, is refused outright (`invalid`): the request type is closed;
//! * stores the record in the app's own settings document (a `rightkit-app-settings`
//!   [`Document`], so it gets the atomic write, backup and cross-process lock of every PK04 store),
//!   and returns the stored record.
//!
//! Every command checks the caller first, under [`ServiceId::LEGAL`]: a registered content webview,
//! a foreign origin or an untrusted window label reaches nothing.
//!
//! # Legacy receipts
//! An app that kept its receipt in another store passes [`LegalReceiptService::with_legacy_import`].
//! On the first `read` that finds no receipt, the importer's record is stored once, only when its
//! version and documents hash equal the manifest's and its time is canonical UTC; anything else is
//! ignored. The document existing afterwards (even after a `reset`) is what stops a second import.
//!
//! # What is NOT here
//! No command writes a record the renderer supplies, and none reads one back from a path the
//! renderer names: the document and its location are fixed by the app when it builds the store.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rightkit_app_settings::{Document, Error as StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Manager, Webview, Wry};

use super::policy::ServiceId;
use super::{blocking, HostError, HostServices};
use crate::caller_gate::{self, CallerFacts};

/// The schema version the receipt document is stored under.
pub const RECEIPT_DOCUMENT_VERSION: u32 = 1;
/// Earliest time the service will stamp: a clock before this is not set, and a receipt carrying it
/// would be worthless, so `create` is refused (`legal_clock_invalid`) rather than recording it.
pub const MIN_PLAUSIBLE_MILLIS: i64 = 1_704_067_200_000; // 2024-01-01T00:00:00.000Z
const MAX_FIELD_BYTES: usize = 2048;
const MAX_DOCUMENTS: usize = 64;

// ---- time -----------------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for Unix milliseconds, the form `Date#toISOString` gives for years
/// 0000 to 9999. `None` outside that range.
pub fn format_utc_millis(ms: i64) -> Option<String> {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    if !(0..=9999).contains(&y) {
        return None;
    }
    let (h, mi, s, milli) = (
        rem / 3_600_000,
        rem % 3_600_000 / 60_000,
        rem % 60_000 / 1000,
        rem % 1000,
    );
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"
    ))
}

/// Unix milliseconds of a canonical UTC timestamp, or `None` when `text` is not one: the exact
/// `YYYY-MM-DDTHH:MM:SS.mmmZ` shape of a real date and time that formats back to itself (the rule
/// `@rightkit/onboarding` applies to a stored record).
pub fn parse_canonical_utc(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 24 || !text.is_ascii() {
        return None;
    }
    let shape = b"dddd-dd-ddTdd:dd:dd.dddZ";
    for (c, s) in b.iter().zip(shape) {
        let ok = if *s == b'd' {
            c.is_ascii_digit()
        } else {
            c == s
        };
        if !ok {
            return None;
        }
    }
    let n = |range: std::ops::Range<usize>| text[range].parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, s, milli) = (n(11..13)?, n(14..16)?, n(17..19)?, n(20..23)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let ms =
        days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + milli;
    // A date that does not exist (February 30) rolls over and no longer formats back to itself.
    (format_utc_millis(ms).as_deref() == Some(text)).then_some(ms)
}

/// The service's source of time. The default reads the system clock.
pub trait LegalClock: Send + Sync {
    /// Unix milliseconds now.
    fn now_millis(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLegalClock;

impl LegalClock for SystemLegalClock {
    fn now_millis(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

// ---- data -----------------------------------------------------------------------------------

/// Who the user said they were agreeing as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EligibilityBasis {
    Individual,
    Enterprise,
}

/// Where the agreement was given. Today only the first-run (and re-acceptance) gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Surface {
    #[serde(rename = "first_run")]
    FirstRun,
}

/// One document of the set shown, the shape of `@rightkit/legal`'s declaration. Every field is
/// optional, as in the renderer's type; an unknown field is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub treatment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<bool>,
}

impl DeclaredDocument {
    fn fields(&self) -> impl Iterator<Item = &String> {
        [
            &self.id,
            &self.role,
            &self.title,
            &self.url,
            &self.version,
            &self.sha256,
            &self.treatment,
        ]
        .into_iter()
        .flatten()
    }
}

/// What the app registered natively as the text a user must have agreed to. The declaration is
/// compared with it in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalManifest {
    version: String,
    documents_hash: String,
    documents: Vec<DeclaredDocument>,
}

impl LegalManifest {
    /// `version` and `documents_hash` are what the app requires; `documents` is the exact set the
    /// gate shows, in order.
    pub fn new(
        version: impl Into<String>,
        documents_hash: impl Into<String>,
        documents: Vec<DeclaredDocument>,
    ) -> Result<Self, HostError> {
        let manifest = Self {
            version: version.into(),
            documents_hash: documents_hash.into(),
            documents,
        };
        manifest.check()?;
        Ok(manifest)
    }

    fn check(&self) -> Result<(), HostError> {
        let bad = |what: &str| HostError::invalid(format!("the legal manifest {what}"));
        for (name, value) in [
            ("version", &self.version),
            ("documents hash", &self.documents_hash),
        ] {
            if value.trim().is_empty()
                || value.len() > MAX_FIELD_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(bad(&format!("has no usable {name}")));
            }
        }
        if self.documents.len() > MAX_DOCUMENTS {
            return Err(bad("lists too many documents"));
        }
        if self
            .documents
            .iter()
            .flat_map(DeclaredDocument::fields)
            .any(|f| f.len() > MAX_FIELD_BYTES)
        {
            return Err(bad("holds an oversized document field"));
        }
        Ok(())
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn documents_hash(&self) -> &str {
        &self.documents_hash
    }
}

/// The declaration the renderer sends when the user agrees: no time, closed to unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateRequest {
    pub version: String,
    pub documents_hash: String,
    pub documents: Vec<DeclaredDocument>,
    pub eligibility_basis: EligibilityBasis,
    pub surface: Surface,
}

/// The stored and returned receipt: exactly the renderer's `LegalAcceptanceRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LegalRecord {
    pub version: String,
    pub documents_hash: String,
    /// Native UTC time, `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    pub accepted_at: String,
    pub eligibility_basis: EligibilityBasis,
}

impl LegalRecord {
    /// A record a store may hold or the service may return: non-empty text and canonical UTC.
    fn is_well_formed(&self) -> bool {
        !self.version.trim().is_empty()
            && !self.documents_hash.trim().is_empty()
            && parse_canonical_utc(&self.accepted_at).is_some()
    }
}

// ---- store ----------------------------------------------------------------------------------

/// Where the receipt lives. Errors carry no path and no value.
pub trait LegalReceiptStore: Send + Sync {
    /// The stored receipt; `None` when there is none, it was reset, or it is not a well-formed record.
    fn load(&self) -> Result<Option<LegalRecord>, HostError>;
    fn save(&self, record: &LegalRecord) -> Result<(), HostError>;
    fn clear(&self) -> Result<(), HostError>;
    /// Store `build()`'s record, only when nothing has ever been stored, then return what
    /// [`LegalReceiptStore::load`] now finds. The one-time import of a legacy receipt.
    fn import_once(
        &self,
        build: &mut dyn FnMut() -> Option<LegalRecord>,
    ) -> Result<Option<LegalRecord>, HostError>;
}

fn store_failed(_: StoreError) -> HostError {
    HostError::new(
        "legal_store_failed",
        "the legal receipt could not be stored",
    )
}

/// The receipt in an app-settings [`Document`] of its own: the document IS the record, and a reset
/// leaves an empty document (so a legacy receipt is never imported twice).
pub struct DocumentReceiptStore {
    document: Document,
}

impl DocumentReceiptStore {
    pub fn new(document: Document) -> Self {
        Self { document }
    }
}

fn record_of(value: Value) -> Option<LegalRecord> {
    serde_json::from_value::<LegalRecord>(value)
        .ok()
        .filter(LegalRecord::is_well_formed)
}

impl LegalReceiptStore for DocumentReceiptStore {
    fn load(&self) -> Result<Option<LegalRecord>, HostError> {
        let Some(state) = self.document.get().map_err(store_failed)? else {
            return Ok(None);
        };
        if state.version > RECEIPT_DOCUMENT_VERSION {
            return Err(HostError::new(
                "legal_store_version",
                "the legal receipt needs a newer app version",
            ));
        }
        if state.version != RECEIPT_DOCUMENT_VERSION {
            return Ok(None);
        }
        Ok(record_of(state.value))
    }

    fn save(&self, record: &LegalRecord) -> Result<(), HostError> {
        let value = serde_json::to_value(record).map_err(|_| {
            HostError::new(
                "legal_store_failed",
                "the legal receipt could not be stored",
            )
        })?;
        self.document
            .put(RECEIPT_DOCUMENT_VERSION, value, None)
            .map(|_| ())
            .map_err(store_failed)
    }

    fn clear(&self) -> Result<(), HostError> {
        self.document
            .put(RECEIPT_DOCUMENT_VERSION, json!({}), None)
            .map(|_| ())
            .map_err(store_failed)
    }

    fn import_once(
        &self,
        build: &mut dyn FnMut() -> Option<LegalRecord>,
    ) -> Result<Option<LegalRecord>, HostError> {
        self.document
            .import_if_missing(RECEIPT_DOCUMENT_VERSION, || {
                Ok(build().and_then(|r| serde_json::to_value(r).ok()))
            })
            .map_err(store_failed)?;
        self.load()
    }
}

// ---- service --------------------------------------------------------------------------------

/// A legacy receipt, read from wherever the app used to keep it and mapped to a [`LegalRecord`].
pub type LegacyImporter = Arc<dyn Fn() -> Option<LegalRecord> + Send + Sync>;

/// The native legal receipt service. Register it with
/// `HostServicesBuilder::service(ServiceId::LEGAL, trust, service)` ([`HostServicesBuilder::legal`]).
///
/// [`HostServicesBuilder::legal`]: super::HostServicesBuilder::legal
pub struct LegalReceiptService {
    manifest: LegalManifest,
    store: Box<dyn LegalReceiptStore>,
    clock: Arc<dyn LegalClock>,
    legacy: Option<LegacyImporter>,
    import_tried: AtomicBool,
    /// One writer at a time: a `create` and a `reset` never interleave.
    gate: Mutex<()>,
}

impl LegalReceiptService {
    /// A service over `store` that stamps system time.
    pub fn new(manifest: LegalManifest, store: impl LegalReceiptStore + 'static) -> Self {
        Self {
            manifest,
            store: Box::new(store),
            clock: Arc::new(SystemLegalClock),
            legacy: None,
            import_tried: AtomicBool::new(false),
            gate: Mutex::new(()),
        }
    }

    /// Replace the clock (tests, or a host with a trusted time source).
    pub fn with_clock(mut self, clock: impl LegalClock + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Import a receipt kept elsewhere, once, on the first `read` that finds none.
    pub fn with_legacy_import(
        mut self,
        importer: impl Fn() -> Option<LegalRecord> + Send + Sync + 'static,
    ) -> Self {
        self.legacy = Some(Arc::new(importer));
        self
    }

    pub fn manifest(&self) -> &LegalManifest {
        &self.manifest
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current manifest's stored receipt, or `None`. Historical receipts stay stored until
    /// re-acceptance replaces them, but cannot satisfy a changed native version or digest.
    pub fn read(&self) -> Result<Option<LegalRecord>, HostError> {
        let _gate = self.lock();
        if let Some(record) = self.store.load()? {
            // Native policy remains authoritative even if the renderer omits its requirements.
            // Preserve historical receipts in storage, but never expose one as current approval.
            return Ok(self.matches_manifest(&record).then_some(record));
        }
        let Some(importer) = &self.legacy else {
            return Ok(None);
        };
        if self.import_tried.swap(true, Ordering::SeqCst) {
            return Ok(None);
        }
        let manifest = &self.manifest;
        let imported = self.store.import_once(&mut || {
            importer().filter(|r| {
                r.is_well_formed()
                    && r.version == manifest.version
                    && r.documents_hash == manifest.documents_hash
            })
        })?;
        Ok(imported.filter(|record| self.matches_manifest(record)))
    }

    fn matches_manifest(&self, record: &LegalRecord) -> bool {
        record.is_well_formed()
            && record.version == self.manifest.version
            && record.documents_hash == self.manifest.documents_hash
    }

    /// Check the declaration against the manifest, stamp native time, store and return the record.
    pub fn create(&self, request: CreateRequest) -> Result<LegalRecord, HostError> {
        if request.version != self.manifest.version {
            return Err(HostError::new(
                "legal_version_mismatch",
                "the agreement version shown is not the version this app requires",
            ));
        }
        if request.documents_hash != self.manifest.documents_hash {
            return Err(HostError::new(
                "legal_hash_mismatch",
                "the documents shown are not the documents this app requires",
            ));
        }
        if request.documents != self.manifest.documents {
            return Err(HostError::new(
                "legal_documents_mismatch",
                "the document list shown is not the list this app requires",
            ));
        }
        let now = self.clock.now_millis();
        let accepted_at = (now >= MIN_PLAUSIBLE_MILLIS)
            .then(|| format_utc_millis(now))
            .flatten()
            .ok_or_else(|| {
                HostError::new(
                    "legal_clock_invalid",
                    "the device clock is not set, so the agreement cannot be recorded",
                )
            })?;
        let record = LegalRecord {
            version: self.manifest.version.clone(),
            documents_hash: self.manifest.documents_hash.clone(),
            accepted_at,
            eligibility_basis: request.eligibility_basis,
        };
        let _gate = self.lock();
        self.store.save(&record)?;
        // The service answers with what it can read back, not with what it meant to write.
        match self.store.load()? {
            Some(stored) if stored == record => Ok(stored),
            _ => Err(HostError::new(
                "legal_store_failed",
                "the legal receipt could not be stored",
            )),
        }
    }

    /// Forget the receipt. The next `read` finds none, and a legacy receipt is not imported again.
    pub fn reset(&self) -> Result<(), HostError> {
        let _gate = self.lock();
        // A reset must also stop a legacy import from bringing a record back.
        self.import_tried.store(true, Ordering::SeqCst);
        self.store.clear()?;
        if self.store.load()?.is_some() {
            return Err(HostError::new(
                "legal_store_failed",
                "the legal receipt could not be cleared",
            ));
        }
        Ok(())
    }
}

// ---- commands -------------------------------------------------------------------------------

fn legal_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<LegalReceiptService>, HostError> {
    let app = webview.app_handle();
    let host = super::managed(app)?;
    let owned = caller_gate::gather(webview)?;
    legal_for(&host, action, &owned.facts())
}

/// The legal receipt service `facts` may use for `action` (`read`, `create`, `reset`); the first
/// call of every legal command.
pub fn legal_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<LegalReceiptService>, HostError> {
    host.service_for::<LegalReceiptService>(ServiceId::LEGAL, action, facts)
}

/// The stored receipt, or `null`.
#[tauri::command]
pub async fn rightkit_legal_receipt_read(
    webview: Webview<Wry>,
) -> Result<Option<LegalRecord>, HostError> {
    let service = legal_service(&webview, "read")?;
    blocking(move || service.read()).await
}

/// Record the agreement the user just gave. `request` is the declaration (no time); the service
/// compares it with the manifest, stamps native UTC time and answers with the stored record.
#[tauri::command]
pub async fn rightkit_legal_receipt_create(
    webview: Webview<Wry>,
    request: Value,
) -> Result<LegalRecord, HostError> {
    let service = legal_service(&webview, "create")?;
    // Parsed after the caller check, so a refused caller learns nothing about the shape.
    let request: CreateRequest = serde_json::from_value(request)
        .map_err(|_| HostError::invalid("the legal declaration is not valid"))?;
    blocking(move || service.create(request)).await
}

/// Forget the receipt.
#[tauri::command]
pub async fn rightkit_legal_receipt_reset(webview: Webview<Wry>) -> Result<(), HostError> {
    let service = legal_service(&webview, "reset")?;
    blocking(move || service.reset()).await
}
