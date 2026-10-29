//! `tokens.txt` and keyword-list parsing, ported from Sherpa-ONNX 1.13.4
//! `symbol-table.cc` (`ReadTokens`) and `utils.cc` (`EncodeKeywords`).

use std::collections::HashMap;

use crate::error::{Error, Result};

/// Token ↔ id table read from a Sherpa `tokens.txt` (`<symbol> <id>` per line).
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    sym2id: HashMap<String, i32>,
    id2sym: HashMap<i32, String>,
}

impl SymbolTable {
    pub fn parse(text: &str) -> Result<Self> {
        let mut table = Self::default();
        for line in text.lines() {
            let line = line.trim();
            let mut fields = line.split_whitespace();
            // A line holding only a number names the space token (`ReadTokens`).
            let (sym, id) = match (fields.next(), fields.next()) {
                (Some(id), None) => (" ".to_string(), parse_id(id, line)?),
                (Some(sym), Some(id)) => (sym.to_string(), parse_id(id, line)?),
                (None, _) => continue,
            };
            if fields.next().is_some() {
                return Err(Error::Config(format!("tokens.txt: malformed line `{line}`")));
            }
            // First occurrence wins in both directions, like unordered_map::insert.
            table.id2sym.entry(id).or_insert_with(|| sym.clone());
            table.sym2id.entry(sym).or_insert(id);
        }
        Ok(table)
    }

    pub fn id(&self, symbol: &str) -> Option<i32> {
        self.sym2id.get(symbol).copied()
    }

    /// The symbol for `id`; empty when unknown (Sherpa's `operator[]` would abort).
    pub fn symbol(&self, id: i32) -> &str {
        self.id2sym.get(&id).map(String::as_str).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.id2sym.len()
    }

    pub fn is_empty(&self) -> bool {
        self.id2sym.is_empty()
    }
}

/// `atoi`: leading integer, zero when there is none.
fn parse_id(field: &str, line: &str) -> Result<i32> {
    let digits: String = field
        .char_indices()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
        .map(|(_, c)| c)
        .collect();
    if digits.is_empty() || digits == "-" || digits == "+" {
        return Ok(0);
    }
    digits
        .parse()
        .map_err(|_| Error::Config(format!("tokens.txt: bad id in `{line}`")))
}

/// Encoded keyword list. `scores`, `thresholds` and `phrases` are empty unless at least
/// one line set them, which tells the graph to use the spotter-wide defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EncodedKeywords {
    pub ids: Vec<Vec<i32>>,
    pub scores: Vec<f32>,
    pub thresholds: Vec<f32>,
    pub phrases: Vec<String>,
}

/// Encode a keyword list: one keyword per line, whitespace-separated tokens from
/// `symbols`, then optional `:boost`, `#threshold` and `@phrase` fields.
pub fn encode_keywords(text: &str, symbols: &SymbolTable) -> Result<EncodedKeywords> {
    let mut out = EncodedKeywords::default();
    let (mut scores, mut thresholds, mut phrases) = (Vec::new(), Vec::new(), Vec::new());
    let (mut has_scores, mut has_thresholds, mut has_phrases) = (false, false, false);
    let mut oov = Vec::new();
    for line in text.lines() {
        let mut ids = Vec::new();
        let (mut score, mut threshold, mut phrase) = (0.0f32, 0.0f32, String::new());
        for word in line.split_whitespace() {
            if let Some(id) = symbols.id(word) {
                ids.push(id);
                continue;
            }
            let rest = &word[word.chars().next().map_or(0, char::len_utf8)..];
            match word.chars().next() {
                Some(':') => {
                    score = parse_float(rest, line)?;
                    has_scores = true;
                }
                Some('#') => {
                    threshold = parse_float(rest, line)?;
                    has_thresholds = true;
                }
                Some('@') => {
                    phrase = rest.to_string();
                    has_phrases = true;
                }
                _ => oov.push(word.to_string()),
            }
        }
        out.ids.push(ids);
        scores.push(score);
        thresholds.push(threshold);
        phrases.push(phrase);
    }
    if !oov.is_empty() {
        return Err(Error::Config(format!(
            "keywords use tokens missing from tokens.txt: {}",
            oov.join(", ")
        )));
    }
    if has_scores {
        out.scores = scores;
    }
    if has_thresholds {
        out.thresholds = thresholds;
    }
    if has_phrases {
        out.phrases = phrases;
    }
    Ok(out)
}

fn parse_float(field: &str, line: &str) -> Result<f32> {
    field
        .parse()
        .map_err(|_| Error::Config(format!("keywords: bad number `{field}` in `{line}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &str = "<blk> 0\n<sos/eos> 1\n<unk> 2\nZ 3\nEH1 4\nF 5\nER0 6\n";

    #[test]
    fn reads_sherpa_tokens() {
        let t = SymbolTable::parse(TOKENS).unwrap();
        assert_eq!(t.id("<blk>"), Some(0));
        assert_eq!(t.id("ER0"), Some(6));
        assert_eq!(t.symbol(4), "EH1");
        assert_eq!(t.len(), 7);
    }

    #[test]
    fn encodes_boost_threshold_and_phrase() {
        let t = SymbolTable::parse(TOKENS).unwrap();
        let k = encode_keywords("Z EH1 F ER0 :2 #0.05 @ZEPHYR\n", &t).unwrap();
        assert_eq!(k.ids, vec![vec![3, 4, 5, 6]]);
        assert_eq!(k.scores, vec![2.0]);
        assert_eq!(k.thresholds, vec![0.05]);
        assert_eq!(k.phrases, vec!["ZEPHYR".to_string()]);
    }

    #[test]
    fn options_absent_on_every_line_stay_empty() {
        let t = SymbolTable::parse(TOKENS).unwrap();
        let k = encode_keywords("Z EH1\nF ER0 @FER\n", &t).unwrap();
        assert!(k.scores.is_empty() && k.thresholds.is_empty());
        assert_eq!(k.phrases, vec![String::new(), "FER".to_string()]);
    }

    #[test]
    fn unknown_token_is_an_error() {
        let t = SymbolTable::parse(TOKENS).unwrap();
        assert!(encode_keywords("Z XX1 @BAD\n", &t).is_err());
    }
}
