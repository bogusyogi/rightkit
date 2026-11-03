//! Shared lexical splitter for compound shell commands (POSIX, PowerShell, cmd).
//!
//! Used by owning-process protection and by permission-rule matching so that a
//! rule written for one command can never authorize a chained one. The lexer
//! is deliberately conservative: [`analyze`] reports `confident == false` for
//! anything it cannot split with certainty (command substitution, subshells,
//! file redirections, escapes, unterminated quotes, script blocks), and
//! callers must treat such commands as never auto-allowed.

fn flush_word(word: &mut String, tokens: &mut Vec<String>) {
    if !word.is_empty() {
        tokens.push(std::mem::take(word));
    }
}

fn end_stage(tokens: &mut Vec<String>, stages: &mut Vec<Vec<String>>) {
    if !tokens.is_empty() {
        stages.push(std::mem::take(tokens));
    }
}

/// Lexically split into pipelines of stages of tokens. Quotes and grouping
/// characters become separators (so nested `bash -c "kill 1"` and `$(...)`
/// are exposed); `;`, newlines, `&`, `&&` and `||` end a pipeline; a single `|`
/// ends a stage.
pub(crate) fn split_pipelines(command: &str) -> Vec<Vec<Vec<String>>> {
    let mut pipelines: Vec<Vec<Vec<String>>> = Vec::new();
    let mut stages: Vec<Vec<String>> = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut word = String::new();

    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ';' | '\n' | '\r' | '&' => {
                flush_word(&mut word, &mut tokens);
                end_stage(&mut tokens, &mut stages);
                if !stages.is_empty() {
                    pipelines.push(std::mem::take(&mut stages));
                }
            }
            '|' => {
                flush_word(&mut word, &mut tokens);
                end_stage(&mut tokens, &mut stages);
                if chars.get(i + 1) == Some(&'|') {
                    if !stages.is_empty() {
                        pipelines.push(std::mem::take(&mut stages));
                    }
                    i += 1;
                }
            }
            '\'' | '"' | '`' | '(' | ')' | '{' | '}' => {
                // `${PPID}` must stay one word.
                if (c == '{' && word == "$") || (c == '}' && word.starts_with("${")) {
                    word.push(c);
                } else {
                    flush_word(&mut word, &mut tokens);
                }
            }
            c if c.is_whitespace() => flush_word(&mut word, &mut tokens),
            c => word.push(c),
        }
        i += 1;
    }
    flush_word(&mut word, &mut tokens);
    end_stage(&mut tokens, &mut stages);
    if !stages.is_empty() {
        pipelines.push(stages);
    }
    pipelines
}

/// Result of splitting a command into its executable stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellAnalysis {
    /// Each stage as text. A lone confident stage is the trimmed raw command
    /// (so single-command rule matching is unchanged); otherwise the stage's
    /// tokens joined by single spaces.
    pub stages: Vec<String>,
    /// False when the command contains constructs that cannot be analysed
    /// with certainty. Such a command is never auto-allowed.
    pub confident: bool,
    /// Stages with substitution bodies, backtick bodies and subshell bodies
    /// hoisted out as their own stages. Used only to make deny/ask matching
    /// stricter; never to grant anything.
    pub exposed_stages: Vec<String>,
}

impl ShellAnalysis {
    /// True when the command is one plain, confidently parsed stage.
    pub fn is_single(&self) -> bool {
        self.confident && self.stages.len() == 1
    }
}

/// Split `command` into stages (separated by `;`, `&&`, `||`, `|`, `&`,
/// newlines) and decide whether the split can be trusted.
pub(crate) fn analyze(command: &str) -> ShellAnalysis {
    let trimmed = command.trim();
    let text = strip_benign_redirects(trimmed);
    let mut confident = !trimmed.is_empty() && lexically_plain(&text);
    let pipelines = split_pipelines(&text);
    let mut stages: Vec<String> = pipelines
        .iter()
        .flatten()
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| tokens.join(" "))
        .collect();
    if stages.is_empty() {
        confident = false;
    }
    if confident && stages.len() == 1 {
        stages[0] = trimmed.to_string();
    }
    let exposed = text
        .replace("$(", " ; ")
        .replace(['`', '(', ')', '{', '}'], " ; ");
    let exposed_stages = split_pipelines(&exposed)
        .iter()
        .flatten()
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| tokens.join(" "))
        .collect();
    ShellAnalysis {
        stages,
        confident,
        exposed_stages,
    }
}

/// Strip redirections that cannot write to a file (`2>&1`, `>&2`,
/// `>/dev/null`) when they end a word, so they do not look like stage
/// separators or file writes.
fn strip_benign_redirects(text: &str) -> String {
    const BENIGN: [&str; 9] = [
        "2>&1",
        "1>&2",
        ">&2",
        "2>/dev/null",
        ">/dev/null",
        "2> /dev/null",
        "> /dev/null",
        "2>$null",
        ">$null",
    ];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while !rest.is_empty() {
        let at_word_start = out.is_empty() || out.ends_with(char::is_whitespace);
        if at_word_start || rest.starts_with('>') || rest.starts_with('2') || rest.starts_with('1')
        {
            for benign in BENIGN {
                if let Some(after) = rest.strip_prefix(benign) {
                    if after.is_empty() || after.starts_with(char::is_whitespace) {
                        out.push(' ');
                        rest = after;
                        continue 'scan;
                    }
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Quote-aware scan for constructs the stage splitter cannot represent.
fn lexically_plain(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    let mut word_start = true;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
            }
            Some(q) => {
                // Inside double quotes substitution is still live.
                if c == '`' || (c == '$' && matches!(next, Some('(') | Some('{'))) {
                    return false;
                }
                if c == '\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '`' | '(' | ')' | '<' | '>' | '^' => return false,
                '$' if matches!(next, Some('(') | Some('{')) => return false,
                '\\' => {
                    if matches!(
                        next,
                        None | Some(
                            ';' | '&'
                                | '|'
                                | '<'
                                | '>'
                                | '('
                                | ')'
                                | '$'
                                | '`'
                                | '\n'
                                | '\r'
                                | '\\'
                        )
                    ) {
                        return false;
                    }
                    i += 1;
                }
                '{' | '}' if word_start => return false,
                _ => {}
            },
        }
        word_start = quote.is_none() && (c.is_whitespace() || matches!(c, ';' | '&' | '|'));
        i += 1;
    }
    quote.is_none()
}
