//! Global edit-distance alignment between script units and ASR units.
//!
//! Costs are integers so ties are exact. A match costs 0. A substitution costs
//! `100 - round(100 * similarity)`, so near-misses ("postgress" for
//! "postgres") are cheap. Substitutions between dissimilar words (similarity
//! below [`DISSIMILAR`]) pay an extra [`DISSIMILAR_PENALTY`]. Without it, a word
//! the ASR dropped would be absorbed by a chain of wrong substitutions, which
//! mislabels its neighbours; with it, the solver re-synchronises through one
//! deletion and one insertion. An insertion or deletion costs 100.
//!
//! Ambiguity is decided exactly. A matched pair `(s, a)` is ambiguous when some
//! other ASR unit `a'` gives an optimal alignment with the same script unit.
//! That is checked with forward and backward tables, so no sampling and no
//! heuristic threshold is involved.

pub(crate) const INDEL: u32 = 100;
/// Similarity below which two different words count as dissimilar.
const DISSIMILAR: f32 = 0.5;
const DISSIMILAR_PENALTY: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Op {
    /// Identical units.
    Match { script: usize, asr: usize },
    /// Different units; `similarity` is in 0..=1.
    Sub {
        script: usize,
        asr: usize,
        similarity: f32,
    },
    /// Script unit with no ASR counterpart.
    Del { script: usize },
    /// ASR unit with no script counterpart (ignored in output).
    Ins { asr: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Solution {
    /// Operations in script order.
    pub ops: Vec<Op>,
    pub total_cost: u32,
    /// Per script unit: true when a diagonal pair is optimal and another ASR unit ties it.
    pub ambiguous: Vec<bool>,
}

/// Levenshtein similarity on characters: `1 - distance / max_len`.
pub(crate) fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    1.0 - previous[b.len()] as f32 / longest as f32
}

pub(crate) fn pair_cost(script: &str, asr: &str) -> (u32, f32) {
    if script == asr {
        return (0, 1.0);
    }
    let sim = similarity(script, asr);
    let base = INDEL - (INDEL as f32 * sim).round() as u32;
    let penalty = if sim < DISSIMILAR {
        DISSIMILAR_PENALTY
    } else {
        0
    };
    (base + penalty, sim)
}

pub(crate) fn solve(script: &[String], asr: &[String]) -> Solution {
    let n = script.len();
    let m = asr.len();
    let stride = m + 1;

    // forward[i * stride + j]: cheapest alignment of script[..i] with asr[..j].
    let mut forward = vec![0u32; (n + 1) * stride];
    for i in 0..=n {
        for j in 0..=m {
            if i == 0 && j == 0 {
                continue;
            }
            let mut best = u32::MAX;
            if i > 0 && j > 0 {
                best = best.min(
                    forward[(i - 1) * stride + j - 1] + pair_cost(&script[i - 1], &asr[j - 1]).0,
                );
            }
            if i > 0 {
                best = best.min(forward[(i - 1) * stride + j] + INDEL);
            }
            if j > 0 {
                best = best.min(forward[i * stride + j - 1] + INDEL);
            }
            forward[i * stride + j] = best;
        }
    }

    // backward[i * stride + j]: cheapest alignment of script[i..] with asr[j..].
    let mut backward = vec![0u32; (n + 1) * stride];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            if i == n && j == m {
                continue;
            }
            let mut best = u32::MAX;
            if i < n && j < m {
                best =
                    best.min(backward[(i + 1) * stride + j + 1] + pair_cost(&script[i], &asr[j]).0);
            }
            if i < n {
                best = best.min(backward[(i + 1) * stride + j] + INDEL);
            }
            if j < m {
                best = best.min(backward[i * stride + j + 1] + INDEL);
            }
            backward[i * stride + j] = best;
        }
    }

    let total_cost = forward[n * stride + m];
    let ops = traceback(script, asr, &forward, stride);
    let ambiguous = ambiguity(script, asr, &forward, &backward, stride, total_cost, &ops);
    Solution {
        ops,
        total_cost,
        ambiguous,
    }
}

fn traceback(script: &[String], asr: &[String], forward: &[u32], stride: usize) -> Vec<Op> {
    let (mut i, mut j) = (script.len(), asr.len());
    let mut reversed = Vec::with_capacity(i.max(j));
    while i > 0 || j > 0 {
        let here = forward[i * stride + j];
        if i > 0 && j > 0 {
            let (cost, sim) = pair_cost(&script[i - 1], &asr[j - 1]);
            if here == forward[(i - 1) * stride + j - 1] + cost {
                reversed.push(if cost == 0 {
                    Op::Match {
                        script: i - 1,
                        asr: j - 1,
                    }
                } else {
                    Op::Sub {
                        script: i - 1,
                        asr: j - 1,
                        similarity: sim,
                    }
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && here == forward[(i - 1) * stride + j] + INDEL {
            reversed.push(Op::Del { script: i - 1 });
            i -= 1;
        } else {
            reversed.push(Op::Ins { asr: j - 1 });
            j -= 1;
        }
    }
    reversed.reverse();
    reversed
}

fn ambiguity(
    script: &[String],
    asr: &[String],
    forward: &[u32],
    backward: &[u32],
    stride: usize,
    total_cost: u32,
    ops: &[Op],
) -> Vec<bool> {
    let m = asr.len();
    let mut ambiguous = vec![false; script.len()];
    for op in ops {
        let (s, a) = match *op {
            Op::Match { script, asr } | Op::Sub { script, asr, .. } => (script, asr),
            _ => continue,
        };
        for alternative in 0..m {
            if alternative == a {
                continue;
            }
            let (cost, _) = pair_cost(&script[s], &asr[alternative]);
            let via = forward[s * stride + alternative]
                + cost
                + backward[(s + 1) * stride + alternative + 1];
            if via == total_cost {
                ambiguous[s] = true;
                break;
            }
        }
    }
    ambiguous
}
