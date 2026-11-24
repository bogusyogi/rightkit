//! What syncs, and one wire form for it (ME-4).
//!
//! The engine syncs one item type per engine, per zone. The agent's data is several kinds of item (memories,
//! profile entries, the Inbox, facts, schedules, standing instructions and cursors), each kept by a crate
//! this crate must not depend on. So the wire form is generic and the families are a closed list:
//!
//! * [`Family`] names the only things that sync. Google tokens, secrets and repository text have no family,
//!   so no engine can be built for them from this module.
//! * [`FamilyItem`] is the one item: an id, the two times the engine orders by, the device it came from, a
//!   schema number and an opaque body. The body travels byte for byte (a `Bytes` field), so a memory's text
//!   is never re-encoded or normalised on the way.
//! * [`FamilyMapper`] is the [`RecordMapper`] for one family. A host implements [`LocalStore`] for its own
//!   store over [`FamilyItem`], turning each of its rows into a body (JSON, usually) and back.
//!
//! Conflicts are the engine's: last writer wins on `updatedAt`, a tombstone beats any live copy and reaches
//! every copy. "A memory's text never changes after it is written; its state does" is the producer's rule:
//! the host bumps `updated_at_ms` only for a state change, and a body that would change a memory's text is a
//! new memory with a new id.
//!
//! Run every family behind [`crate::cipher::EncryptingBackend`] (the provider then holds ciphertext only),
//! one zone per family ([`Family::zone`], also the zone name to derive keys for), and with
//! [`sync_config`] (consent required when the account or key changes).
//!
//! [`LocalStore`]: crate::LocalStore

use crate::engine::SyncConfig;
use crate::record::{field, FieldValue, RawRecord};
use crate::traits::{ItemHeader, RecordMapper};
use ring::digest;
use std::fmt::Write as _;

/// Largest body, in bytes. A larger row is the host's to split or to leave out.
pub const MAX_BODY_BYTES: usize = 1 << 20;
/// Longest item id, in bytes.
pub const MAX_ID_BYTES: usize = 128;
/// The field that holds the body's schema number.
pub const SCHEMA_FIELD: &str = "schema";
/// The field that holds the body.
pub const BODY_FIELD: &str = "body";
const ORIGIN_FIELD: &str = "origin";

/// The things that sync. The list is closed on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Memory,
    Profile,
    Inbox,
    Facts,
    Schedules,
    Instructions,
    /// Cursors the agent keeps (for example how far each mailbox was read).
    Cursors,
}

impl Family {
    pub const ALL: [Family; 7] = [
        Family::Memory,
        Family::Profile,
        Family::Inbox,
        Family::Facts,
        Family::Schedules,
        Family::Instructions,
        Family::Cursors,
    ];

    /// The zone of this family: one backend zone, one ledger and one key derivation each.
    pub fn zone(self) -> &'static str {
        match self {
            Family::Memory => "rk-memory",
            Family::Profile => "rk-profile",
            Family::Inbox => "rk-inbox",
            Family::Facts => "rk-facts",
            Family::Schedules => "rk-schedules",
            Family::Instructions => "rk-instructions",
            Family::Cursors => "rk-cursors",
        }
    }

    /// The record type of this family's items (alphanumeric, so CloudKit accepts it).
    pub fn record_type(self) -> &'static str {
        match self {
            Family::Memory => "RkMemory",
            Family::Profile => "RkProfile",
            Family::Inbox => "RkInbox",
            Family::Facts => "RkFacts",
            Family::Schedules => "RkSchedules",
            Family::Instructions => "RkInstructions",
            Family::Cursors => "RkCursors",
        }
    }
}

/// One syncable item of any family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyItem {
    id: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub origin_device: Option<String>,
    /// The number of the body's layout, so a reader can tell a body it does not know.
    pub schema: u32,
    pub body: Vec<u8>,
}

