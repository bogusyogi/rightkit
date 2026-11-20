//! Input limits for vocabulary commands. Numeric limits come from the host; lengths use byte counts.

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Longest term after trim.
    pub max_term_len: usize,
    /// Most sound-alike aliases for one term.
    pub max_aliases: usize,
    /// Longest alias after trim.
    pub max_alias_len: usize,
    /// Most terms in one import.
    pub max_import_terms: usize,
}

/// A term must be non-empty after trim, within the limit, and one line.
pub fn validate_term(term: &str, limits: &Limits) -> Result<(), String> {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return Err("term is empty".to_string());
    }
    if trimmed.len() > limits.max_term_len {
        return Err(format!(
            "term too long ({} > {})",
            trimmed.len(),
            limits.max_term_len
        ));
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err("term cannot contain newlines".to_string());
    }
    Ok(())
}

/// Trimmed aliases, each non-empty, within the limit and one line; at most `max_aliases`.
pub fn validate_aliases(aliases: &[String], limits: &Limits) -> Result<Vec<String>, String> {
    if aliases.len() > limits.max_aliases {
        return Err(format!(
            "Too many aliases ({} > {})",
            aliases.len(),
            limits.max_aliases
        ));
    }
    let mut out = Vec::with_capacity(aliases.len());
    for raw in aliases {
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() || trimmed.len() > limits.max_alias_len {
            return Err(format!(
                "Alias must be 1..={} characters after trim",
                limits.max_alias_len
            ));
        }
        if trimmed.contains('\n') || trimmed.contains('\r') {
            return Err("Alias cannot contain newlines".into());
        }
        out.push(trimmed);
    }
    Ok(out)
}

/// An import of `count` terms is refused above `max_import_terms`.
pub fn check_import_count(count: usize, limits: &Limits) -> Result<(), String> {
    if count > limits.max_import_terms {
        return Err(format!(
            "too many terms ({count} > {})",
            limits.max_import_terms
        ));
    }
    Ok(())
}
