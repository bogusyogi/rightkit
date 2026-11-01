//! Acceptance guardrails applied to provider output before it is accepted:
//! response-shape normalization, compiled-wire echo rejection, deterministic
//! dash scrubbing, focused-field echo stripping, and the numeric-value
//! preservation checks. `guardrail_replay.rs` replays this exact chain.

#[cfg(test)]
pub(crate) fn accept_output(input: &str, output: &str) -> bool {
    normalize_output(input, output).is_some()
}

pub(crate) fn normalize_output(input: &str, output: &str) -> Option<String> {
    normalize_output_with_extra(input, output, 160)
}

/// Remove focused-field text a model prepended to actual dictation. Only an
/// exact whole prefix is eligible; echo-only output is retained so this guard
/// can never erase dictation.
pub(crate) fn strip_field_text_echo(field_text: &str, input: &str, output: &str) -> String {
    fn prefix_len(text: &str, field: &str) -> Option<usize> {
        let matches = text.starts_with(field)
            || (field.is_ascii()
                && text
                    .get(..field.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(field)));
        if !matches {
            return None;
        }
        let rest = &text[field.len()..];
        if rest
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace() && c != ',')
        {
            return None;
        }
        Some(field.len())
    }

    let field = field_text.trim();
    if field.is_empty() {
        return output.to_string();
    }
    if prefix_len(input.trim_start(), field).is_some() {
        return output.to_string();
    }
    let output_start = output.trim_start();
    let Some(prefix_len) = prefix_len(output_start, field) else {
        return output.to_string();
    };
    let rest = output_start[prefix_len..].trim_start();
    let rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
    if rest.is_empty() {
        return output.to_string();
    }
    rest.to_string()
}

pub(crate) fn normalize_output_with_extra(
    input: &str,
    output: &str,
    extra_chars: usize,
) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.starts_with("```") {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("transcript:") || lower.contains("the user wants") {
        return None;
    }
    // Strip an echoed system cue if the model prepends one. The base lane cues
    // "Cleaned transcript:"; the live App lane cues "Polished output:". Both are
    // safe to strip — no genuine dictation output starts with either.
    let trimmed = if lower.starts_with("cleaned transcript:") {
        trimmed["cleaned transcript:".len()..].trim()
    } else if lower.starts_with("polished output:") {
        trimmed["polished output:".len()..].trim()
    } else {
        trimmed
    };
    if trimmed.is_empty() {
        return None;
    }
    // A provider can echo the complete compiled request envelope; it fits the
    // length budget and retains every source number, so generic guards accept
    // it. Wire control fields can never be user-facing output; reject
    // structural echoes and continue the provider ladder.
    if looks_like_compiled_prompt_echo(trimmed) {
        return None;
    }
    let max_len = input.len().saturating_add(extra_chars);
    (trimmed.len() <= max_len).then(|| trimmed.to_string())
}

/// Name the first generic acceptance rule that `normalize_output_with_extra` would fail on, for
/// diagnostics only (never the text). "numeric_guard" and "lane_filter" are reported by the lane
/// when the generic rules pass but a lane-specific filter rejects the output.
pub(crate) fn rejection_rule(input: &str, output: &str, extra_chars: usize) -> &'static str {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "empty";
    }
    if trimmed.starts_with("```") {
        return "code_fence";
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("transcript:") {
        return "transcript_prefix";
    }
    if lower.contains("the user wants") {
        return "reasoning_leak";
    }
    let trimmed = if lower.starts_with("cleaned transcript:") {
        trimmed["cleaned transcript:".len()..].trim()
    } else if lower.starts_with("polished output:") {
        trimmed["polished output:".len()..].trim()
    } else {
        trimmed
    };
    if trimmed.is_empty() {
        return "empty_after_cue";
    }
    if looks_like_compiled_prompt_echo(trimmed) {
        return "compiled_prompt_echo";
    }
    if trimmed.len() > input.len().saturating_add(extra_chars) {
        return "over_length";
    }
    if !no_unsupported_numeric_value(input, trimmed) {
        return "numeric_guard";
    }
    "lane_filter"
}

