//! BPE / text tokenizer loaded from GGUF metadata.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::model::GgufFile;
use crate::pipeline::byte_unicode::BYTE_TO_UNICODE;
use crate::Qwen3TtsError;

#[derive(Debug, Clone)]
pub struct TokenizerConfig {
    pub vocab_size: i32,
    pub pad_token_id: i32,
    pub eos_token_id: i32,
    pub bos_token_id: i32,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            vocab_size: 151_936,
            pad_token_id: 151_643,
            eos_token_id: 151_645,
            bos_token_id: 151_644,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextTokenizer {
    config: TokenizerConfig,
    vocab: HashMap<String, i32>,
    id_to_token: Vec<String>,
    bpe_ranks: HashMap<(String, String), i32>,
    assistant_token_id: i32,
    newline_token_id: i32,
}

impl TextTokenizer {
    fn assistant_prefix_tokens(&self) -> [i32; 3] {
        [
            self.config.bos_token_id,
            self.assistant_token_id,
            self.newline_token_id,
        ]
    }

    pub fn load_from_gguf(file: &GgufFile) -> Result<Self, Qwen3TtsError> {
        let tokens = file
            .get_arr_str("tokenizer.ggml.tokens")
            .ok_or_else(|| Qwen3TtsError::Tokenizer("tokenizer.ggml.tokens not found".into()))?;
        let n_vocab = tokens.len();
        if n_vocab == 0 {
            return Err(Qwen3TtsError::Tokenizer("empty vocabulary".into()));
        }

        let mut vocab = HashMap::with_capacity(n_vocab);
        let mut id_to_token = vec![String::new(); n_vocab];
        for (i, token) in tokens.into_iter().enumerate() {
            id_to_token[i] = token.clone();
            vocab.insert(token, i as i32);
        }

        let mut bpe_ranks = HashMap::new();
        if let Some(merges) = file.get_arr_str("tokenizer.ggml.merges") {
            for (i, merge) in merges.into_iter().enumerate() {
                if let Some((first, second)) = merge.split_once(' ') {
                    bpe_ranks.insert((first.to_owned(), second.to_owned()), i as i32);
                }
            }
        }

        let mut config = TokenizerConfig::default();
        config.bos_token_id =
            file.get_u32("tokenizer.ggml.bos_token_id", config.bos_token_id as u32) as i32;
        config.eos_token_id =
            file.get_u32("tokenizer.ggml.eos_token_id", config.eos_token_id as u32) as i32;
        config.pad_token_id = file.get_u32(
            "tokenizer.ggml.padding_token_id",
            config.pad_token_id as u32,
        ) as i32;
        config.vocab_size = n_vocab as i32;

        let assistant_token_id = vocab
            .get("assistant")
            .copied()
            .or_else(|| vocab.get("Ġassistant").copied())
            .ok_or_else(|| Qwen3TtsError::MissingTokenizerToken("assistant".into()))?;
        let newline_token_id = vocab
            .get("Ċ")
            .copied()
            .or_else(|| vocab.get("\n").copied())
            .ok_or_else(|| Qwen3TtsError::MissingTokenizerToken("\\n".into()))?;

        Ok(Self {
            config,
            vocab,
            id_to_token,
            bpe_ranks,
            assistant_token_id,
            newline_token_id,
        })
    }

    #[must_use]
    pub fn config(&self) -> &TokenizerConfig {
        &self.config
    }

    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<i32> {
        let unicode_text = bytes_to_unicode(text);
        let mut words = Vec::<String>::new();
        let mut current_word = String::new();

        for ch in unicode_text.chars() {
            if ch == 'Ġ' {
                if !current_word.is_empty() {
                    words.push(std::mem::take(&mut current_word));
                }
                current_word.push(ch);
            } else {
                current_word.push(ch);
            }
        }
        if !current_word.is_empty() {
            words.push(current_word);
        }

        let mut tokens = Vec::new();
        for word in words {
            for piece in self.bpe(&word) {
                if let Some(id) = self.vocab.get(&piece) {
                    tokens.push(*id);
                } else {
                    for byte in piece.as_bytes() {
                        let mapped = BYTE_TO_UNICODE[*byte as usize];
                        if let Some(id) = self.vocab.get(mapped) {
                            tokens.push(*id);
                        }
                    }
                }
            }
        }

        tokens
    }

