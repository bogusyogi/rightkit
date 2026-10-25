//! Bounded line reading shared by the listing and the libtest summary parsers.
use std::io::BufRead;

/// Call `f` for every line of `reader` (without its `\n` or `\r\n`). A line longer than `max_len`
/// bytes is skipped whole, never truncated and never held in memory past `max_len`: a test binary
/// that prints a gigabyte without a newline cannot exhaust the runner. Invalid UTF-8 is replaced.
pub(crate) fn for_each_line<R: BufRead>(
    mut reader: R,
    max_len: usize,
    mut f: impl FnMut(&str),
) -> std::io::Result<()> {
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    let emit = |bytes: &mut Vec<u8>, f: &mut dyn FnMut(&str)| {
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        f(&String::from_utf8_lossy(bytes));
        bytes.clear();
    };
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                if !skipping && line.len() + i <= max_len {
                    line.extend_from_slice(&buf[..i]);
                    emit(&mut line, &mut f);
                }
                line.clear();
                skipping = false;
                reader.consume(i + 1);
            }
            None => {
                let n = buf.len();
                if !skipping {
                    if line.len() + n > max_len {
                        skipping = true;
                        line.clear();
                    } else {
                        line.extend_from_slice(buf);
                    }
                }
                reader.consume(n);
            }
        }
    }
    if !skipping && !line.is_empty() {
        emit(&mut line, &mut f);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8], max: usize) -> Vec<String> {
        let mut out = vec![];
        for_each_line(input, max, |l| out.push(l.to_string())).unwrap();
        out
    }

    #[test]
    fn splits_on_lf_and_crlf_and_keeps_a_final_unterminated_line() {
        assert_eq!(lines(b"a\nb\r\nc", 64), ["a", "b", "c"]);
        assert_eq!(lines(b"a\n\nb\n", 64), ["a", "", "b"]);
        assert!(lines(b"", 64).is_empty());
    }

    #[test]
    fn a_line_over_the_limit_is_skipped_whole_and_the_next_line_survives() {
        let long = "x".repeat(100);
        let input = format!("short\n{long}\nafter\n{long}");
        assert_eq!(lines(input.as_bytes(), 10), ["short", "after"]);
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let got = lines(b"ok\xff\xfe\nnext\n", 64);
        assert_eq!(got.len(), 2);
        assert!(got[0].starts_with("ok"));
        assert_eq!(got[1], "next");
    }
}
