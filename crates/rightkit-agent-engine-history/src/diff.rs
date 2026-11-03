//! Unified diff generation — pure std, no external crates.

/// Produce a `diff -u`-style string between `old` and `new` for `path`.
/// Returns an empty string when `old == new`.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    if old == new {
        return String::new();
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let lcs = compute_lcs(&old_lines, &new_lines);
    let hunks = build_hunks(&old_lines, &new_lines, &lcs);

    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- {path}\n"));
    out.push_str(&format!("+++ {path}\n"));
    for hunk in &hunks {
        out.push_str(hunk);
    }
    out
}

/// An edit operation on the diff output.
#[derive(Debug, PartialEq)]
enum Op {
    Keep(usize, usize), // old_idx, new_idx
    Delete(usize),      // old_idx
    Insert(usize),      // new_idx
}

/// Compute LCS as a Vec of (old_idx, new_idx) pairs in order.
fn compute_lcs(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let m = old.len();
    let n = new.len();

    // dp[i][j] = LCS length for old[..i], new[..j]
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to collect the pairs.
    let mut pairs = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

const CONTEXT: usize = 3;

/// Build hunk strings from the edit script derived from the LCS.
fn build_hunks(old: &[&str], new: &[&str], lcs: &[(usize, usize)]) -> Vec<String> {
    // Build the full edit script as Ops.
    let mut ops: Vec<Op> = Vec::new();
    let mut lcs_iter = lcs.iter().peekable();
    let (mut oi, mut ni) = (0usize, 0usize);

    while oi < old.len() || ni < new.len() {
        if let Some(&&(li, lj)) = lcs_iter.peek() {
            // Emit deletions before the next keep.
            while oi < li {
                ops.push(Op::Delete(oi));
                oi += 1;
            }
            // Emit insertions before the next keep.
            while ni < lj {
                ops.push(Op::Insert(ni));
                ni += 1;
            }
            // Emit the keep.
            ops.push(Op::Keep(oi, ni));
            oi += 1;
            ni += 1;
            lcs_iter.next();
        } else {
            // Tail deletions / insertions.
            while oi < old.len() {
                ops.push(Op::Delete(oi));
                oi += 1;
            }
            while ni < new.len() {
                ops.push(Op::Insert(ni));
                ni += 1;
            }
            break;
        }
    }

    // Identify changed op indices (delete or insert).
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Keep(_, _)))
        .map(|(i, _)| i)
        .collect();

    if changed.is_empty() {
        return Vec::new();
    }

    // Group changed ops into hunk ranges (with CONTEXT lines around each change).
    let mut ranges: Vec<(usize, usize)> = Vec::new(); // (start, end) in ops index, inclusive
    for &ci in &changed {
        let start = ci.saturating_sub(CONTEXT);
        let end = (ci + CONTEXT).min(ops.len() - 1);
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 + 1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    // Render each range as a unified hunk.
    let mut hunks = Vec::new();
    for (hstart, hend) in ranges {
        // Compute old/new line numbers for the hunk header.
        let mut old_start = 1usize;
        let mut old_count = 0usize;
        let mut new_start = 1usize;
        let mut new_count = 0usize;
        let mut first_old = true;
        let mut first_new = true;

        for op in &ops[hstart..=hend] {
            match op {
                Op::Keep(oi, ni) => {
                    if first_old {
                        old_start = oi + 1;
                        first_old = false;
                    }
                    if first_new {
                        new_start = ni + 1;
                        first_new = false;
                    }
                    old_count += 1;
                    new_count += 1;
                }
                Op::Delete(oi) => {
                    if first_old {
                        old_start = oi + 1;
                        first_old = false;
                    }
                    old_count += 1;
                }
                Op::Insert(ni) => {
                    if first_new {
                        new_start = ni + 1;
                        first_new = false;
                    }
                    new_count += 1;
                }
            }
        }

        let mut hunk = format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n");
        for op in &ops[hstart..=hend] {
            match op {
                Op::Keep(oi, _) => hunk.push_str(&format!(" {}\n", old[*oi])),
                Op::Delete(oi) => hunk.push_str(&format!("-{}\n", old[*oi])),
                Op::Insert(ni) => hunk.push_str(&format!("+{}\n", new[*ni])),
            }
        }
        hunks.push(hunk);
    }

    hunks
}