fn looks_like_compiled_prompt_echo(output: &str) -> bool {
    // Some renderers escape underscores in model text before it reaches this
    // guard. Normalize only that presentation escape, then require all three
    // envelope fields to avoid rejecting ordinary dictated mentions of schema.
    let mut normalized = output.to_string();
    while normalized.contains("\\_") {
        normalized = normalized.replace("\\_", "_");
    }
    let mut lines = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some((schema_field, schema_value)) = lines.next().and_then(|line| line.split_once(':'))
    else {
        return false;
    };
    if !schema_field.trim().eq_ignore_ascii_case("SCHEMA")
        || !matches!(
            schema_value.trim().to_ascii_uppercase().as_str(),
            "HEARDRIGHT_CLOUD_C1" | "HEARDRIGHT_CLOUD_C2" | "HEARDRIGHT_CLOUD_C3"
        )
    {
        return false;
    }
    let remaining = lines.collect::<Vec<_>>();
    remaining.iter().any(|line| {
        line.split_once(':').is_some_and(|(field, value)| {
            field.trim().eq_ignore_ascii_case("MODE")
                && value.trim().to_ascii_uppercase().starts_with('L')
        })
    }) && remaining.iter().any(|line| {
        line.split_once(':')
            .is_some_and(|(field, _)| field.trim().eq_ignore_ascii_case("SOURCE_JSON"))
    })
}

/// Deterministic dash scrub on accepted LLM output. The prompts ban em/en
/// dashes outright, but payload audits (Mac + Windows) measured a
/// 5-12% leak rate — a 27B model does not reliably obey a NEVER rule. The ban
/// is mechanical, so enforcement is mechanical: digit-adjacent dashes become
/// "to" (numeric range per the prompt's own rule), everything else becomes a
/// comma, matching the prompt's suggested replacements.
pub(crate) fn scrub_dashes(text: &str) -> String {
    if !text.contains('\u{2014}') && !text.contains('\u{2013}') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\u{2014}' && c != '\u{2013}' {
            out.push(c);
            i += 1;
            continue;
        }
        // Collapse a run of dashes and the whitespace around it.
        let mut j = i;
        while j < chars.len() && matches!(chars[j], '\u{2014}' | '\u{2013}') {
            j += 1;
        }
        while out.ends_with(' ') {
            out.pop();
        }
        let prev_digit = out.chars().last().is_some_and(|p| p.is_ascii_digit());
        let mut k = j;
        while k < chars.len() && chars[k] == ' ' {
            k += 1;
        }
        let next_digit = k < chars.len() && chars[k].is_ascii_digit();
        if prev_digit && next_digit {
            out.push_str(" to ");
            i = k;
        } else if out.is_empty() || k >= chars.len() {
            // Leading or trailing dash: drop it rather than leaving a
            // dangling comma.
            out.push(' ');
            i = k;
        } else {
            out.push_str(", ");
            i = k;
        }
    }
    let cleaned = out.replace(", ,", ",").replace(",,", ",");
    cleaned.trim().trim_end_matches(',').trim_end().to_string()
}

/// One-directional numeric-value guard (docs/POST_ASR_PROCESSING_ARCHITECTURE.md
/// §13.0/13.1/34). The compiler sends the model the RAW ASR
/// transcript rather than an L0-normalized string, so `digits_preserved`'s
/// glyph comparison rejects almost every correct polish -- "about fifteen
/// hundred users" -> "about 1,500 users" shares no digit glyphs with the
/// source at all. The invariant that actually matters is one-directional:
///
/// > The model must not introduce a numeric value unsupported by the source.
///
/// This is NOT "every source number must survive" -- an L3 summary may drop
/// numbers entirely, and glyph comparison against raw ASR is hopeless. It is
/// also not a restoration/repair system: on failure the caller rejects the
/// output and falls back to L0 (see the call sites), never rewrites.
///
/// `source_numeric_values` builds a SET of numeric values the source
/// supports, parsed from both written digits ("1,500", "2.1",
/// "20%", "2:30", "2026") and spoken number words ("fifteen hundred" -> 1500,
/// "twenty" -> 20, "two point one" -> 2.1, "two thirty" -> component values
/// 2 and 30 when followed by AM/PM). Compound components are not added
/// independently because that would license a changed value.
///
/// `output_numeric_values` extracts every numeric value the OUTPUT actually
/// asserts. A colon-delimited time is decomposed into its component values
/// (hour, minute) rather than checked as one fused value, because the source
/// side can only ever produce those components from spoken words like "two
/// thirty" -- there is no natural fused source form to match against.
/// Decimals stay fused (`2.1` is one value, not "2" and "1") so a mutated
/// decimal such as `2.1 -> 2.2` is still caught.
pub(crate) fn no_unsupported_numeric_value(source: &str, output: &str) -> bool {
    let supported = source_numeric_values(source);
    output_numeric_values(output)
        .iter()
        .all(|value| supported.contains(value))
}

fn source_numeric_values(source: &str) -> std::collections::HashSet<String> {
    let mut supported = std::collections::HashSet::new();
    for literal in numeric_literals(source) {
        insert_literal_support(&mut supported, &literal);
    }
    insert_model_size_digit_join_support(source, &mut supported);
    insert_spoken_number_support(source, &mut supported);
    supported
}

