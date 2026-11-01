//! The D7 caps on relayed traffic, the monthly relayed-bytes ledger and the refusal wording.
//!
//! Adrian, 2026-10-17 (D7): 25 MB per transfer, 10 MB per clipboard image or chat attachment,
//! 250 MB per circle per month, screens direct-only. A transfer on a direct path is never
//! capped here and never counted. A path that is not provably direct counts as relayed
//! (fail closed).

use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use rightkit_fs::{AtomicOptions, atomic_write_with};
use serde::{Deserialize, Serialize};

use crate::error::MeshError;

/// A megabyte in the caps below (binary: 1,048,576 bytes).
pub const MB: u64 = 1024 * 1024;
/// Relayed cap per file, document, diagnostics bundle or fetched file.
pub const RELAY_TRANSFER_MAX: u64 = 25 * MB;
/// Relayed cap per clipboard image, phone image or chat attachment.
pub const RELAY_IMAGE_MAX: u64 = 10 * MB;
/// Relayed bytes one circle may use per calendar month (UTC).
pub const RELAY_CIRCLE_MONTH: u64 = 250 * MB;
/// Largest message or sync record that travels on the small path (mailbox or live stream).
pub const SMALL_ITEM_MAX: u64 = 64 * 1024;
/// Phones downscale to this long edge before sending (`docs/connectivity-images.md`).
pub const IMAGE_MAX_EDGE_PX: u32 = 2048;
/// JPEG or WebP quality floor for screenshots with text.
pub const IMAGE_MIN_QUALITY: u8 = 80;

/// What a transfer is, which decides its relayed cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A file or batch sent to a device.
    File,
    /// A document or report for the phone.
    Document,
    /// A diagnostics bundle.
    Diagnostics,
    /// A clipboard image or screenshot.
    ClipboardImage,
    /// An image from the phone, already downscaled.
    PhoneImage,
    /// A chat attachment between machines.
    ChatAttachment,
    /// A screen view or share: never relayed.
    Screen,
}

impl Kind {
    /// The largest size the relay may carry for this kind. `0` means direct only.
    pub fn relay_cap(self) -> u64 {
        match self {
            Kind::File | Kind::Document | Kind::Diagnostics => RELAY_TRANSFER_MAX,
            Kind::ClipboardImage | Kind::PhoneImage | Kind::ChatAttachment => RELAY_IMAGE_MAX,
            Kind::Screen => 0,
        }
    }

    /// Images try a direct path first and wait briefly for one before the relayed fallback.
    pub fn prefers_direct(self) -> bool {
        matches!(self, Kind::ClipboardImage | Kind::PhoneImage)
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Document => "document",
            Kind::Diagnostics => "diagnostics bundle",
            Kind::ClipboardImage | Kind::PhoneImage => "image",
            Kind::ChatAttachment => "attachment",
            Kind::Screen => "screen",
        }
    }
}

/// How the connection currently reaches the other device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    /// The selected path is a direct IP path.
    Direct,
    /// The selected path goes through a relay.
    Relayed,
    /// No selected path is known yet; treated as relayed for every cap.
    Unknown,
}

/// The way a transfer travels once it is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Direct,
    Relayed,
}

/// Stable machine words for refusals (`docs/device-link.md`: `direct_required`, `relay_budget_spent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// Needs a direct connection (over the cap, screen, or the direct setting is off).
    DirectRequired,
    /// The circle's 250 MB for this month is spent.
    RelayBudgetSpent,
    /// Over the small-item limit; send it as a file instead.
    TooLarge,
    /// The receiving person (or their per-device rule) said no.
    Declined,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalCode::DirectRequired => "direct_required",
            RefusalCode::RelayBudgetSpent => "relay_budget_spent",
            RefusalCode::TooLarge => "too_large",
            RefusalCode::Declined => "declined",
        }
    }
}

