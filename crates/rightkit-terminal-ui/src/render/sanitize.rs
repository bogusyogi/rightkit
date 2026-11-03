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
pub fn sanitize(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