/// ASR sometimes spaces a spoken model size into individual digit tokens
/// (`27B` -> `2 7B`). License only that narrow, unit-bound reconstruction.
/// The trailing `B` makes this distinct from arbitrary values such as `2 7`,
/// which must remain separate. If the sequence follows a spaced decimal
/// (`3. 5 2 7B`), drop the decimal's fractional digit before joining.
fn insert_model_size_digit_join_support(source: &str, set: &mut std::collections::HashSet<String>) {
    let bytes = source.as_bytes();
    for unit_index in 0..bytes.len() {
        if !matches!(bytes[unit_index], b'b' | b'B')
            || bytes
                .get(unit_index + 1)
                .is_some_and(|next| next.is_ascii_alphanumeric())
        {
            continue;
        }

        let Some(last_digit_index) = unit_index.checked_sub(1) else {
            continue;
        };
        if !bytes[last_digit_index].is_ascii_digit()
            || last_digit_index
                .checked_sub(1)
                .is_some_and(|previous| bytes[previous].is_ascii_digit())
        {
            continue;
        }

        let mut positions = vec![last_digit_index];
        let mut cursor = last_digit_index;
        loop {
            let mut previous = cursor;
            while previous > 0 && matches!(bytes[previous - 1], b' ' | b'\t') {
                previous -= 1;
            }
            if previous == cursor || previous == 0 {
                break;
            }
            let digit_index = previous - 1;
            if !bytes[digit_index].is_ascii_digit()
                || digit_index
                    .checked_sub(1)
                    .is_some_and(|before| bytes[before].is_ascii_digit())
            {
                break;
            }
            positions.push(digit_index);
            cursor = digit_index;
        }
        positions.reverse();

        if positions.len() >= 3 {
            let first = positions[0];
            let mut previous = first;
            while previous > 0 && matches!(bytes[previous - 1], b' ' | b'\t') {
                previous -= 1;
            }
            if previous > 0 && bytes[previous - 1] == b'.' {
                positions.remove(0);
            }
        }
        if positions.len() < 2 {
            continue;
        }

        let joined: String = positions
            .into_iter()
            .map(|index| bytes[index] as char)
            .collect();
        set.insert(canonicalize_numeric_string(&joined));
    }
}

fn output_numeric_values(output: &str) -> Vec<String> {
    let mut values = Vec::new();
    for literal in numeric_literals(&strip_list_markers(output)) {
        values.extend(literal_required_values(&literal));
    }
    values
}

