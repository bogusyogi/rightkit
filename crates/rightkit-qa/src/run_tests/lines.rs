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