    #[must_use]
    pub fn encode_for_tts(&self, text: &str) -> Vec<i32> {
        let mut tokens = Vec::new();
        tokens.extend_from_slice(&self.assistant_prefix_tokens());
        tokens.extend(self.encode(text));
        tokens.push(self.config.eos_token_id);
        tokens.push(self.newline_token_id);
        tokens.extend_from_slice(&self.assistant_prefix_tokens());
        tokens
    }

    /// Separate instruction turn; project & prepend it before assistant/codec prefill.
    pub fn encode_instruction_for_tts(&self, instruct: &str) -> Result<Vec<i32>, Qwen3TtsError> {
        let mut tokens = vec![self.config.bos_token_id];
        // Normal encoding permits a split `user` token in custom vocabularies.
        let user = self
            .vocab
            .get("user")
            .copied()
            .map_or_else(|| self.encode("user"), |id| vec![id]);
        if self.decode(&user) != "user" {
            return Err(Qwen3TtsError::MissingTokenizerToken("user".into()));
        }
        tokens.extend(user);
        tokens.push(self.newline_token_id);
        tokens.extend(self.encode(instruct));
        tokens.push(self.config.eos_token_id);
        tokens.push(self.newline_token_id);
        Ok(tokens)
    }

    pub(crate) fn encode_optional_instruction_for_tts(
        &self,
        instruct: Option<&str>,
        enabled: bool,
    ) -> Result<Vec<i32>, Qwen3TtsError> {
        if enabled {
            instruct
                .filter(|text| !text.is_empty())
                .map(|text| self.encode_instruction_for_tts(text))
                .transpose()
                .map(Option::unwrap_or_default)
        } else {
            Ok(Vec::new())
        }
    }

    #[must_use]
    pub fn encode_ref_for_tts(&self, text: &str) -> Vec<i32> {
        let mut tokens = Vec::new();
        tokens.extend_from_slice(&self.assistant_prefix_tokens());
        tokens.extend(self.encode(text));
        tokens.push(self.config.eos_token_id);
        tokens.push(self.newline_token_id);
        tokens
    }

    #[must_use]
    pub fn encode_for_voice_clone(&self, ref_text: &str, text: &str) -> Vec<i32> {
        let mut tokens = self.encode_ref_for_tts(ref_text);
        tokens.extend(self.encode_for_tts(text));
        tokens
    }

    #[must_use]
    pub fn decode(&self, tokens: &[i32]) -> String {
        let mut result = String::new();
        for token in tokens {
            result.push_str(&self.decode_token(*token));
        }
        result
    }

    #[must_use]
    pub fn decode_token(&self, token_id: i32) -> String {
        self.id_to_token
            .get(token_id as usize)
            .map_or_else(String::new, |token| unicode_to_bytes(token))
    }

    fn bpe(&self, token: &str) -> Vec<String> {
        if token.is_empty() {
            return Vec::new();
        }

        let mut word = token.chars().map(|c| c.to_string()).collect::<Vec<_>>();
        if word.len() == 1 {
            return word;
        }

        while let Some((first, second)) = self.get_min_pair(&word) {
            let mut merged = Vec::with_capacity(word.len());
            let mut i = 0;
            while i < word.len() {
                if i + 1 < word.len() && word[i] == first && word[i + 1] == second {
                    merged.push(format!("{first}{second}"));
                    i += 2;
                } else {
                    merged.push(word[i].clone());
                    i += 1;
                }
            }
            word = merged;
            if word.len() == 1 {
                break;
            }
        }

        word
    }