/// Maximal runs of ASCII digits, tolerating an embedded ',', '.', or ':' only
/// when both neighbors are digits -- this keeps sentence-final periods and
/// stray punctuation out of the run while still joining "1,500", "2.1", and
/// "2:30" into single literals. A dot may have horizontal ASR whitespace
/// after it, so a source "3. 8" licenses the equivalent model output "3.8".
/// Newlines never participate in that join.
pub(crate) fn numeric_literals(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if chars[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < n {
                if chars[j].is_ascii_digit()
                    || (matches!(chars[j], ',' | ':') && j + 1 < n && chars[j + 1].is_ascii_digit())
                {
                    j += 1;
                } else if chars[j] == '.' {
                    let mut next = j + 1;
                    while next < n && matches!(chars[next], ' ' | '\t') {
                        next += 1;
                    }
                    if next >= n || !chars[next].is_ascii_digit() {
                        break;
                    }
                    j = next;
                } else {
                    break;
                }
            }
            // The dot's horizontal ASR gap licenses a numeric join, but it
            // must not remain in the canonical literal key.
            out.push(
                chars[start..j]
                    .iter()
                    .filter(|c| !matches!(**c, ' ' | '\t'))
                    .collect(),
            );
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Values REQUIRED to be source-supported for one OUTPUT literal. Colon
/// literals decompose into their components (no fused source form exists);
/// everything else (including decimals) stays a single fused value so a
/// changed decimal is still caught.
fn literal_required_values(literal: &str) -> Vec<String> {
    if literal.contains(':') {
        literal
            .split(':')
            .map(|part| canonicalize_numeric_string(&part.replace(',', "")))
            .collect()
    } else {
        vec![canonicalize_numeric_string(&literal.replace(',', ""))]
    }
}

/// Values ADDED as SOURCE support for one literal found in the source text.
/// Colon-delimited times expose their components because output validation
/// checks those components. Decimals stay fused so `2.1` cannot license `2`.
fn insert_literal_support(set: &mut std::collections::HashSet<String>, literal: &str) {
    if literal.contains(':') {
        for part in literal.split(':') {
            set.insert(canonicalize_numeric_string(&part.replace(',', "")));
        }
        let joined = literal.replace(':', ".").replace(',', "");
        set.insert(canonicalize_numeric_string(&joined));
        return;
    }
    let stripped = literal.replace(',', "");
    set.insert(canonicalize_numeric_string(&stripped));
}

/// Strip leading zeros from an integer part and trailing zeros from a
/// fractional part so "1500" == "1,500" == "1500.0" and "2.10" == "2.1".
fn canonicalize_numeric_string(raw: &str) -> String {
    fn strip_leading_zeros(int_part: &str) -> String {
        let trimmed = int_part.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed.to_string()
        }
    }
    if let Some(dot) = raw.find('.') {
        let int_part = strip_leading_zeros(&raw[..dot]);
        let frac_part = raw[dot + 1..].trim_end_matches('0');
        if frac_part.is_empty() {
            int_part
        } else {
            format!("{int_part}.{frac_part}")
        }
    } else {
        strip_leading_zeros(raw)
    }
}

/// Word value for a single spelled cardinal 0-99 ("zero".."ninety"), or None.
fn spoken_word_value(word: &str) -> Option<u32> {
    Some(match word {
        "zero" => 0,
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        "seventy" => 70,
        "eighty" => 80,
        "ninety" => 90,
        _ => return None,
    })
}

fn spoken_cardinal_under_hundred(words: &[String], start: usize) -> Option<(u32, usize)> {
    let base = spoken_word_value(words.get(start)?)?;
    let mut total = base;
    let mut end = start + 1;
    if base >= 20 && base % 10 == 0 {
        if let Some(next) = words.get(end).and_then(|word| spoken_word_value(word)) {
            if (1..=9).contains(&next) {
                total += next;
                end += 1;
            }
        }
    }
    Some((total, end))
}

/// Add maximal spoken-number values found in `source`. Components of a valid
/// compound do not independently license output: "twenty five" supports 25,
/// not 20 or 5. Clock speech with an AM/PM suffix is the deliberate exception;
/// `2:30` is validated as hour and minute components.
fn insert_spoken_number_support(source: &str, set: &mut std::collections::HashSet<String>) {
    let words: Vec<String> = source
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();

    let mut i = 0;
    while i < words.len() {
        let Some((base, mut j)) = spoken_cardinal_under_hundred(&words, i) else {
            i += 1;
            continue;
        };

        if base <= 12 {
            if let Some((minute, minute_end)) = spoken_cardinal_under_hundred(&words, j) {
                if minute <= 59
                    && words
                        .get(minute_end)
                        .is_some_and(|word| matches!(word.as_str(), "am" | "pm"))
                {
                    set.insert(base.to_string());
                    set.insert(minute.to_string());
                    i = minute_end + 1;
                    continue;
                }
            }
        }

        let mut total = base;
        // <number> hundred [<number>]
        if words.get(j).is_some_and(|w| w == "hundred") {
            let mult = if total == 0 { 1 } else { total };
            total = mult * 100;
            j += 1;
            if words.get(j).is_some_and(|w| w == "and") {
                j += 1;
            }
            if let Some((add, end)) = spoken_cardinal_under_hundred(&words, j) {
                total += add;
                j = end;
            }
        }
        // <number> thousand [<number>]
        if words.get(j).is_some_and(|w| w == "thousand") {
            let mult = if total == 0 { 1 } else { total };
            total = mult * 1000;
            j += 1;
            if words.get(j).is_some_and(|w| w == "and") {
                j += 1;
            }
            if let Some((add, end)) = spoken_cardinal_under_hundred(&words, j) {
                total += add;
                j = end;
            }
        }
        // <number> point <digit>+
        let mut emitted_decimal = false;
        if words.get(j).is_some_and(|w| w == "point") {
            let mut k = j + 1;
            let mut frac = String::new();
            while let Some(d) = words.get(k).and_then(|w| spoken_word_value(w)) {
                if d > 9 {
                    break;
                }
                frac.push_str(&d.to_string());
                k += 1;
            }
            if !frac.is_empty() {
                let combined = format!("{total}.{frac}");
                set.insert(canonicalize_numeric_string(&combined));
                j = k;
                emitted_decimal = true;
            }
        }
        if !emitted_decimal {
            set.insert(total.to_string());
        }
        i = j.max(i + 1);
    }
}

/// Remove ordered-list markers from the number signature. The prompt asks the
/// model to turn spoken enumeration into numbered lines, so those formatting
/// digits must not make an otherwise valid polish fail the digit guard. Only
/// one- or two-digit markers at line start followed by punctuation and
/// whitespace are excluded; numbers in list content remain protected.
fn strip_list_markers(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let rest = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            let consumed = trimmed.len() - rest.len();
            if consumed > 0 && consumed <= 2 {
                if let Some(after) = rest.strip_prefix(['.', ')']) {
                    if after.starts_with(' ') || after.starts_with('\t') {
                        return after.trim_start().to_string();
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
