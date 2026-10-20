//! The stored account session: one credential item, versioned like the licence envelope.
//!
//! The item text is
//! `{"rkv":1,"revision":"<random 128-bit>","state":"active","token":"..","email":"..","issuedAtUnix":N}`
//! for a session and `{"rkv":1,"revision":"..","state":"tombstone"}` for a signed-out one.
//!
//! - The active form keeps HeardRight's existing field names (`token`, `email`, `issuedAtUnix`) at
//!   the top level, next to the envelope keys, so the session an older build stored is read in
//!   place (a bare item with a `token` and no `rkv` is an active session with no revision) and an
//!   older reader that ignores unknown fields still finds its session in the new one.
//! - `revision` is fresh random text on every write. A late result of an operation that began
//!   under an older revision (a refresh or a sign-in whose answer arrives after a sign-out) is
//!   recognised by it and discarded, so a signed-out session is never brought back.
//! - A tombstone is a sign-out (or a deleted account). Nothing here ever deletes the item.
//!
//! The token is `Zeroizing`, has a redacted `Debug`, and no type here is `Serialize`; the item
//! text is built by hand. A stored text longer than [`MAX_ITEM_BYTES`], or one holding a token over
//! 16 KiB, is refused unread as corrupt.

use std::fmt;

use rightkit_credentials::Secret;
use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use super::envelope::new_revision;
use super::view::LicenseError;
use crate::record_text::{ENVELOPE_KEY, ENVELOPE_VERSION};
use crate::MAX_TOKEN_BYTES;

/// A stored session item larger than this is refused unread.
pub(crate) const MAX_ITEM_BYTES: usize = 2 * MAX_TOKEN_BYTES as usize;

/// A signed-in session.
#[derive(Clone)]
pub(crate) struct Session {
    pub token: Zeroizing<String>,
    pub email: Option<String>,
    /// When the session was issued or last confirmed, Unix seconds.
    pub issued_at: i64,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("has_email", &self.email.is_some())
            .finish_non_exhaustive()
    }
}

/// What the stored item holds.
#[derive(Clone, Debug)]
pub(crate) enum State {
    Absent,
    Active(Session),
    Tombstone,
    /// Present but not a session this crate understands.
    Corrupt,
}

/// The item as read: its revision (none for an older bare item, an absent one or a corrupt one)
/// and what it holds.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub revision: Option<String>,
    pub state: State,
}

/// The shape of a state, for comparing two reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Absent,
    Active,
    Tombstone,
    Corrupt,
}

impl Item {
    pub(crate) fn absent() -> Self {
        Self {
            revision: None,
            state: State::Absent,
        }
    }

    fn kind(&self) -> Kind {
        match self.state {
            State::Absent => Kind::Absent,
            State::Active(_) => Kind::Active,
            State::Tombstone => Kind::Tombstone,
            State::Corrupt => Kind::Corrupt,
        }
    }

    /// Whether `other` is the same stored item: the same kind under the same revision. An
    /// operation that read `self` before a server call writes its result only when a second
    /// read is still the same item.
    pub(crate) fn is_same_as(&self, other: &Item) -> bool {
        self.revision == other.revision && self.kind() == other.kind()
    }

    /// The session, when the item holds one.
    pub(crate) fn session(&self) -> Option<&Session> {
        match &self.state {
            State::Active(session) => Some(session),
            _ => None,
        }
    }

    /// Read a credential item. Never "signed out" for something unreadable.
    pub(crate) fn from_secret(secret: &Secret) -> Self {
        if secret.len() > MAX_ITEM_BYTES {
            return Self::corrupt();
        }
        match secret.expose_str() {
            Ok(text) => Self::parse(text),
            Err(_) => Self::corrupt(),
        }
    }

    fn corrupt() -> Self {
        Self {
            revision: None,
            state: State::Corrupt,
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Self::corrupt();
        };
        let Some(top) = value.as_object() else {
            return Self::corrupt();
        };
        if !top.contains_key(ENVELOPE_KEY) {
            // HeardRight's older item: the session fields and no envelope.
            return match session_from(top) {
                Some(session) => Self {
                    revision: None,
                    state: State::Active(session),
                },
                None => Self::corrupt(),
            };
        }
        if top.get(ENVELOPE_KEY).and_then(Value::as_u64) != Some(ENVELOPE_VERSION) {
            return Self::corrupt();
        }
        let Some(revision) = text_field(top, &["revision"]) else {
            return Self::corrupt();
        };
        match top.get("state").and_then(Value::as_str) {
            Some("tombstone") => Self {
                revision: Some(revision),
                state: State::Tombstone,
            },
            Some("active") => match session_from(top) {
                Some(session) => Self {
                    revision: Some(revision),
                    state: State::Active(session),
                },
                None => Self::corrupt(),
            },
            // An unknown state is never read as signed in.
            _ => Self::corrupt(),
        }
    }
}