/// A refusal with the plain sentence a person is shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: RefusalCode,
    pub message: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn megabytes(bytes: u64) -> String {
    let tenths = (bytes.saturating_mul(10) + MB / 2) / MB;
    if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// Decide whether a transfer of `size` bytes may go now, given how the devices are connected.
///
/// A direct path is always allowed. Otherwise the kind's relayed cap, the monthly budget
/// (`budget_remaining`) and the "direct connections for large transfers" setting decide.
/// The caller reserves the bytes with [`RelayBudget::reserve`] only after this returns
/// [`Route::Relayed`].
pub fn check_route(
    kind: Kind,
    size: u64,
    path: PathKind,
    budget_remaining: u64,
    direct_for_large: bool,
    relay_small_files: bool,
) -> Result<Route, Refusal> {
    if path == PathKind::Direct {
        return Ok(Route::Direct);
    }
    if kind == Kind::Screen {
        return Err(Refusal {
            code: RefusalCode::DirectRequired,
            message: "Screen view needs a direct connection.".to_string(),
        });
    }
    let cap = kind.relay_cap();
    if size > cap || !relay_small_files {
        let message = if !relay_small_files {
            format!(
                "This {} is {} MB. Relaying is switched off in Settings, so it goes only on a direct connection, such as the same Wi-Fi. Pulse will send it when the devices can connect directly.",
                kind.noun(),
                megabytes(size)
            )
        } else if direct_for_large {
            format!(
                "This {} is {} MB. A {} over {} MB goes only on a direct connection, such as the same Wi-Fi. These two devices can't connect directly right now. Pulse will send it when they can.",
                kind.noun(),
                megabytes(size),
                kind.noun(),
                megabytes(cap)
            )
        } else {
            format!(
                "This {} is {} MB, over the {} MB relay limit, and direct connections for large transfers are switched off in Settings.",
                kind.noun(),
                megabytes(size),
                megabytes(cap)
            )
        };
        return Err(Refusal {
            code: RefusalCode::DirectRequired,
            message,
        });
    }
    if size > budget_remaining {
        return Err(Refusal {
            code: RefusalCode::RelayBudgetSpent,
            message: format!(
                "This circle has used its {} MB of relayed transfers for this month. Direct transfers and messages still work. Relayed transfers resume on the 1st.",
                megabytes(RELAY_CIRCLE_MONTH)
            ),
        });
    }
    Ok(Route::Relayed)
}

/// Check a message or sync record against the small-path limit.
pub fn check_small(size: u64) -> Result<(), Refusal> {
    if size > SMALL_ITEM_MAX {
        return Err(Refusal {
            code: RefusalCode::TooLarge,
            message: format!(
                "This is {} KB. Messages and records over {} KB are sent as a file instead.",
                size.div_ceil(1024),
                SMALL_ITEM_MAX / 1024
            ),
        });
    }
    Ok(())
}

/// `YYYY-MM` (UTC) for an epoch-millisecond time.
pub fn month_key(now_ms: u64) -> String {
    let days = (now_ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}")
}

#[derive(Serialize, Deserialize)]
struct LedgerFile {
    month: String,
    bytes: u64,
}

/// Relayed bytes used this month, kept in a small file beside the registry.
///
/// Counts what THIS device relayed (sent or received). A circle of several devices spends one
/// shared 250 MB, but each device only sees its own transfers until the ledgers are reconciled
/// over sync; until then the circle total can exceed the cap by the other devices' share (see
/// `docs/relay-hetzner.md`, "Caps"). A corrupt file is an error, never "zero used".
#[derive(Debug)]
pub struct RelayBudget {
    path: PathBuf,
    month: String,
    bytes: u64,
    limit: u64,
}

impl RelayBudget {
    pub fn open(path: PathBuf, limit: u64, now_ms: u64) -> Result<Self, MeshError> {
        let month = month_key(now_ms);
        let (stored_month, bytes) = match std::fs::read(&path) {
            Ok(raw) => {
                let file: LedgerFile = serde_json::from_slice(&raw)
                    .map_err(|e| MeshError::Store(format!("relay budget file: {e}")))?;
                (file.month, file.bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (month.clone(), 0),
            Err(e) => return Err(MeshError::Store(e.to_string())),
        };
        let bytes = if stored_month == month { bytes } else { 0 };
        Ok(Self {
            path,
            month,
            bytes,
            limit,
        })
    }

    fn roll(&mut self, now_ms: u64) {
        let month = month_key(now_ms);
        if month != self.month {
            self.month = month;
            self.bytes = 0;
        }
    }

    pub fn used(&mut self, now_ms: u64) -> u64 {
        self.roll(now_ms);
        self.bytes
    }

    pub fn remaining(&mut self, now_ms: u64) -> u64 {
        self.roll(now_ms);
        self.limit.saturating_sub(self.bytes)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Count `bytes` as relayed. Refused (nothing counted) when it would pass the limit.
    pub fn reserve(&mut self, now_ms: u64, bytes: u64) -> Result<(), MeshError> {
        self.roll(now_ms);
        if self.bytes.saturating_add(bytes) > self.limit {
            return Err(MeshError::Refused(Refusal {
                code: RefusalCode::RelayBudgetSpent,
                message: "This circle has used its relayed transfers for this month.".to_string(),
            }));
        }
        self.bytes += bytes;
        self.save()
    }

    /// Give back bytes reserved for a transfer that did not complete.
    pub fn refund(&mut self, bytes: u64) -> Result<(), MeshError> {
        self.bytes = self.bytes.saturating_sub(bytes);
        self.save()
    }

    fn save(&self) -> Result<(), MeshError> {
        let raw = serde_json::to_vec(&LedgerFile {
            month: self.month.clone(),
            bytes: self.bytes,
        })
        .map_err(|e| MeshError::Store(e.to_string()))?;
        atomic_write_with(
            &self.path,
            AtomicOptions {
                create_parents: true,
                ..AtomicOptions::default()
            },
            |f| f.write_all(&raw),
        )
        .map_err(|e| MeshError::Store(e.to_string()))
    }
}
