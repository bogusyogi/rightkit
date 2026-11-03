//! Addressable script tokens. A script is authored as text with [`ScriptSpec::from_text`]
//! or built by the caller. Either way every token carries the identity that
//! alignment rows echo back, and no row is ever identified by its label text.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One script token: a whitespace-delimited piece of the script, with the step it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptToken {
    /// Unique within the script. Anchors, coverage and clauses refer to tokens by this id.
    pub word_id: String,
    /// The narration step this token belongs to. A change of step always ends a clause.
    pub step_id: String,
    /// Position of the token in the whole script. Strictly increasing in script order.
    pub token_index: u32,
    /// The token as written. A newline in the text ends its clause; the newline is not spoken.
    pub text: String,
}

/// The script as tokens, in script order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptSpec {
    pub tokens: Vec<ScriptToken>,
}

/// A script that cannot be aligned because its identities are ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptError {
    /// A token has an empty `word_id`.
    EmptyWordId { token_index: u32 },
    /// Two tokens share a `word_id`.
    DuplicateWordId(String),
    /// `token_index` does not increase in script order at this token.
    TokenIndexNotIncreasing { word_id: String },
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::EmptyWordId { token_index } => {
                write!(f, "empty word_id at token_index {token_index}")
            }
            ScriptError::DuplicateWordId(word_id) => write!(f, "duplicate word_id {word_id:?}"),
            ScriptError::TokenIndexNotIncreasing { word_id } => {
                write!(f, "token_index does not increase at word_id {word_id:?}")
            }
        }
    }
}

impl std::error::Error for ScriptError {}

impl ScriptSpec {
    /// Splits `text` on whitespace into tokens of one step. Ids are `w{index}`
    /// and `token_index` is the zero-based position. A newline in the whitespace
    /// after a token is kept as a trailing `\n` on that token's text, so the
    /// token ends its clause (see [`crate::Clause`]).
    pub fn from_text(step_id: &str, text: &str) -> ScriptSpec {
        let mut pieces: Vec<String> = Vec::new();
        let mut word = String::new();
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !word.is_empty() {
                    pieces.push(std::mem::take(&mut word));
                }
                if ch == '\n' {
                    if let Some(last) = pieces.last_mut() {
                        if !last.ends_with('\n') {
                            last.push('\n');
                        }
                    }
                }
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            pieces.push(word);
        }

        let tokens = pieces
            .into_iter()
            .enumerate()
            .map(|(index, text)| ScriptToken {
                word_id: format!("w{index}"),
                step_id: step_id.to_string(),
                token_index: u32::try_from(index).expect("script has more than u32::MAX tokens"),
                text,
            })
            .collect();
        ScriptSpec { tokens }
    }

    /// Checks that word ids are non-empty and unique and that token indices increase.
    pub fn validate(&self) -> Result<(), ScriptError> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut previous: Option<u32> = None;
        for token in &self.tokens {
            if token.word_id.is_empty() {
                return Err(ScriptError::EmptyWordId {
                    token_index: token.token_index,
                });
            }
            if !seen.insert(token.word_id.as_str()) {
                return Err(ScriptError::DuplicateWordId(token.word_id.clone()));
            }
            if previous.is_some_and(|p| token.token_index <= p) {
                return Err(ScriptError::TokenIndexNotIncreasing {
                    word_id: token.word_id.clone(),
                });
            }
            previous = Some(token.token_index);
        }
        Ok(())
    }
}

/// Whether a token ends a clause on its own text: a newline, or script
/// punctuation (`. , ; : ? !`) after any closing quotes or brackets.
pub(crate) fn ends_clause(text: &str) -> bool {
    if text.contains('\n') {
        return true;
    }
    let trimmed = text.trim_end_matches(|c: char| {
        matches!(c, '"' | '\'' | '\u{201D}' | '\u{2019}' | ')' | ']' | '}')
    });
    trimmed.ends_with(['.', ',', ';', ':', '?', '!'])
}
