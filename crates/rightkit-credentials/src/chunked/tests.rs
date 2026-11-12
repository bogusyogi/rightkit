//! Chunking against a store that keeps what Credential Manager would keep: the blob the Windows
//! backend's codec produces for the namespace (`keyring_compat` for a keyring-compatible one),
//! refused over 2,560 bytes. Runs on every host, so the layout is proved without Windows.

use super::*;
use crate::keyring_compat;
use crate::WindowsTarget;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`.
const BLOB_MAX: usize = 2_560;

/// Blobs by Windows target name, encoded and decoded as `windows.rs` does.
#[derive(Default)]
struct BlobStore {
    blobs: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl BlobStore {
    fn blob(&self, ns: &Namespace, account: &str) -> Option<Vec<u8>> {
        let name = ns.windows_target_name(account);
        self.blobs.lock().unwrap().get(&name).cloned()
    }
    fn names(&self) -> Vec<String> {
        self.blobs.lock().unwrap().keys().cloned().collect()
    }
    fn encode(ns: &Namespace, secret: &Secret) -> Result<Vec<u8>> {
        let blob = match ns.blob_codec() {
            BlobCodec::Raw => secret.expose_bytes().to_vec(),
            BlobCodec::KeyringUtf16 => keyring_compat::encode(secret.expose_bytes())?,
            BlobCodec::LegacyUtf8 => {
                return Err(Error::Invalid {
                    what: "windows target",
                    reason: "read only".into(),
                })
            }
        };
        if blob.len() > BLOB_MAX {
            // What CredWriteW answers for an oversized blob.
            return Err(Error::Backend {
                op: "set",
                code: 1783,
            });
        }
        Ok(blob)
    }
}

impl Backend for BlobStore {
    fn name(&self) -> &'static str {
        "blob-store"
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        match self.blob(ns, account) {
            None => Ok(None),
            Some(blob) => match ns.blob_codec() {
                BlobCodec::KeyringUtf16 => keyring_compat::decode(&blob)
                    .map(Secret::from_bytes)
                    .map(Some),
                BlobCodec::Raw | BlobCodec::LegacyUtf8 => Ok(Some(Secret::from_bytes(blob))),
            },
        }
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        Ok(self.blob(ns, account).is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        let blob = Self::encode(ns, secret)?;
        let name = ns.windows_target_name(account);
        self.blobs.lock().unwrap().insert(name, blob);
        Ok(())
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        if self.exists(ns, account, mode)? {
            return Ok(false);
        }
        self.set(ns, account, secret, mode).map(|()| true)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        let name = ns.windows_target_name(account);
        Ok(self.blobs.lock().unwrap().remove(&name).is_some())
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        mode: Interaction,
    ) -> Result<ConditionalDelete> {
        match self.get(ns, account, mode)? {
            None => Ok(ConditionalDelete::Absent),
            Some(found) if found.ct_eq(expected) => {
                self.delete(ns, account, mode)?;
                Ok(ConditionalDelete::Deleted)
            }
            Some(_) => Ok(ConditionalDelete::Changed),
        }
    }
}

const MODE: Interaction = Interaction::NonInteractive;
const ACCOUNT: &str = "license";

fn keyring_ns() -> Namespace {
    Namespace::new("app.example.test")
        .unwrap()
        .windows_target(WindowsTarget::KeyringCompat)
}

fn raw_ns() -> Namespace {
    Namespace::new("app.example.test").unwrap()
}

fn stores() -> (Arc<BlobStore>, ChunkedBackend) {
    let inner = Arc::new(BlobStore::default());
    let chunked = ChunkedBackend::new(inner.clone());
    (inner, chunked)
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Stores `text`, reads it back byte for byte and returns how many native items hold it. Every
/// stored blob must be valid for the namespace's codec and within the blob limit.
fn round_trip(ns: &Namespace, text: &str) -> usize {
    let (inner, chunked) = stores();
    chunked
        .set(ns, ACCOUNT, &Secret::from_string(text), MODE)
        .unwrap();
    let read = chunked.get(ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), text.as_bytes());
    let blobs = inner.blobs.lock().unwrap();
    for blob in blobs.values() {
        assert!(blob.len() <= BLOB_MAX);
        if ns.blob_codec() == BlobCodec::KeyringUtf16 {
            keyring_compat::decode(blob).unwrap();
        }
    }
    blobs.len()
}

#[test]
fn keyring_text_up_to_the_blob_limit_is_one_item_keyring_reads() {
    let ns = keyring_ns();
    for units in [1, 1_200, 1_201, 1_280] {
        let text = "a".repeat(units);
        let (inner, chunked) = stores();
        chunked
            .set(&ns, ACCOUNT, &Secret::from_string(text.as_str()), MODE)
            .unwrap();
        assert_eq!(inner.names(), vec![ns.windows_target_name(ACCOUNT)]);
        // The blob is the text as UTF-16LE with no terminator: the `keyring` crate's layout.
        assert_eq!(inner.blob(&ns, ACCOUNT).unwrap(), utf16le(&text));
        let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
        assert_eq!(read.expose_bytes(), text.as_bytes(), "{units} units");
    }
}

#[test]
fn keyring_text_over_the_blob_limit_is_text_chunks_behind_a_text_manifest() {
    let ns = keyring_ns();
    let text = "a".repeat(1_281);
    let (inner, chunked) = stores();
    chunked
        .set(&ns, ACCOUNT, &Secret::from_string(text.as_str()), MODE)
        .unwrap();
    assert_eq!(inner.names().len(), 3, "manifest and two chunks");
    let manifest = keyring_compat::decode(&inner.blob(&ns, ACCOUNT).unwrap()).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    let fields: Vec<&str> = manifest.split(' ').collect();
    assert_eq!(fields.len(), 4);
    assert_eq!((fields[0], fields[2], fields[3]), ("RKCHUNK2", "2", "1281"));
    let generation = u64::from_str_radix(fields[1], 16).unwrap();
    let first = inner
        .blob(&ns, &chunk_account(ACCOUNT, generation, 0))
        .unwrap();
    let second = inner
        .blob(&ns, &chunk_account(ACCOUNT, generation, 1))
        .unwrap();
    assert_eq!(first, utf16le(&"a".repeat(1_280)));
    assert_eq!(second, utf16le("a"));
    let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), text.as_bytes());
}

#[test]
fn keyring_text_with_wide_characters_round_trips_at_every_boundary() {
    let ns = keyring_ns();
    // One, two, three and four bytes of UTF-8; the last is two UTF-16 units.
    for lead in ["a", "\u{e9}", "\u{20ac}", "\u{1f600}"] {
        for fill in ["a", "\u{e9}", "\u{20ac}", "\u{1f600}"] {
            for count in [
                1_198, 1_199, 1_200, 1_201, 1_278, 1_279, 1_280, 1_281, 2_559, 2_561,
            ] {
                let lead_units = lead.encode_utf16().count();
                let fill_units = fill.encode_utf16().count();
                let text = format!("{lead}{}", fill.repeat(count / fill_units));
                let units = lead_units + (count / fill_units) * fill_units;
                let items = round_trip(&ns, &text);
                if units <= KEYRING_INLINE_UNITS {
                    assert_eq!(items, 1, "{units} units stay inline");
                } else {
                    assert!(items >= 3, "{units} units are chunked");
                }
            }
        }
    }
}

#[test]
fn a_two_unit_character_at_the_chunk_edge_moves_whole_into_the_next_chunk() {
    let ns = keyring_ns();
    // Units 1,280 and 1,281 are one character: the first chunk stops at 1,279.
    let text = format!("{}\u{1f600}tail", "a".repeat(1_279));
    let (inner, chunked) = stores();
    chunked
        .set(&ns, ACCOUNT, &Secret::from_string(text.as_str()), MODE)
        .unwrap();
    let manifest = chunked.read_raw(&ns, ACCOUNT, MODE).unwrap().unwrap().1;
    let manifest = manifest.unwrap();
    assert_eq!(manifest.count, 2);
    let first = inner
        .blob(&ns, &chunk_account(ACCOUNT, manifest.generation, 0))
        .unwrap();
    assert_eq!(first, utf16le(&"a".repeat(1_279)));
    let second = inner
        .blob(&ns, &chunk_account(ACCOUNT, manifest.generation, 1))
        .unwrap();
    assert_eq!(second, utf16le("\u{1f600}tail"));
    let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), text.as_bytes());
}

#[test]
fn a_character_across_byte_1200_no_longer_breaks_a_keyring_secret() {
    let ns = keyring_ns();
    // Byte 1,200 falls inside the three-byte character; 1,203 bytes are 1,201 units.
    let text = format!("{}\u{20ac}aa", "a".repeat(1_198));
    assert_eq!(text.len(), 1_203);
    assert_eq!(round_trip(&ns, &text), 1);
}

#[test]
fn keyring_items_stored_before_are_read_byte_for_byte() {
    let ns = keyring_ns();
    let (inner, chunked) = stores();
    // Inline, any size the store accepted (written by `keyring` or an earlier version).
    for units in [32, 1_200, 1_250, 1_280] {
        let text = format!("{}\u{e9}", "k".repeat(units - 1));
        inner
            .set(&ns, ACCOUNT, &Secret::from_string(text.as_str()), MODE)
            .unwrap();
        let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
        assert_eq!(read.expose_bytes(), text.as_bytes());
    }
    // The binary form, which an earlier version could store here when the random generation and
    // every 1,200-byte chunk happened to be text.
    let text = "b".repeat(2_400);
    let manifest = Manifest {
        form: Form::Binary,
        generation: 0x0102_0304_0506_0708,
        count: 2,
        total: 2_400,
    };
    for (i, part) in text.as_bytes().chunks(INLINE_MAX).enumerate() {
        let name = chunk_account(ACCOUNT, manifest.generation, i as u32);
        inner
            .set(&ns, &name, &Secret::from_bytes(part.to_vec()), MODE)
            .unwrap();
    }
    inner.set(&ns, ACCOUNT, &manifest.encode(), MODE).unwrap();
    let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), text.as_bytes());
    // Replacing it removes the old chunks and leaves one inline item.
    chunked
        .set(&ns, ACCOUNT, &Secret::from_string("short"), MODE)
        .unwrap();
    assert_eq!(inner.names(), vec![ns.windows_target_name(ACCOUNT)]);
}

#[test]
fn keyring_secret_that_begins_with_a_marker_round_trips() {
    let ns = keyring_ns();
    for text in ["RKCHUNK1 is not a manifest", "RKCHUNK2 0 1 5", "RKCHUNK2"] {
        assert_eq!(round_trip(&ns, text), 2, "manifest and one chunk");
    }
}

#[test]
fn keyring_secret_that_is_not_text_is_refused_and_leaves_nothing() {
    let ns = keyring_ns();
    for len in [16, 1_201, 4_000] {
        let (inner, chunked) = stores();
        let mut bytes = vec![b'a'; len];
        bytes[len - 1] = 0xff;
        let outcome = chunked.set(&ns, ACCOUNT, &Secret::from_bytes(bytes), MODE);
        assert_eq!(outcome, Err(Error::NotUtf8));
        assert!(inner.names().is_empty());
    }
}

#[test]
fn replace_delete_and_conditional_delete_remove_text_chunks() {
    let ns = keyring_ns();
    let long = "x".repeat(4_000);
    let (inner, chunked) = stores();
    let secret = Secret::from_string(long.as_str());
    chunked.set(&ns, ACCOUNT, &secret, MODE).unwrap();
    assert_eq!(inner.names().len(), 5);
    // A second generation replaces the first; the first one's chunks are gone.
    chunked.set(&ns, ACCOUNT, &secret, MODE).unwrap();
    assert_eq!(inner.names().len(), 5);
    assert!(!chunked.add_if_absent(&ns, ACCOUNT, &secret, MODE).unwrap());
    assert_eq!(inner.names().len(), 5);
    let other = Secret::from_string("other");
    assert_eq!(
        chunked.delete_if_equals(&ns, ACCOUNT, &other, MODE),
        Ok(ConditionalDelete::Changed)
    );
    assert_eq!(
        chunked.delete_if_equals(&ns, ACCOUNT, &secret, MODE),
        Ok(ConditionalDelete::Deleted)
    );
    assert!(inner.names().is_empty());
    assert!(chunked.add_if_absent(&ns, ACCOUNT, &secret, MODE).unwrap());
    assert!(chunked.delete(&ns, ACCOUNT, MODE).unwrap());
    assert!(inner.names().is_empty());
}

#[test]
fn a_damaged_text_value_is_an_error_never_absent_or_partial() {
    let ns = keyring_ns();
    let text = "y".repeat(3_000);
    let (inner, chunked) = stores();
    chunked
        .set(&ns, ACCOUNT, &Secret::from_string(text.as_str()), MODE)
        .unwrap();
    let manifest = chunked.read_raw(&ns, ACCOUNT, MODE).unwrap().unwrap().1;
    let generation = manifest.unwrap().generation;
    let last = chunk_account(ACCOUNT, generation, 2);
    // A chunk cut short.
    inner
        .set(&ns, &last, &Secret::from_string("y"), MODE)
        .unwrap();
    assert!(chunked.get(&ns, ACCOUNT, MODE).is_err());
    // A chunk missing.
    inner.delete(&ns, &last, MODE).unwrap();
    assert!(chunked.get(&ns, ACCOUNT, MODE).is_err());
    // A manifest that is not the exact text this module writes.
    for bad in [
        "RKCHUNK2 0000000000000001 +1 5",
        "RKCHUNK2 1 1 5",
        "RKCHUNK2 0000000000000001 0 0",
        "RKCHUNK2 0000000000000001 2 5",
        "RKCHUNK2 0000000000000001 1 5 ",
        "RKCHUNK2x",
    ] {
        inner
            .set(&ns, ACCOUNT, &Secret::from_string(bad), MODE)
            .unwrap();
        assert!(chunked.get(&ns, ACCOUNT, MODE).is_err(), "{bad}");
    }
}

#[test]
fn raw_namespaces_keep_the_binary_form_and_the_1200_byte_limit() {
    let ns = raw_ns();
    let (inner, chunked) = stores();
    let inline = vec![0xfe_u8; INLINE_MAX];
    chunked
        .set(&ns, ACCOUNT, &Secret::from_bytes(inline.clone()), MODE)
        .unwrap();
    assert_eq!(inner.blob(&ns, ACCOUNT).unwrap(), inline);
    assert_eq!(inner.names().len(), 1);
    let long = vec![0xfe_u8; INLINE_MAX + 1];
    chunked
        .set(&ns, ACCOUNT, &Secret::from_bytes(long.clone()), MODE)
        .unwrap();
    assert_eq!(inner.names().len(), 3);
    let manifest = inner.blob(&ns, ACCOUNT).unwrap();
    assert_eq!(manifest.len(), MANIFEST_LEN);
    assert!(manifest.starts_with(MAGIC));
    let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), long.as_slice());
    // The text marker means nothing outside a keyring-compatible namespace.
    chunked
        .set(&ns, ACCOUNT, &Secret::from_string("RKCHUNK2 x"), MODE)
        .unwrap();
    assert_eq!(inner.names().len(), 1);
    assert_eq!(inner.blob(&ns, ACCOUNT).unwrap(), b"RKCHUNK2 x");
    let read = chunked.get(&ns, ACCOUNT, MODE).unwrap().unwrap();
    assert_eq!(read.expose_bytes(), b"RKCHUNK2 x");
}
