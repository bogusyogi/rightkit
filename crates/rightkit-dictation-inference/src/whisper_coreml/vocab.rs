// Whisper vocabulary table read straight from the bundle's `tokenizer.json`
// (DEP-09). The CoreML Whisper path only ever needed four tokenizer calls —
// `token_to_id`, `id_to_token` and `decode(ids, skip_special_tokens = true)` —
// none of which needs a regex engine, BPE merges or a normalizer. Carrying the
// whole `tokenizers` crate (+ the Oniguruma C build) for them cost ~47 crates in
// the macOS engine graph.
//
// PARITY CONTRACT: every call here reproduces `tokenizers` 0.23.2 exactly for a
// byte-level BPE tokenizer with no normalizer (the Whisper large-v3 layout):
//   * ids of `added_tokens` are ASSIGNED the way `AddedVocabulary::add_tokens`
//     assigns them (existing id if the content is already known, else the next
//     id from the model vocab size), not read from the JSON `id` field;
//   * `id_to_token` / `token_to_id` consult the added vocabulary first, then the
//     model vocab;
//   * `decode` drops ids with no token, drops tokens whose STRING is a special
//     added token (when `skip_special_tokens`), maps each token through the GPT-2
//     byte decoder (a token with any unmapped char contributes its raw UTF-8
//     bytes instead), and finishes with `String::from_utf8_lossy`.
// Anything this table cannot reproduce exactly (a non-ByteLevel decoder, a
// normalizer that would rewrite added tokens, a non-BPE model, duplicate vocab
// ids) is rejected at load instead of decoding differently.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct TokenizerJson {
    #[serde(default)]
    added_tokens: Vec<AddedTokenJson>,
    #[serde(default)]
    normalizer: Option<serde_json::Value>,
    #[serde(default)]
    decoder: Option<DecoderJson>,
    model: ModelJson,
}