fn text_field(map: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn session_from(map: &Map<String, Value>) -> Option<Session> {
    let token = text_field(map, &["token"])?;
    if token.len() as u64 > MAX_TOKEN_BYTES {
        return None;
    }
    let issued_at = ["issuedAtUnix", "issued_at_unix"]
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(Session {
        token: Zeroizing::new(token),
        email: text_field(map, &["email"]),
        issued_at,
    })
}

/// The item text of an active session under a fresh revision.
pub(crate) fn encode_active(session: &Session) -> Result<Secret, LicenseError> {
    let mut top = Map::new();
    top.insert(ENVELOPE_KEY.into(), json!(ENVELOPE_VERSION));
    top.insert("revision".into(), json!(new_revision()?));
    top.insert("state".into(), json!("active"));
    top.insert("token".into(), json!(session.token.as_str()));
    if let Some(email) = &session.email {
        top.insert("email".into(), json!(email));
    }
    top.insert("issuedAtUnix".into(), json!(session.issued_at));
    // The text goes straight into a zeroizing secret.
    let text = Zeroizing::new(Value::Object(top).to_string());
    Ok(Secret::from_string(text.as_str()))
}

/// The item text of the sign-out tombstone under a fresh revision.
pub(crate) fn encode_tombstone() -> Result<Secret, LicenseError> {
    let mut top = Map::new();
    top.insert(ENVELOPE_KEY.into(), json!(ENVELOPE_VERSION));
    top.insert("revision".into(), json!(new_revision()?));
    top.insert("state".into(), json!("tombstone"));
    Ok(Secret::from_string(Value::Object(top).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            token: Zeroizing::new("SENTINEL_SESSION".into()),
            email: Some("a@b.co".into()),
            issued_at: 1_000,
        }
    }

    fn parsed(secret: &Secret) -> Item {
        Item::from_secret(secret)
    }

    #[test]
    fn an_active_item_round_trips_with_a_revision_and_the_legacy_field_names() {
        let secret = encode_active(&session()).unwrap();
        let text = secret.expose_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        // The older reader's names are at the top level.
        assert_eq!(value["token"], "SENTINEL_SESSION");
        assert_eq!(value["email"], "a@b.co");
        assert_eq!(value["issuedAtUnix"], 1_000);
        let item = parsed(&secret);
        assert!(item.revision.is_some());
        let read = item.session().expect("an active session");
        assert_eq!(read.token.as_str(), "SENTINEL_SESSION");
        assert_eq!(read.issued_at, 1_000);
    }

    #[test]
    fn two_writes_never_share_a_revision() {
        let one = parsed(&encode_active(&session()).unwrap());
        let two = parsed(&encode_active(&session()).unwrap());
        assert_ne!(one.revision, two.revision);
        assert!(!one.is_same_as(&two));
        assert!(one.is_same_as(&one.clone()));
    }

    #[test]
    fn a_bare_older_item_is_an_active_session_with_no_revision() {
        let item = Item::parse(r#"{"token":"T","email":"a@b.co","issuedAtUnix":7}"#);
        assert!(item.revision.is_none());
        assert_eq!(item.session().unwrap().token.as_str(), "T");
        let snake = Item::parse(r#"{"token":"T","issued_at_unix":9}"#);
        assert_eq!(snake.session().unwrap().issued_at, 9);
    }

    #[test]
    fn a_tombstone_is_never_a_session() {
        let item = parsed(&encode_tombstone().unwrap());
        assert!(matches!(item.state, State::Tombstone));
        assert!(item.revision.is_some());
        assert!(item.session().is_none());
        // A tombstone that still carries a token is a tombstone.
        let carrying = Item::parse(r#"{"rkv":1,"revision":"r","state":"tombstone","token":"T"}"#);
        assert!(matches!(carrying.state, State::Tombstone));
    }

    #[test]
    fn unreadable_items_are_corrupt_never_signed_in() {
        for text in [
            "",
            "not json",
            "[]",
            r#"{"email":"a@b.co"}"#,
            r#"{"token":"  "}"#,
            r#"{"rkv":2,"revision":"r","state":"active","token":"T"}"#,
            r#"{"rkv":1,"state":"active","token":"T"}"#,
            r#"{"rkv":1,"revision":"r","state":"weird","token":"T"}"#,
            r#"{"rkv":1,"revision":"r","state":"active"}"#,
        ] {
            assert!(
                matches!(Item::parse(text).state, State::Corrupt),
                "{text:?}"
            );
        }
    }

    #[test]
    fn an_oversized_item_or_token_is_refused_unread() {
        let long = "x".repeat(MAX_TOKEN_BYTES as usize + 1);
        let token = format!(r#"{{"token":"{long}"}}"#);
        assert!(matches!(Item::parse(&token).state, State::Corrupt));
        let item = Item::from_secret(&Secret::from_string("y".repeat(MAX_ITEM_BYTES + 1)));
        assert!(matches!(item.state, State::Corrupt));
    }

    #[test]
    fn a_session_never_prints_its_token() {
        let text = format!(
            "{:?} {:?}",
            session(),
            parsed(&encode_active(&session()).unwrap())
        );
        assert!(!text.contains("SENTINEL"), "{text}");
    }
}
