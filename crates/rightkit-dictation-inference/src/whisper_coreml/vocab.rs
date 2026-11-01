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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A miniature tokenizer.json exercising every decode rule.
    fn mini() -> WhisperVocab {
        // "Ġ" = U+0120 (space), "Ċ" = U+010A (newline), "Ã" "©" = bytes C3 A9 ("é").
        let json = r#"{
          "version": "1.0",
          "added_tokens": [
            {"id": 5, "content": "<|endoftext|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
            {"id": 6, "content": "<|en|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
            {"id": 7, "content": "<|0.00|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": true, "special": false},
            {"id": 8, "content": "naïve★", "single_word": false, "lstrip": false, "rstrip": false, "normalized": true, "special": false}
          ],
          "normalizer": null,
          "pre_tokenizer": {"type": "ByteLevel", "add_prefix_space": false, "trim_offsets": true, "use_regex": true},
          "decoder": {"type": "ByteLevel", "add_prefix_space": true, "trim_offsets": true, "use_regex": true},
          "model": {"type": "BPE", "vocab": {"Hello": 0, "Ġworld": 1, "Ċ": 2, "Ã": 3, "©": 4}, "merges": []}
        }"#;
        WhisperVocab::from_json_bytes(json.as_bytes()).expect("mini tokenizer")
    }

    #[test]
    fn byte_decoder_is_the_gpt2_bijection() {
        let map = byte_decoder();
        assert_eq!(map.len(), 256);
        let mut seen = [false; 256];
        for &b in map.values() {
            assert!(!seen[b as usize], "byte {b} mapped twice");
            seen[b as usize] = true;
        }
        assert_eq!(map[&'\u{0100}'], 0x00);
        assert_eq!(map[&'\u{010A}'], b'\n');
        assert_eq!(map[&'\u{0120}'], b' ');
        assert_eq!(map[&'\u{0121}'], 0x7F);
        assert_eq!(map[&'\u{0142}'], 0xA0);
        assert_eq!(map[&'\u{0143}'], 0xAD);
        assert_eq!(map[&'!'], b'!');
        assert_eq!(map[&'~'], b'~');
        assert_eq!(map[&'\u{00FF}'], 0xFF);
        assert!(!map.contains_key(&' '));
        assert!(!map.contains_key(&'\u{0144}'));
    }

    #[test]
    fn decode_follows_tokenizers_rules() {
        let v = mini();
        assert_eq!(v.decode(&[0, 1, 2], true), "Hello world\n");
        // Multi-byte UTF-8 split across two tokens joins before lossy decode.
        assert_eq!(v.decode(&[3, 4], true), "é");
        // A dangling lead byte is replaced, exactly like from_utf8_lossy.
        assert_eq!(v.decode(&[0, 3], true), "Hello\u{FFFD}");
        // Specials are skipped only when asked; unknown ids always vanish.
        assert_eq!(v.decode(&[5, 0, 6, 99], true), "Hello");
        assert_eq!(v.decode(&[6, 0], false), "<|en|>Hello");
        // Non-special added tokens survive; ASCII-printable maps to itself.
        assert_eq!(v.decode(&[7, 0], true), "<|0.00|>Hello");
        // A token with an unmapped char ('★') contributes its raw UTF-8.
        assert_eq!(v.decode(&[8], true), "naïve★");
        assert_eq!(v.decode(&[], true), "");
    }

    #[test]
    fn ids_and_lookups_follow_added_vocabulary_assignment() {
        let v = mini();
        // Added ids are assigned from the model vocab size (5), in order.
        assert_eq!(v.token_to_id("<|endoftext|>"), Some(5));
        assert_eq!(v.token_to_id("<|en|>"), Some(6));
        assert_eq!(v.token_to_id("Ġworld"), Some(1));
        assert_eq!(v.token_to_id("<|xx|>"), None);
        assert_eq!(v.id_to_token(1).as_deref(), Some("Ġworld"));
        assert_eq!(v.id_to_token(6).as_deref(), Some("<|en|>"));
        assert_eq!(v.id_to_token(42), None);
    }

    #[test]
    fn rejects_layouts_it_cannot_reproduce() {
        let bad_decoder = r#"{"added_tokens": [], "normalizer": null,
            "decoder": {"type": "WordPiece"}, "model": {"type": "BPE", "vocab": {}}}"#;
        assert!(WhisperVocab::from_json_bytes(bad_decoder.as_bytes()).is_err());
        let normalizing = r#"{"added_tokens": [{"id": 0, "content": "x", "normalized": true, "special": false}],
            "normalizer": {"type": "Lowercase"},
            "decoder": {"type": "ByteLevel"}, "model": {"type": "BPE", "vocab": {}}}"#;
        assert!(WhisperVocab::from_json_bytes(normalizing.as_bytes()).is_err());
        let dup = r#"{"added_tokens": [], "normalizer": null,
            "decoder": {"type": "ByteLevel"}, "model": {"type": "BPE", "vocab": {"a": 0, "b": 0}}}"#;
        assert!(WhisperVocab::from_json_bytes(dup.as_bytes()).is_err());
    }

    // ---- Parity against `tokenizers` on the real Whisper tokenizer.json ----
    //
    // The golden digests below were produced by `tokenizers` 0.23.2 itself
    // (Tokenizer::from_file + decode(ids, true) / id_to_token) over the
    // Hugging Face `openai/whisper-large-v3-turbo` and `openai/whisper-large-v3`
    // tokenizer.json files (sha256 297b1337…71fd and 6d8cbd7c…21ca; both give the
    // same digests). Opt-in because the tokenizer is a model asset, not a repo
    // file: RIGHTKIT_WHISPER_VOCAB_PARITY_TOKENIZER=<bundle>/tokenizer.json.

    pub(crate) const PARITY_ID_SPACE: u32 = 51_870; // 51,866 tokens + 4 unknown ids
    pub(crate) const PARITY_RANDOM_SEQUENCES: usize = 4_000;

    /// Deterministic id sequences (fixed LCG) mixing text, specials,
    /// timestamps and unknown ids, lengths 1..=48.
    pub(crate) fn parity_sequences() -> Vec<Vec<u32>> {
        let mut state: u64 = 0x5EED_0000_DEB0_0009;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        let mut sequences = Vec::with_capacity(PARITY_RANDOM_SEQUENCES);
        for _ in 0..PARITY_RANDOM_SEQUENCES {
            let len = 1 + (next() % 48) as usize;
            let seq = (0..len)
                .map(|_| {
                    // 90 % text ids (where multi-byte joins happen), 10 % anything.
                    if next() % 10 == 0 {
                        next() % PARITY_ID_SPACE
                    } else {
                        next() % 50_257
                    }
                })
                .collect();
            sequences.push(seq);
        }
        sequences
    }

    /// Per-id digest input and sequence digest input, shared with the
    /// `tokenizers`-side generator so both hash identical framing.
    pub(crate) fn parity_transcripts(
        decode: impl Fn(&[u32]) -> String,
        id_to_token: impl Fn(u32) -> Option<String>,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut per_id = Vec::new();
        for id in 0..PARITY_ID_SPACE {
            per_id.extend_from_slice(id.to_string().as_bytes());
            per_id.push(0x1F);
            match id_to_token(id) {
                Some(token) => per_id.extend_from_slice(token.as_bytes()),
                None => per_id.extend_from_slice(b"\xFF<none>"),
            }
            per_id.push(0x1F);
            per_id.extend_from_slice(decode(&[id]).as_bytes());
            per_id.push(0x1E);
        }
        let mut sequences = Vec::new();
        for seq in parity_sequences() {
            sequences.extend_from_slice(decode(&seq).as_bytes());
            sequences.push(0x1E);
        }
        (per_id, sequences)
    }

    pub(crate) const GOLDEN_PER_ID_SHA256: &str =
        "eb1efd0a49f3b4e1dc9c6288ad9b54521d9b9f763ed2626c858c106ff15e049d";
    pub(crate) const GOLDEN_SEQUENCES_SHA256: &str =
        "aed92c96b9c5ea46f6e5c4a4d835be308b4f7adb41a0da2de35a4ef8c940adf9";
    /// (token, id) pairs `lang_token` relies on.
    pub(crate) const GOLDEN_TOKEN_IDS: &[(&str, Option<u32>)] = &[
        ("<|endoftext|>", Some(50257)),
        ("<|startoftranscript|>", Some(50258)),
        ("<|en|>", Some(50259)),
        ("<|ja|>", Some(50266)),
        ("<|yue|>", Some(50358)),
        ("<|transcribe|>", Some(50360)),
        ("<|notimestamps|>", Some(50364)),
        ("<|0.00|>", Some(50365)),
        ("<|30.00|>", Some(51865)),
        ("<|zz|>", None),
    ];

    #[test]
    fn parity_with_tokenizers_on_the_real_whisper_tokenizer() {
        let Ok(path) = rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_VOCAB_PARITY_TOKENIZER")
        else {
            eprintln!(
                "skipped: set RIGHTKIT_WHISPER_VOCAB_PARITY_TOKENIZER=<bundle>/tokenizer.json"
            );
            return;
        };
        let vocab = WhisperVocab::from_file(Path::new(&path)).expect("load tokenizer.json");
        for &(token, id) in GOLDEN_TOKEN_IDS {
            assert_eq!(vocab.token_to_id(token), id, "token_to_id({token})");
        }
        let (per_id, sequences) =
            parity_transcripts(|ids| vocab.decode(ids, true), |id| vocab.id_to_token(id));
        assert_eq!(
            rightkit_fs::digest::sha256_bytes(&per_id),
            GOLDEN_PER_ID_SHA256,
            "per-id decode/id_to_token differs from tokenizers 0.23.2"
        );
        assert_eq!(
            rightkit_fs::digest::sha256_bytes(&sequences),
            GOLDEN_SEQUENCES_SHA256,
            "sequence decode differs from tokenizers 0.23.2"
        );
    }
}