#[derive(Deserialize, Clone, PartialEq, Eq)]
struct AddedTokenJson {
    /// Informational only: `tokenizers` assigns ids itself (see module doc).
    #[serde(default)]
    id: Option<u32>,
    content: String,
    #[serde(default)]
    single_word: bool,
    #[serde(default)]
    lstrip: bool,
    #[serde(default)]
    rstrip: bool,
    #[serde(default = "default_true")]
    normalized: bool,
    #[serde(default)]
    special: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct DecoderJson {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ModelJson {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    vocab: HashMap<String, u32>,
}

/// The token-id ↔ string table plus the byte-level decoder Whisper needs.
pub(crate) struct WhisperVocab {
    /// Model (BPE) vocab, reverse map: id → token.
    model_by_id: HashMap<u32, String>,
    /// Model (BPE) vocab: token → id.
    model_by_token: HashMap<String, u32>,
    /// Added vocabulary: id → content (what `simple_id_to_token` returns when
    /// there is no normalizer).
    added_by_id: HashMap<u32, String>,
    /// Added vocabulary: content → id.
    added_by_token: HashMap<String, u32>,
    /// Contents of every special added token (`decode` filters by string).
    special: HashSet<String>,
    /// GPT-2 byte decoder: unicode char → raw byte.
    char_bytes: HashMap<char, u8>,
}

impl WhisperVocab {
    pub(crate) fn from_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read(path).map_err(|e| e.to_string())?;
        Self::from_json_bytes(&raw)
    }

    pub(crate) fn from_json_bytes(raw: &[u8]) -> Result<Self, String> {
        let parsed: TokenizerJson = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        match parsed.model.kind.as_deref() {
            None | Some("BPE") => {}
            Some(other) => return Err(format!("unsupported tokenizer model type {other}")),
        }
        match parsed.decoder.as_ref().map(|d| d.kind.as_str()) {
            Some("ByteLevel") => {}
            Some(other) => return Err(format!("unsupported tokenizer decoder {other}")),
            None => return Err("tokenizer has no decoder; expected ByteLevel".to_string()),
        }
        let has_normalizer = parsed
            .normalizer
            .as_ref()
            .is_some_and(|value| !value.is_null());
        if has_normalizer && parsed.added_tokens.iter().any(|t| t.normalized) {
            // `tokenizers` would decode those added tokens through their
            // normalized form; this table does not implement normalizers.
            return Err(
                "tokenizer normalizer rewrites added tokens; not supported by the vocab table"
                    .to_string(),
            );
        }

        let model_by_token = parsed.model.vocab;
        let mut model_by_id = HashMap::with_capacity(model_by_token.len());
        for (token, &id) in &model_by_token {
            if model_by_id.insert(id, token.clone()).is_some() {
                // `tokenizers` would pick one at HashMap-iteration random; refuse.
                return Err(format!("tokenizer vocab maps id {id} twice"));
            }
        }

        // Port of `AddedVocabulary::add_tokens` on an empty added vocabulary.
        let mut added_by_id: HashMap<u32, AddedTokenJson> = HashMap::new();
        let mut added_by_token: HashMap<String, u32> = HashMap::new();
        let mut special: HashSet<String> = HashSet::new();
        let mut next_id = model_by_token.len() as u32;
        for token in parsed.added_tokens {
            if token.content.is_empty() {
                continue;
            }
            if let Some(id) = added_by_token.get(&token.content) {
                if added_by_id.get(id) == Some(&token) {
                    continue;
                }
            }
            let new_id = match added_by_token
                .get(&token.content)
                .or_else(|| model_by_token.get(&token.content))
            {
                Some(&id) => id,
                None => {
                    let id = next_id;
                    next_id += 1;
                    id
                }
            };
            if let Some(declared) = token.id {
                if declared != new_id {
                    // `tokenizers` only warns here too; the assigned id wins.
                    tracing::warn!(
                        content = %token.content,
                        declared,
                        assigned = new_id,
                        "whisper tokenizer added token id differs from its declared id"
                    );
                }
            }
            added_by_token.insert(token.content.clone(), new_id);
            if token.special {
                special.insert(token.content.clone());
            }
            added_by_id.insert(new_id, token);
        }

        Ok(Self {
            model_by_id,
            model_by_token,
            added_by_id: added_by_id
                .into_iter()
                .map(|(id, token)| (id, token.content))
                .collect(),
            added_by_token,
            special,
            char_bytes: byte_decoder(),
        })
    }

    /// `Tokenizer::token_to_id`: added vocabulary first, then the model vocab.
    pub(crate) fn token_to_id(&self, token: &str) -> Option<u32> {
        self.added_by_token
            .get(token)
            .or_else(|| self.model_by_token.get(token))
            .copied()
    }

    fn token_str(&self, id: u32) -> Option<&str> {
        self.added_by_id
            .get(&id)
            .or_else(|| self.model_by_id.get(&id))
            .map(String::as_str)
    }

    /// `Tokenizer::id_to_token`: the raw (byte-level encoded) token string.
    pub(crate) fn id_to_token(&self, id: u32) -> Option<String> {
        self.token_str(id).map(str::to_owned)
    }

    /// `Tokenizer::decode(ids, skip_special_tokens)` with the ByteLevel decoder.
    pub(crate) fn decode(&self, ids: &[u32], skip_special_tokens: bool) -> String {
        let mut bytes: Vec<u8> = Vec::with_capacity(ids.len() * 4);
        for &id in ids {
            let Some(token) = self.token_str(id) else {
                continue;
            };
            if skip_special_tokens && self.special.contains(token) {
                continue;
            }
            let start = bytes.len();
            let mut all_mapped = true;
            for c in token.chars() {
                match self.char_bytes.get(&c) {
                    Some(&b) => bytes.push(b),
                    None => {
                        all_mapped = false;
                        break;
                    }
                }
            }
            if !all_mapped {
                bytes.truncate(start);
                bytes.extend_from_slice(token.as_bytes());
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// GPT-2 `bytes_to_unicode`, inverted (char → byte). The printable Latin-1
/// bytes map to themselves; the remaining 68 bytes map, in byte order, to
/// U+0100.. (identical to `tokenizers::pre_tokenizers::byte_level::bytes_char`).
pub(crate) fn byte_decoder() -> HashMap<char, u8> {
    let printable = |b: u8| matches!(b, b'!'..=b'~' | 0xA1..=0xAC | 0xAE..=0xFF);
    let mut map = HashMap::with_capacity(256);
    let mut n = 0u32;
    for b in 0..=255u8 {
        let c = if printable(b) {
            char::from(b)
        } else {
            let c = char::from_u32(256 + n).expect("U+0100..U+0143 are valid chars");
            n += 1;
            c
        };
        map.insert(c, b);
    }
    map
}