impl FamilyItem {
    /// `id` is 1 to [`MAX_ID_BYTES`] bytes of `A-Z a-z 0-9 . _ : -` (a record name every backend takes; use
    /// [`item_id`] to make one from a row's own key). `body` is at most [`MAX_BODY_BYTES`].
    pub fn new(
        id: &str,
        created_at_ms: i64,
        updated_at_ms: i64,
        origin_device: Option<String>,
        schema: u32,
        body: Vec<u8>,
    ) -> Result<FamilyItem, String> {
        let id_ok = !id.is_empty()
            && id.len() <= MAX_ID_BYTES
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        if !id_ok {
            return Err(
                "item id is empty, too long, or has a character outside A-Za-z0-9._:-".into(),
            );
        }
        if body.len() > MAX_BODY_BYTES {
            return Err("item body is larger than the cap".into());
        }
        Ok(FamilyItem {
            id: id.to_owned(),
            created_at_ms,
            updated_at_ms,
            origin_device,
            schema,
            body,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// A stable id for a row: the lowercase hex SHA-256 of the length-prefixed `parts`, 64 characters, so a row
/// key with a slash or any other character can still be a record name, and two devices derive the same id.
pub fn item_id(parts: &[&str]) -> String {
    let mut context = digest::Context::new(&digest::SHA256);
    for part in parts {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part.as_bytes());
    }
    let mut out = String::with_capacity(64);
    for byte in context.finish().as_ref() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The mapper for one family.
#[derive(Debug, Clone, Copy)]
pub struct FamilyMapper {
    family: Family,
}

impl FamilyMapper {
    pub fn new(family: Family) -> Self {
        Self { family }
    }

    pub fn family(&self) -> Family {
        self.family
    }
}

impl RecordMapper for FamilyMapper {
    type Item = FamilyItem;

    fn record_type(&self) -> &str {
        self.family.record_type()
    }

    fn header(&self, item: &FamilyItem) -> ItemHeader {
        ItemHeader {
            id: item.id.clone(),
            created_at_ms: item.created_at_ms,
            updated_at_ms: item.updated_at_ms,
            origin_device: item.origin_device.clone(),
        }
    }

    fn encode(&self, item: &FamilyItem, record: &mut RawRecord) {
        record.set(SCHEMA_FIELD, FieldValue::I64(i64::from(item.schema)));
        record.set(BODY_FIELD, FieldValue::Bytes(item.body.clone()));
        record.set_opt(
            ORIGIN_FIELD,
            item.origin_device.clone().map(FieldValue::Text),
        );
    }

    fn decode(&self, record: &RawRecord) -> Result<FamilyItem, String> {
        let id = record
            .text(field::ID)
            .unwrap_or_else(|| record.record_name.clone());
        let fallback = record.modified_ms.or(record.created_ms);
        let created = record
            .date_ms(field::CREATED_AT)
            .or(fallback)
            .ok_or_else(|| "missing createdAt".to_string())?;
        let updated = record
            .date_ms(field::UPDATED_AT)
            .or(fallback)
            .ok_or_else(|| "missing updatedAt".to_string())?;
        let schema = match record.get(SCHEMA_FIELD) {
            Some(FieldValue::I64(value)) => {
                u32::try_from(*value).map_err(|_| "schema out of range".to_string())?
            }
            _ => return Err("missing schema".to_string()),
        };
        let body = match record.get(BODY_FIELD) {
            Some(FieldValue::Bytes(bytes)) => bytes.clone(),
            Some(FieldValue::Text(text)) => text.clone().into_bytes(),
            _ => return Err("missing body".to_string()),
        };
        FamilyItem::new(
            &id,
            created,
            updated,
            record.text(ORIGIN_FIELD),
            schema,
            body,
        )
    }
}

/// The engine settings for a family: the defaults, with consent required when the account or the key
/// changes, which every encrypting provider needs (`http::sync_config` does the same under the `http` feature).
pub fn sync_config() -> SyncConfig {
    SyncConfig {
        require_consent_on_scope_change: true,
        ..SyncConfig::default()
    }
}
