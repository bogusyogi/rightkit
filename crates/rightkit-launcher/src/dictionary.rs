//! "define word" in the launcher .
//! Windows has no dictionary service, so the entry comes from the free Dictionary API
//! (api.dictionaryapi.dev, English, no key) over https. Only the word the person typed after
//! "define " is sent, and only when they press Return on the row; nothing is requested while
//! typing, and a bare word is never looked up (the Mac's bare-word row needs an offline
//! dictionary). Called by the launcher only while `launcherDictionary` is on.

use crate::config as launcher_config;
use crate::host;
use crate::json::{self, Value};
use std::time::Duration;

const HOST: &str = "api.dictionaryapi.dev";
const TIMEOUT: Duration = Duration::from_secs(10);
/// The largest response read.
const BODY_MAX: usize = 512 * 1024;
/// The longest word asked for.
const WORD_MAX: usize = 40;
/// Characters in one line of the output view (620 px panel, 12 px text).
const LINE: usize = 78;
/// Definitions kept for each part of speech, and parts of speech kept.
const SENSES: usize = 3;
const PARTS: usize = 4;

/// What a lookup found.
pub enum Found {
    /// The entry, as lines for the output view.
    Entry(Vec<String>),
    /// The dictionary has no entry for the word.
    NoEntry,
    /// The lookup could not be made (offline, refused, unreadable).
    Unavailable,
}

/// The word to look up from what followed "define ": letters, apostrophes, hyphens and single
/// spaces, at most `WORD_MAX` characters. `None` for anything else, so odd text is never sent.
pub fn word_of(argument: &str) -> Option<String> {
    let word = argument.trim();
    let valid = !word.is_empty()
        && word.chars().count() <= WORD_MAX
        && word
            .chars()
            .all(|c| c.is_alphabetic() || matches!(c, '\'' | '-' | ' '))
        && !word.contains("  ");
    valid.then(|| word.to_lowercase())
}

/// Looks `word` up. Blocks for up to the timeout: call it from a thread.
pub fn lookup(word: &str) -> Found {
    let path = format!("/api/v2/entries/en/{}", launcher_config::encode_query(word));
    let Some(response) = host::https_get(HOST, &path, &[("Accept", "application/json")], TIMEOUT)
    else {
        return Found::Unavailable;
    };
    match response.status {
        404 => return Found::NoEntry,
        200 => {}
        _ => return Found::Unavailable,
    }
    let Some(Value::Array(entries)) = json::parse(&response.body, BODY_MAX) else {
        return Found::Unavailable;
    };
    let lines = format(word, &entries);
    if lines.is_empty() {
        Found::NoEntry
    } else {
        Found::Entry(lines)
    }
}

/// The entry as output lines: the word with its phonetic spelling, then each part of speech
/// with its first few definitions, wrapped to the panel.
fn format(word: &str, entries: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    let phonetic = entries
        .iter()
        .filter_map(|entry| entry.get("phonetic").and_then(Value::as_str))
        .find(|text| !text.is_empty());
    let mut parts = 0;
    for entry in entries {
        let Some(meanings) = entry.get("meanings").and_then(Value::as_array) else {
            continue;
        };
        for meaning in meanings {
            if parts == PARTS {
                break;
            }
            let Some(definitions) = meaning.get("definitions").and_then(Value::as_array) else {
                continue;
            };
            let senses: Vec<&str> = definitions
                .iter()
                .filter_map(|d| d.get("definition").and_then(Value::as_str))
                .filter(|d| !d.trim().is_empty())
                .take(SENSES)
                .collect();
            if senses.is_empty() {
                continue;
            }
            if parts == 0 {
                lines.push(match phonetic {
                    Some(spelling) => format!("{word}  {spelling}"),
                    None => word.to_string(),
                });
            }
            parts += 1;
            lines.push(String::new());
            let speech = meaning
                .get("partOfSpeech")
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(speech.to_string());
            for (number, sense) in senses.iter().enumerate() {
                let first = format!("{}. ", number + 1);
                for (row, text) in wrap(sense.trim(), LINE - first.len())
                    .into_iter()
                    .enumerate()
                {
                    if row == 0 {
                        lines.push(format!("{first}{text}"));
                    } else {
                        lines.push(format!("{}{text}", " ".repeat(first.len())));
                    }
                }
            }
        }
    }
    lines
}

/// `text` broken at spaces into lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
