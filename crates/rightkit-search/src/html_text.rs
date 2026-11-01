//! HTML to indexable plain text. Only visible text survives: tags, attribute
//! values, comments and the contents of `<script>`, `<style>` and `<head>` are
//! dropped, entities are decoded and whitespace is collapsed. No dependency.

const SKIPPED_BLOCKS: [&str; 3] = ["script", "style", "head"];
const INLINE_TAGS: [&str; 14] = [
    "a", "b", "i", "u", "em", "strong", "span", "font", "small", "big", "sub", "sup", "code",
    "abbr",
];

/// Visible text of `html`, entities decoded and whitespace collapsed, for an FTS index.
pub fn html_to_text(html: &str) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 3);
    let (mut i, mut run) = (0usize, 0usize);
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(&next) = b.get(i + 1) else { break };
        if !(next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?') {
            i += 1; // a literal '<' in text
            continue;
        }
        push_decoded(&mut out, &html[run..i]);
        if b[i + 1..].starts_with(b"!--") {
            i = find_ci(b, b"-->", i + 4).map_or(b.len(), |p| p + 3);
            out.push(' ');
            run = i;
            continue;
        }
        let closing = next == b'/';
        let name_start = i + 1 + usize::from(closing);
        let name_end = name_start
            + b[name_start.min(b.len())..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric())
                .count();
        let name = html[name_start.min(name_end)..name_end].to_ascii_lowercase();
        let tag_end = tag_end(b, name_end);
        i = tag_end;
        if !closing
            && SKIPPED_BLOCKS.contains(&name.as_str())
            && !(tag_end >= 2 && b[tag_end - 2] == b'/')
        {
            let close = format!("</{name}");
            i = match find_ci(b, close.as_bytes(), tag_end) {
                Some(p) => tag_end_from(b, p),
                None => b.len(),
            };
        }
        if !INLINE_TAGS.contains(&name.as_str()) {
            out.push(' ');
        }
        run = i;
    }
    if run < b.len() {
        push_decoded(&mut out, &html[run..]);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Index just past the `>` that ends the tag whose name ends at `from`,
/// ignoring any `>` inside a quoted attribute value.
fn tag_end(b: &[u8], from: usize) -> usize {
    let mut quote = 0u8;
    for (k, &c) in b.iter().enumerate().skip(from) {
        match (quote, c) {
            (0, b'"' | b'\'') if opens_value(b, k) => quote = c,
            (q, c) if q != 0 && q == c => quote = 0,
            (0, b'>') => return k + 1,
            _ => {}
        }
    }
    b.len()
}

/// A quote only opens an attribute value when it follows `=`; a stray
/// apostrophe elsewhere in a tag must not swallow the rest of the message.
fn opens_value(b: &[u8], k: usize) -> bool {
    b[..k].iter().rev().find(|c| !c.is_ascii_whitespace()) == Some(&b'=')
}

fn tag_end_from(b: &[u8], at: usize) -> usize {
    tag_end(b, at + 2)
}

fn find_ci(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| p + from)
}

fn push_decoded(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let after = &rest[p + 1..];
        let semi = after.as_bytes().iter().take(12).position(|&c| c == b';');
        if let Some(ch) = semi.and_then(|e| decode_entity(&after[..e]).map(|c| (c, e))) {
            out.push(ch.0);
            rest = &after[ch.1 + 1..];
            continue;
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::html_to_text;

    #[test]
    fn keeps_visible_text_only() {
        let html = "<html><head><title>Hidden</title><style>.x{color:red}</style></head>\
            <body class=\"zebra\"><p style=\"a>b\">Hello <b>wor</b>ld</p>\
            <script>var secret = 1;</script><!-- note --><a href=\"http://e.test/q\">Pay&nbsp;now</a>\
            <br/>caf&#233; &amp; &lt;tea&gt; 1 < 2</body></html>";
        let t = html_to_text(html);
        assert_eq!(t, "Hello world Pay now café & <tea> 1 < 2");
    }

    #[test]
    fn tolerates_unterminated_markup() {
        assert_eq!(html_to_text("a <b"), "a");
        assert_eq!(html_to_text("x <script>never closed"), "x");
        assert_eq!(
            html_to_text("fish & chips &bogus; &#xZZ;"),
            "fish & chips &bogus; &#xZZ;"
        );
    }
}
