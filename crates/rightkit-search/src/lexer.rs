//! Tokenizer for the search query language (ported from MailRight `db/search/lexer.rs`).
//!
//! Quoted phrases are atomic tokens (an `operator:` prefix directly before the
//! quote is captured), `(`/`)`/`{`/`}` are always structural, and `OR` is only
//! recognised as a standalone case-insensitive word.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Bare word or `operator:value` (no whitespace). A leading `-` stays attached.
    Word(String),
    /// `"phrase"` or `op:"phrase"`; `op_prefix` keeps the trailing colon.
    Phrase {
        op_prefix: Option<String>,
        body: String,
    },
    Or,
    LParen,
    RParen,
    LBrace,
    RBrace,
}

pub fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let structural = match c {
            '(' => Some(Token::LParen),
            ')' => Some(Token::RParen),
            '{' => Some(Token::LBrace),
            '}' => Some(Token::RBrace),
            _ => None,
        };
        if let Some(t) = structural {
            out.push(t);
            i += 1;
            continue;
        }
        let mut buf = String::new();
        let mut op_prefix = None;
        let mut saw_quote = false;
        while i < n {
            let ch = chars[i];
            if ch.is_whitespace() || "(){}".contains(ch) {
                break;
            }
            if ch == '"' {
                saw_quote = true;
                if !buf.is_empty() {
                    op_prefix = Some(buf.clone());
                }
                buf.clear();
                i += 1;
                while i < n && chars[i] != '"' {
                    buf.push(chars[i]);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
                break;
            }
            buf.push(ch);
            i += 1;
        }
        if saw_quote {
            out.push(Token::Phrase {
                op_prefix,
                body: buf,
            });
        } else if buf.eq_ignore_ascii_case("OR") {
            out.push(Token::Or);
        } else if !buf.is_empty() {
            out.push(Token::Word(buf));
        }
    }
    out
}