    fn get_min_pair(&self, word: &[String]) -> Option<(String, String)> {
        let mut best: Option<(String, String)> = None;
        let mut best_rank = i32::MAX;

        for pair in word.windows(2) {
            let key = (pair[0].clone(), pair[1].clone());
            if let Some(rank) = self.bpe_ranks.get(&key) {
                if *rank < best_rank {
                    best_rank = *rank;
                    best = Some(key);
                }
            }
        }

        best
    }
}

fn unicode_to_bytes(text: &str) -> String {
    let reverse = unicode_to_byte();
    let mut result = Vec::new();
    for ch in text.chars() {
        if let Some(byte) = reverse.get(&ch) {
            result.push(*byte);
        } else {
            result.extend(ch.to_string().into_bytes());
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn bytes_to_unicode(text: &str) -> String {
    let mut result = String::new();
    for byte in text.as_bytes() {
        result.push_str(BYTE_TO_UNICODE[*byte as usize]);
    }
    result
}

fn unicode_to_byte() -> &'static HashMap<char, u8> {
    static REVERSE: OnceLock<HashMap<char, u8>> = OnceLock::new();
    REVERSE.get_or_init(|| {
        BYTE_TO_UNICODE
            .iter()
            .enumerate()
            .filter_map(|(idx, value)| value.chars().next().map(|ch| (ch, idx as u8)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    use ggml_sys as sys;

    fn write_minimal_tokenizer_gguf(path: &std::path::Path) {
        let key_tokens = CString::new("tokenizer.ggml.tokens").unwrap();
        let key_merges = CString::new("tokenizer.ggml.merges").unwrap();
        let key_bos = CString::new("tokenizer.ggml.bos_token_id").unwrap();
        let key_eos = CString::new("tokenizer.ggml.eos_token_id").unwrap();
        let key_pad = CString::new("tokenizer.ggml.padding_token_id").unwrap();

        let tokens = [
            CString::new("<pad>").unwrap(),
            CString::new("<|im_start|>").unwrap(),
            CString::new("<|im_end|>").unwrap(),
            CString::new("assistant").unwrap(),
            CString::new("Ċ").unwrap(),
            CString::new("h").unwrap(),
            CString::new("i").unwrap(),
        ];
        let mut token_ptrs = tokens.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
        let mut merge_ptrs: [*const i8; 0] = [];
        let path = CString::new(path.to_string_lossy().as_ref()).unwrap();

        unsafe {
            let ctx = sys::gguf_init_empty();
            sys::gguf_set_arr_str(
                ctx,
                key_tokens.as_ptr(),
                token_ptrs.as_mut_ptr(),
                token_ptrs.len(),
            );
            sys::gguf_set_arr_str(ctx, key_merges.as_ptr(), merge_ptrs.as_mut_ptr(), 0);
            sys::gguf_set_val_u32(ctx, key_bos.as_ptr(), 1);
            sys::gguf_set_val_u32(ctx, key_eos.as_ptr(), 2);
            sys::gguf_set_val_u32(ctx, key_pad.as_ptr(), 0);
            assert!(sys::gguf_write_to_file(ctx, path.as_ptr(), true));
            sys::gguf_free(ctx);
        }
    }

    #[test]
    fn encode_for_tts_from_synthetic_gguf() {
        let mut path = std::env::temp_dir();
        path.push("qwen_tts_native_tokenizer_test.gguf");
        write_minimal_tokenizer_gguf(&path);

        let file = crate::model::GgufFile::open(&path).unwrap();
        let tokenizer = TextTokenizer::load_from_gguf(&file).unwrap();
        let tokens = tokenizer.encode_for_tts("hi");

        assert_eq!(tokens, vec![1, 3, 4, 5, 6, 2, 4, 1, 3, 4]);

        let _ = std::fs::remove_file(path);
    }
    fn synthetic_tokenizer() -> TextTokenizer {
        let tokens = [
            "<pad>",
            "<|im_start|>",
            "<|im_end|>",
            "assistant",
            "Ċ",
            "h",
            "i",
            "user",
        ];
        TextTokenizer {
            config: TokenizerConfig {
                vocab_size: 8,
                pad_token_id: 0,
                bos_token_id: 1,
                eos_token_id: 2,
            },
            vocab: tokens
                .iter()
                .enumerate()
                .map(|(id, token)| ((*token).into(), id as i32))
                .collect(),
            id_to_token: tokens.iter().map(|token| (*token).into()).collect(),
            bpe_ranks: HashMap::new(),
            assistant_token_id: 3,
            newline_token_id: 4,
        }
    }

    #[test]
    fn base_prompt_ids_match_0_1_1() {
        let tokenizer = synthetic_tokenizer();
        // Frozen HEAD (0.1.1) sequence: assistant prefix, text, end/newline,
        // then another assistant prefix. Base adds no instruction turn.
        assert_eq!(
            tokenizer.encode_for_tts("hi"),
            [1, 3, 4, 5, 6, 2, 4, 1, 3, 4],
        );
    }

    #[test]
    fn exact_prompt_token_strings_for_modes() {
        let tokenizer = synthetic_tokenizer();
        let assistant = vec![
            "<|im_start|>",
            "assistant",
            "\n",
            "h",
            "i",
            "<|im_end|>",
            "\n",
            "<|im_start|>",
            "assistant",
            "\n",
        ];
        for mode in [
            crate::QtsMode::Base,
            crate::QtsMode::CustomVoice,
            crate::QtsMode::VoiceDesign,
        ] {
            let mut tokens = if mode == crate::QtsMode::Base {
                Vec::new()
            } else {
                tokenizer.encode_instruction_for_tts("hi").unwrap()
            };
            tokens.extend(tokenizer.encode_for_tts("hi"));
            let strings = tokens
                .iter()
                .map(|id| tokenizer.decode_token(*id))
                .collect::<Vec<_>>();
            let mut expected = if mode == crate::QtsMode::Base {
                Vec::new()
            } else {
                vec!["<|im_start|>", "user", "\n", "h", "i", "<|im_end|>", "\n"]
            };
            expected.extend(assistant.clone());
            assert_eq!(strings, expected, "{mode:?}");
        }
        assert_eq!(
            tokenizer.decode(&tokenizer.encode_for_tts("hi")),
            "<|im_start|>assistant\nhi<|im_end|>\n<|im_start|>assistant\n"
        );
        assert_eq!(
            tokenizer.decode(&tokenizer.encode_instruction_for_tts("hi").unwrap()),
            "<|im_start|>user\nhi<|im_end|>\n"
        );
        // CustomVoice without instruct keeps existing assistant turn unchanged.
        assert_eq!(
            tokenizer.encode_for_tts("hi"),
            vec![1, 3, 4, 5, 6, 2, 4, 1, 3, 4]
        );
    }

    #[test]
    fn instruction_requires_complete_user_role() {
        let mut tokenizer = synthetic_tokenizer();
        tokenizer.vocab.remove("user");
        assert!(tokenizer.encode_instruction_for_tts("hi").is_err());
    }
    #[test]
    fn optional_and_0_6b_instructions_are_omitted() {
        let tokenizer = synthetic_tokenizer();
        assert!(tokenizer
            .encode_optional_instruction_for_tts(Some("hi"), false)
            .unwrap()
            .is_empty());
        assert!(tokenizer
            .encode_optional_instruction_for_tts(None, true)
            .unwrap()
            .is_empty());
        assert!(tokenizer
            .encode_optional_instruction_for_tts(Some(""), true)
            .unwrap()
            .is_empty());
        assert_eq!(
            tokenizer
                .encode_optional_instruction_for_tts(Some("hi"), true)
                .unwrap(),
            vec![1, 7, 4, 5, 6, 2, 4]
        );
    }
}
