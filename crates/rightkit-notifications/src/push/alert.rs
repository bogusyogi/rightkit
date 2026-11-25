//! The only content a phone alert can carry.

use super::PushError;

/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 64;
/// Longest item id, in bytes.
pub const MAX_ITEM_ID_BYTES: usize = 128;

/// A phone alert: a title and the id of the item to open. There is deliberately no body, subtitle
/// or custom-data field; a type that cannot hold message text cannot leak it. Build with
/// [`PhoneAlert::new`], the only constructor, which rejects anything outside the shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneAlert {
    title: String,
    item_id: String,
}

impl PhoneAlert {
    /// `title` is trimmed, then must be 1 to [`MAX_TITLE_CHARS`] characters with no control
    /// characters (an over-long title is rejected, not truncated). `item_id` must be 1 to
    /// [`MAX_ITEM_ID_BYTES`] bytes of `A-Z a-z 0-9 . _ : -`, an opaque handle the phone resolves
    /// locally, so it cannot smuggle prose.
    pub fn new(title: &str, item_id: &str) -> Result<Self, PushError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(PushError::TitleEmpty);
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(PushError::TitleTooLong);
        }
        if title.chars().any(char::is_control) {
            return Err(PushError::TitleControl);
        }
        if item_id.is_empty()
            || item_id.len() > MAX_ITEM_ID_BYTES
            || !item_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
        {
            return Err(PushError::BadItemId);
        }
        Ok(Self {
            title: title.to_owned(),
            item_id: item_id.to_owned(),
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    /// The APNs JSON body: `{"aps":{"alert":{"title":T},"sound":"default"},"item_id":I}`.
    pub(crate) fn payload_json(&self) -> String {
        let mut out = String::with_capacity(64 + self.title.len() + self.item_id.len());
        out.push_str("{\"aps\":{\"alert\":{\"title\":");
        push_json_string(&mut out, &self.title);
        out.push_str("},\"sound\":\"default\"},\"item_id\":");
        push_json_string(&mut out, &self.item_id);
        out.push('}');
        out
    }
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
