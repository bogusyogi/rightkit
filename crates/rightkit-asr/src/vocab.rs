use crate::error::{Error, Result};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Vocabulary parser for vocab.txt format used by TDT models
#[derive(Debug, Clone)]
pub struct Vocabulary {
    pub id_to_token: Vec<String>,
    pub _blank_id: usize,
}

impl Vocabulary {
    /// Load vocabulary from vocab.txt file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path.as_ref())
            .map_err(|e| Error::Config(format!("Failed to open vocab file: {}", e)))?;

        let reader = BufReader::new(file);
        let mut id_to_token = Vec::new();
        let mut blank_id = 0;

        for line in reader.lines() {
            let line =
                line.map_err(|e| Error::Config(format!("Failed to read vocab file: {}", e)))?;

            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let token = parts[0].to_string();
                let id: usize = parts[1]
                    .parse()
                    .map_err(|e| Error::Config(format!("Invalid token ID in vocab: {}", e)))?;

                if id >= id_to_token.len() {
                    id_to_token.resize(id + 1, String::new());
                }
                id_to_token[id] = token.clone();

                // Track blank token
                if token == "<blk>" || token == "<blank>" {
                    blank_id = id;
                }
            }
        }

        // Default to last token if no blank found
        if blank_id == 0 && !id_to_token.is_empty() {
            blank_id = id_to_token.len() - 1;
        }

        Ok(Self {
            id_to_token,
            _blank_id: blank_id,
        })
    }

    /// Get token by ID
    pub fn id_to_text(&self, id: usize) -> Option<&str> {
        self.id_to_token.get(id).map(|s| s.as_str())
    }

    /// Encode a human phrase into the model's SentencePiece-like token ids.
    ///
    /// `vocab.txt` already contains the decoder's fixed output units; adding new
    /// rows would not change the ONNX output dimension. Context biasing therefore
    /// has to bias existing token paths. This helper performs a deterministic
    /// longest-prefix segmentation over those existing pieces, using `▁` as the
    /// word-boundary marker used by Parakeet vocabularies.
    pub fn encode_phrase_greedy(&self, phrase: &str) -> Option<Vec<usize>> {
        let normalized = normalize_phrase_for_bias(phrase);
        if normalized.is_empty() {
            return None;
        }
        let mut piece_text = String::new();
        for word in normalized.split_whitespace() {
            piece_text.push('▁');
            piece_text.push_str(word);
        }

        let mut remaining = piece_text.as_str();
        let mut ids = Vec::new();
        while !remaining.is_empty() {
            let mut best: Option<(usize, usize)> = None;
            for (id, token) in self.id_to_token.iter().enumerate() {
                if token.is_empty() || token.starts_with('<') {
                    continue;
                }
                if remaining.starts_with(token) {
                    let len = token.len();
                    if best.map(|(_, best_len)| len > best_len).unwrap_or(true) {
                        best = Some((id, len));
                    }
                }
            }
            let (id, len) = best?;
            ids.push(id);
            remaining = &remaining[len..];
        }
        Some(ids)
    }

    /// Get vocabulary size (number of tokens)
    pub fn size(&self) -> usize {
        self.id_to_token.len()
    }
}

fn normalize_phrase_for_bias(phrase: &str) -> String {
    phrase
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vocab(tokens: &[&str]) -> Vocabulary {
        Vocabulary {
            id_to_token: tokens.iter().map(|s| s.to_string()).collect(),
            _blank_id: tokens.len().saturating_sub(1),
        }
    }

    #[test]
    fn bias_phrase_encoding_uses_existing_subword_paths() {
        let vocab = make_vocab(&["▁z", "e", "ph", "y", "r", "▁sub", "mit", "<blk>"]);
        assert_eq!(
            vocab.encode_phrase_greedy("Zephyr submit"),
            Some(vec![0, 1, 2, 3, 4, 5, 6])
        );
    }

    #[test]
    fn bias_phrase_encoding_rejects_unrepresentable_text() {
        let vocab = make_vocab(&["▁hello", "<blk>"]);
        assert_eq!(vocab.encode_phrase_greedy("Zephyr"), None);
        assert_eq!(vocab.encode_phrase_greedy("   "), None);
    }
}
