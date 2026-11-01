//! Hand-written lexer for the trigger DSL.

use super::types::{Token, TriggerError};

/// Tokenize a trigger expression into a flat list of [`Token`]s.
pub fn tokenize(input: &str) -> Result<Vec<Token>, TriggerError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        match c {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Eq);
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '~' {
                    tokens.push(Token::Regex);
                    i += 2;
                } else {
                    return Err(TriggerError::Lex(format!(
                        "unexpected '=' at position {i}, did you mean '==' or '=~'?"
                    )));
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Ne);
                    i += 2;
                } else {
                    return Err(TriggerError::Lex(format!(
                        "unexpected '!' at position {i}, did you mean '!='?"
                    )));
                }
            }
            '"' => {
                // String literal.
                let mut s = String::new();
                i += 1; // consume opening quote
                let mut closed = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '"' {
                        closed = true;
                        i += 1; // consume closing quote
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                if !closed {
                    return Err(TriggerError::Lex("unterminated string literal".to_string()));
                }
                tokens.push(Token::Str(s));
            }
            c if is_ident_start(c) => {
                let mut word = String::new();
                while i < chars.len() && is_ident_char(chars[i]) {
                    word.push(chars[i]);
                    i += 1;
                }
                tokens.push(keyword_or_ident(word));
            }
            other => {
                return Err(TriggerError::Lex(format!(
                    "unexpected character '{other}' at position {i}"
                )));
            }
        }
    }

    Ok(tokens)
}

fn keyword_or_ident(word: String) -> Token {
    match word.as_str() {
        "and" => Token::And,
        "or" => Token::Or,
        "not" => Token::Not,
        "contains" => Token::Contains,
        "matches" => Token::Matches,
        _ => Token::Ident(word),
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '/'
}
