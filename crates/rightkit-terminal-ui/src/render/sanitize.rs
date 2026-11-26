//! The trust boundary for render data.
//!
//! Render hints (tool name, call summary, streamed output) originate from
//! whatever a tool -- built-in or a dynamic/MCP tool we did not write --
//! chooses to put in them. That text is DATA, never renderer code: it must
//! never be able to move the cursor, change colours, clear the screen, or
//! otherwise act as if it came from our own drawing code. `sanitize` is the
//! one function every render-hint constructor routes through before storing
//! a string, so the boundary cannot be bypassed by a call site that forgets.

/// Display-only: strip the Windows verbatim prefix (`\\?\` or its
/// slash-normalised `//?/` spelling) that `std::fs::canonicalize` adds.
/// `\\?\UNC\server\share` becomes `\\server\share`; `\\?\D:\x` becomes `D:\x`.
/// Never use the result for IO or identity comparison.
pub fn display_path_str(path: &str) -> String {
    for (unc, verbatim, sep) in [(r"\\?\UNC\", r"\\?\", r"\\"), ("//?/UNC/", "//?/", "//")] {
        if let Some(rest) = path.strip_prefix(unc) {
            return format!("{sep}{rest}");
        }
        if let Some(rest) = path.strip_prefix(verbatim) {
            return rest.to_string();
        }
    }
    path.to_string()
}

/// Strips every control character except the two that are structural to
/// multi-line text (`\n`, `\t`). That removes ESC (the byte every ANSI/CSI
/// sequence starts with), BEL, CR, and the rest of the C0/C1 control range in
/// one pass, without needing to enumerate specific escape sequences a
/// hostile tool might use.
///
/// It also strips the invisible Unicode format characters that can make the
/// displayed text differ from the bytes that run (audit 2026-11-05 TUI M2):
/// bidi embeddings, overrides, isolates and marks, plus zero-width space,
/// word joiner, BOM, soft hyphen and tag characters. ZWNJ/ZWJ (U+200C/U+200D)
/// are kept only right beside a non-ASCII character other than another joiner,
/// where emoji sequences and several scripts need them; between ASCII
/// characters they would only hide a different token (`fo<ZWJ>o` looks like
/// `foo`), so they are stripped. Tag characters are kept only inside a
/// subdivision-flag sequence (U+1F3F4 followed by tags), the one emoji use.
pub fn sanitize(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let is_joiner = |c: char| c == '\u{200C}' || c == '\u{200D}';
    let is_tag = |c: char| ('\u{E0000}'..='\u{E007F}').contains(&c);
    // The nearest non-tag neighbour before each character, found in one pass.
    let mut prev_non_tag: Vec<Option<char>> = Vec::with_capacity(chars.len());
    let mut last_non_tag = None;
    for &c in &chars {
        prev_non_tag.push(last_non_tag);
        if !is_tag(c) {
            last_non_tag = Some(c);
        }
    }
    // A joiner is kept only when the character right beside it is a non-ASCII non-joiner, so a
    // run of joiners keeps at most its two ends however long it is.
    let joins_script = |i: usize| {
        // General punctuation (U+2000..U+206F: the ellipsis a cut-short text ends with, dashes,
        // quotes) joins nothing either.
        let beside = |n: Option<&char>| {
            n.is_some_and(|&n| {
                !n.is_ascii() && !is_joiner(n) && !('\u{2000}'..='\u{206F}').contains(&n)
            })
        };
        beside(i.checked_sub(1).and_then(|p| chars.get(p))) || beside(chars.get(i + 1))
    };
    chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            if is_joiner(c) {
                return joins_script(i);
            }
            if is_tag(c) {
                return prev_non_tag[i] == Some('\u{1F3F4}');
            }
            (!c.is_control() || c == '\n' || c == '\t') && !is_spoofing_format_char(c)
        })
        .map(|(_, &c)| c)
        .collect()
}

/// Bidi controls and zero-width characters that have no visible glyph but can
/// reorder or disguise the surrounding text.
fn is_spoofing_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{00AD}'
            | '\u{E0000}'..='\u{E007F}'
    )
}
