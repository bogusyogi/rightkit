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
use tauri::{Webview, Wry};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{HostServices, ServiceTrust};
    use crate::nav::NavPolicy;
    use std::sync::atomic::AtomicI64;

    struct FixedClock(AtomicI64);

    impl FixedClock {
        fn at(ms: i64) -> Arc<Self> {
            Arc::new(Self(AtomicI64::new(ms)))
        }
    }

    impl LegalClock for Arc<FixedClock> {
        fn now_millis(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// 2025-03-04T05:06:07.089Z
    const NOW: i64 = 1_741_064_767_089;
    const NOW_TEXT: &str = "2025-03-04T05:06:07.089Z";

    fn doc(id: &str, title: &str) -> DeclaredDocument {
        DeclaredDocument {
            id: Some(id.into()),
            title: Some(title.into()),
            url: Some(format!("https://example.com/{id}")),
            material: Some(true),
            ..DeclaredDocument::default()
        }
    }

    fn manifest() -> LegalManifest {
        LegalManifest::new(
            "2",
            "hash-2",
            vec![doc("terms", "Terms"), doc("privacy", "Privacy")],
        )
        .unwrap()
    }

    /// The renderer's declaration, as JSON, the way `createNativeLegalReceiptPort` sends it.
    fn declaration() -> Value {
        json!({
            "version": "2",
            "documentsHash": "hash-2",
            "documents": [
                {"id": "terms", "title": "Terms", "url": "https://example.com/terms", "material": true},
                {"id": "privacy", "title": "Privacy", "url": "https://example.com/privacy", "material": true},
            ],
            "eligibilityBasis": "individual",
            "surface": "first_run",
        })
    }

    fn request(value: Value) -> Result<CreateRequest, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// In-memory store with the document semantics: a stored value, even an empty one, stops an import.
    #[derive(Default)]
    struct MemoryStore {
        cell: Mutex<Option<Option<LegalRecord>>>,
        fail_save: AtomicBool,
        ignore_clear: AtomicBool,
    }

    impl LegalReceiptStore for Arc<MemoryStore> {
        fn load(&self) -> Result<Option<LegalRecord>, HostError> {
            Ok(self.cell.lock().unwrap().clone().flatten())
        }
        fn save(&self, record: &LegalRecord) -> Result<(), HostError> {
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(HostError::new("legal_store_failed", "no"));
            }
            *self.cell.lock().unwrap() = Some(Some(record.clone()));
            Ok(())
        }
        fn clear(&self) -> Result<(), HostError> {
            if !self.ignore_clear.load(Ordering::SeqCst) {
                *self.cell.lock().unwrap() = Some(None);
            }
            Ok(())
        }
        fn import_once(
            &self,
            build: &mut dyn FnMut() -> Option<LegalRecord>,
        ) -> Result<Option<LegalRecord>, HostError> {
            let mut cell = self.cell.lock().unwrap();
            if cell.is_none() {
                if let Some(record) = build() {
                    *cell = Some(Some(record));
                }
            }
            Ok(cell.clone().flatten())
        }
    }

    fn service(store: &Arc<MemoryStore>, clock: &Arc<FixedClock>) -> LegalReceiptService {
        LegalReceiptService::new(manifest(), Arc::clone(store)).with_clock(Arc::clone(clock))
    }

    #[test]
    fn time_formats_and_parses_like_to_iso_string() {
        for (ms, text) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (NOW, NOW_TEXT),
            (1_700_000_000_123, "2023-11-14T22:13:20.123Z"),
            (4_102_444_800_000, "2100-01-01T00:00:00.000Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
        ] {
            assert_eq!(format_utc_millis(ms).as_deref(), Some(text), "{ms}");
            assert_eq!(parse_canonical_utc(text), Some(ms), "{text}");
        }
        assert_eq!(format_utc_millis(i64::MAX), None);
        for bad in [
            "",
            "2025-03-04T05:06:07Z",
            "2025-03-04 05:06:07.089Z",
            "2025-03-04T05:06:07.089+00:00",
            "2025-02-30T05:06:07.089Z",
            "2025-13-04T05:06:07.089Z",
            "2025-03-04T24:06:07.089Z",
            "2025-03-04T05:60:07.089Z",
            "2023-02-29T00:00:00.000Z",
            "2025-03-04T05:06:07.0890Z",
            "２０２５-03-04T05:06:07.089Z",
        ] {
            assert_eq!(parse_canonical_utc(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_request_is_closed_it_carries_no_time_and_no_unknown_field() {
        assert!(request(declaration()).is_ok());
        for extra in [
            ("acceptedAt", json!("2020-01-01T00:00:00.000Z")),
            ("accepted_at", json!("2020-01-01T00:00:00.000Z")),
            ("record", json!({})),
            ("token", json!("x")),
        ] {
            let mut value = declaration();
            value[extra.0] = extra.1;
            assert!(request(value).is_err(), "{} must be refused", extra.0);
        }
        let mut value = declaration();
        value["documents"][0]["acceptedAt"] = json!("x");
        assert!(request(value).is_err(), "unknown document field");
        for (field, bad) in [
            ("eligibilityBasis", json!("minor")),
            ("surface", json!("settings")),
            ("version", json!(2)),
        ] {
            let mut value = declaration();
            value[field] = bad;
            assert!(request(value).is_err(), "{field}");
        }
        let mut missing = declaration();
        missing.as_object_mut().unwrap().remove("surface");
        assert!(request(missing).is_err());
    }

    #[test]
    fn create_stamps_native_time_stores_and_read_returns_it() {
        let store = Arc::new(MemoryStore::default());
        let clock = FixedClock::at(NOW);
        let service = service(&store, &clock);
        assert_eq!(service.read().unwrap(), None);
        let record = service.create(request(declaration()).unwrap()).unwrap();
        assert_eq!(
            record.accepted_at, NOW_TEXT,
            "the time is the service's, not the renderer's"
        );
        assert_eq!(record.version, "2");
        assert_eq!(record.documents_hash, "hash-2");
        assert_eq!(record.eligibility_basis, EligibilityBasis::Individual);
        assert_eq!(service.read().unwrap(), Some(record.clone()));
        // The wire shape is exactly the renderer's `LegalAcceptanceRecord`.
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({
                "version": "2",
                "documentsHash": "hash-2",
                "acceptedAt": NOW_TEXT,
                "eligibilityBasis": "individual",
            })
        );
        // Accepting again (after a re-open) replaces the record with a new native time.
        clock.0.store(NOW + 5000, Ordering::SeqCst);
        let again = service
            .create(
                request({
                    let mut v = declaration();
                    v["eligibilityBasis"] = json!("enterprise");
                    v
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(again.accepted_at, "2025-03-04T05:06:12.089Z");
        assert_eq!(
            service.read().unwrap().unwrap().eligibility_basis,
            EligibilityBasis::Enterprise
        );
    }

    #[test]
    fn a_declaration_that_differs_from_the_manifest_is_refused_and_stores_nothing() {
        let store = Arc::new(MemoryStore::default());
        let service = service(&store, &FixedClock::at(NOW));
        let cases: [(&str, fn(&mut Value)); 6] = [
            ("legal_version_mismatch", |v| v["version"] = json!("1")),
            ("legal_hash_mismatch", |v| {
                v["documentsHash"] = json!("hash-1")
            }),
            ("legal_documents_mismatch", |v| {
                v["documents"].as_array_mut().unwrap().pop();
            }),
            ("legal_documents_mismatch", |v| {
                v["documents"].as_array_mut().unwrap().reverse();
            }),
            ("legal_documents_mismatch", |v| {
                v["documents"][0]["url"] = json!("https://evil.example/terms");
            }),
            ("legal_documents_mismatch", |v| {
                v["documents"][1]["sha256"] = json!("00");
            }),
        ];
        for (code, edit) in cases {
            let mut value = declaration();
            edit(&mut value);
            let err = service.create(request(value).unwrap()).unwrap_err();
            assert_eq!(err.code, code);
            assert!(
                !err.message.contains("evil.example"),
                "no echo of what was sent"
            );
        }
        assert_eq!(service.read().unwrap(), None, "nothing was stored");
    }

    #[test]
    fn a_clock_that_is_not_set_is_refused_rather_than_recorded() {
        let store = Arc::new(MemoryStore::default());
        for ms in [0, -5, MIN_PLAUSIBLE_MILLIS - 1] {
            let service = service(&store, &FixedClock::at(ms));
            assert_eq!(
                service
                    .create(request(declaration()).unwrap())
                    .unwrap_err()
                    .code,
                "legal_clock_invalid"
            );
        }
        assert_eq!(
            service(&store, &FixedClock::at(MIN_PLAUSIBLE_MILLIS))
                .read()
                .unwrap(),
            None
        );
        let ok = service(&store, &FixedClock::at(MIN_PLAUSIBLE_MILLIS))
            .create(request(declaration()).unwrap())
            .unwrap();
        assert_eq!(ok.accepted_at, "2024-01-01T00:00:00.000Z");
    }

    #[test]
    fn reset_clears_and_a_failed_store_is_never_reported_as_accepted() {
        let store = Arc::new(MemoryStore::default());
        let service = service(&store, &FixedClock::at(NOW));
        service.create(request(declaration()).unwrap()).unwrap();
        service.reset().unwrap();
        assert_eq!(service.read().unwrap(), None);
        store.fail_save.store(true, Ordering::SeqCst);
        let err = service.create(request(declaration()).unwrap()).unwrap_err();
        assert_eq!(err.code, "legal_store_failed");
        assert_eq!(service.read().unwrap(), None);
    }

    fn legacy(version: &str, hash: &str, at: &str) -> LegalRecord {
        LegalRecord {
            version: version.into(),
            documents_hash: hash.into(),
            accepted_at: at.into(),
            eligibility_basis: EligibilityBasis::Enterprise,
        }
    }

    #[test]
    fn changed_version_or_digest_requires_native_reacceptance_without_erasing_history() {
        for (version, hash) in [("3", "hash-2"), ("2", "hash-3")] {
            let store = Arc::new(MemoryStore::default());
            let clock = FixedClock::at(NOW);
            let accepted = service(&store, &clock)
                .create(request(declaration()).unwrap())
                .unwrap();
            let changed = LegalManifest::new(version, hash, manifest().documents).unwrap();
            let reopened = LegalReceiptService::new(changed, Arc::clone(&store))
                .with_clock(Arc::clone(&clock));
            assert_eq!(reopened.read().unwrap(), None);
            assert_eq!(store.load().unwrap(), Some(accepted));
            let mut declaration = declaration();
            declaration["version"] = json!(version);
            declaration["documentsHash"] = json!(hash);
            let replacement = reopened.create(request(declaration).unwrap()).unwrap();
            assert_eq!(reopened.read().unwrap(), Some(replacement));
        }
    }

    #[test]
    fn reset_reports_failure_when_the_receipt_survives_clear() {
        let store = Arc::new(MemoryStore::default());
        let service = service(&store, &FixedClock::at(NOW));
        let accepted = service.create(request(declaration()).unwrap()).unwrap();
        store.ignore_clear.store(true, Ordering::SeqCst);
        assert_eq!(service.reset().unwrap_err().code, "legal_store_failed");
        assert_eq!(store.load().unwrap(), Some(accepted));
    }

    #[test]
    fn a_matching_legacy_receipt_is_imported_once_and_never_after_a_reset() {
        let store = Arc::new(MemoryStore::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let service = LegalReceiptService::new(manifest(), Arc::clone(&store))
            .with_clock(FixedClock::at(NOW))
            .with_legacy_import(move || {
                seen.fetch_add(1, Ordering::SeqCst);
                Some(legacy("2", "hash-2", "2024-06-01T10:00:00.000Z"))
            });
        let imported = service.read().unwrap().unwrap();
        assert_eq!(
            imported.accepted_at, "2024-06-01T10:00:00.000Z",
            "the legacy time is kept"
        );
        assert_eq!(service.read().unwrap(), Some(imported));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        service.reset().unwrap();
        assert_eq!(
            service.read().unwrap(),
            None,
            "a reset receipt does not come back"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_legacy_receipt_that_does_not_match_the_manifest_or_is_malformed_is_ignored() {
        for record in [
            legacy("1", "hash-2", "2024-06-01T10:00:00.000Z"),
            legacy("2", "hash-1", "2024-06-01T10:00:00.000Z"),
            legacy("2", "hash-2", "2024-06-01"),
            legacy("2", "hash-2", "2024-02-30T10:00:00.000Z"),
            legacy(" ", "hash-2", "2024-06-01T10:00:00.000Z"),
        ] {
            let store = Arc::new(MemoryStore::default());
            let service = LegalReceiptService::new(manifest(), Arc::clone(&store))
                .with_legacy_import(move || Some(record.clone()));
            assert_eq!(service.read().unwrap(), None);
            assert!(store.cell.lock().unwrap().is_none(), "nothing was stored");
        }
    }

    #[test]
    fn the_document_store_persists_validates_and_blocks_a_second_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legal-receipt.json");
        let open =
            || DocumentReceiptStore::new(Document::open_path(&path, "legal-receipt").unwrap());
        let service = LegalReceiptService::new(manifest(), open()).with_clock(FixedClock::at(NOW));
        assert_eq!(service.read().unwrap(), None);
        let record = service.create(request(declaration()).unwrap()).unwrap();
        // A new service over the same file reads it back.
        let reopened = LegalReceiptService::new(manifest(), open());
        assert_eq!(reopened.read().unwrap(), Some(record));
        reopened.reset().unwrap();
        assert_eq!(
            LegalReceiptService::new(manifest(), open()).read().unwrap(),
            None
        );
        // A legacy importer finds a document already there (the reset one), so it is not used.
        let with_legacy = LegalReceiptService::new(manifest(), open())
            .with_legacy_import(|| Some(legacy("2", "hash-2", "2024-06-01T10:00:00.000Z")));
        assert_eq!(with_legacy.read().unwrap(), None);
    }

    #[test]
    fn a_stored_value_that_is_not_a_well_formed_record_reads_as_no_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legal-receipt.json");
        let document = Document::open_path(&path, "legal-receipt").unwrap();
        for bad in [
            json!({"version": "2", "documentsHash": "hash-2", "acceptedAt": "yesterday", "eligibilityBasis": "individual"}),
            json!({"version": "2", "documentsHash": "hash-2", "acceptedAt": NOW_TEXT, "eligibilityBasis": "individual", "extra": 1}),
            json!({"version": "", "documentsHash": "hash-2", "acceptedAt": NOW_TEXT, "eligibilityBasis": "individual"}),
            json!({"version": "2"}),
            json!({"legalAccepted": true}),
            json!({}),
        ] {
            document.put(RECEIPT_DOCUMENT_VERSION, bad, None).unwrap();
            let store =
                DocumentReceiptStore::new(Document::open_path(&path, "legal-receipt").unwrap());
            assert_eq!(store.load().unwrap(), None);
        }
    }

    #[test]
    fn future_receipt_schema_is_refused_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legal-receipt.json");
        let document = Document::open_path(&path, "legal-receipt").unwrap();
        let value = serde_json::to_value(legacy("2", "hash-2", NOW_TEXT)).unwrap();
        document
            .put(RECEIPT_DOCUMENT_VERSION + 1, value.clone(), None)
            .unwrap();
        let store = DocumentReceiptStore::new(Document::open_path(&path, "legal-receipt").unwrap());
        let service = LegalReceiptService::new(manifest(), store).with_clock(FixedClock::at(NOW));
        assert_eq!(service.read().unwrap_err().code, "legal_store_version");
        assert!(service.create(request(declaration()).unwrap()).is_err());
        assert!(service.reset().is_err());
        let state = document.get().unwrap().unwrap();
        assert_eq!(state.version, RECEIPT_DOCUMENT_VERSION + 1);
        assert_eq!(state.value, value);
    }

    #[test]
    fn a_manifest_that_is_not_usable_is_refused() {
        assert!(LegalManifest::new("", "h", vec![]).is_err());
        assert!(LegalManifest::new("1", " ", vec![]).is_err());
        assert!(LegalManifest::new("1\n", "h", vec![]).is_err());
        assert!(LegalManifest::new("1", "h", vec![DeclaredDocument::default(); 65]).is_err());
        let big = DeclaredDocument {
            title: Some("x".repeat(MAX_FIELD_BYTES + 1)),
            ..DeclaredDocument::default()
        };
        assert!(LegalManifest::new("1", "h", vec![big]).is_err());
        assert!(LegalManifest::new("1", "h", vec![]).is_ok());
    }

    // ---- who may call it ----

    fn facts<'a>(
        label: &'a str,
        url: &'a tauri::Url,
        nav: &'a NavPolicy,
        content: &'a [String],
    ) -> CallerFacts<'a> {
        CallerFacts {
            label,
            is_app_window: true,
            content_labels: content,
            url: Some(url),
            nav,
        }
    }

    fn host(store: &Arc<MemoryStore>) -> HostServices {
        HostServices::builder()
            .legal(
                ServiceTrust::for_labels(["main", "onboarding"]),
                service(store, &FixedClock::at(NOW)),
            )
            .build()
            .unwrap()
    }

    #[test]
    fn a_content_webview_a_foreign_origin_and_an_untrusted_window_reach_nothing() {
        let store = Arc::new(MemoryStore::default());
        let host = host(&store);
        let nav = NavPolicy::default();
        let own = tauri::Url::parse("tauri://localhost/").unwrap();
        let foreign = tauri::Url::parse("https://evil.example/").unwrap();
        let content = vec!["viewer".to_string()];
        for action in ["read", "create", "reset"] {
            let refused = [
                // A registered content webview, even under a trusted-looking label.
                facts("viewer", &own, &nav, &content),
                // The trusted label on a page that is not the app's.
                facts("main", &foreign, &nav, &content),
                // An app window nobody trusted with the receipt.
                facts("settings", &own, &nav, &content),
            ];
            for f in &refused {
                let err = legal_for(&host, action, f).err().expect("must be refused");
                assert_eq!(err.code, "permission_denied", "{action} for {}", f.label);
            }
            assert!(legal_for(&host, action, &facts("onboarding", &own, &nav, &content)).is_ok());
        }
        assert_eq!(
            *store.cell.lock().unwrap(),
            None,
            "a refused caller touched no store"
        );
    }

    #[test]
    fn a_content_label_is_refused_even_when_the_policy_trusts_that_label() {
        let store = Arc::new(MemoryStore::default());
        let host = HostServices::builder()
            .legal(
                ServiceTrust::for_labels(["viewer"]),
                service(&store, &FixedClock::at(NOW)),
            )
            .build()
            .unwrap();
        let nav = NavPolicy::default();
        let own = tauri::Url::parse("tauri://localhost/").unwrap();
        let content = vec!["viewer".to_string()];
        assert_eq!(
            legal_for(&host, "create", &facts("viewer", &own, &nav, &content))
                .err()
                .unwrap()
                .code,
            "permission_denied"
        );
    }

    #[test]
    fn the_service_has_no_instance_without_registration_and_actions_can_be_limited() {
        let nav = NavPolicy::default();
        let own = tauri::Url::parse("tauri://localhost/").unwrap();
        let none = HostServices::builder().build().unwrap();
        assert!(legal_for(&none, "read", &facts("main", &own, &nav, &[])).is_err());
        let store = Arc::new(MemoryStore::default());
        let read_only = HostServices::builder()
            .legal(
                ServiceTrust::for_labels(["main"]).with_actions(["read"]),
                service(&store, &FixedClock::at(NOW)),
            )
            .build()
            .unwrap();
        assert!(legal_for(&read_only, "read", &facts("main", &own, &nav, &[])).is_ok());
        assert!(legal_for(&read_only, "create", &facts("main", &own, &nav, &[])).is_err());
    }
}
