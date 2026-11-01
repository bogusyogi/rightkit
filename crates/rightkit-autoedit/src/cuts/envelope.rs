//! Audio-envelope helpers: per-10 ms RMS frames starting at source time 0.

pub const FRAME_MS: i64 = 10;

pub(crate) struct Envelope<'a> {
    rms: &'a [f32],
    thresh: f32,
}

impl<'a> Envelope<'a> {
    /// `None` when the envelope is empty or flat (no usable speech/noise contrast).
    pub(crate) fn new(rms: &'a [f32]) -> Option<Self> {
        if rms.is_empty() {
            return None;
        }
        let mut sorted: Vec<f32> = rms.iter().copied().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(|a, b| a.total_cmp(b));
        let at = |p: usize| sorted[(sorted.len() - 1) * p / 100];
        let (lo, hi) = (at(10), at(90));
        if hi - lo < 1e-6 {
            return None;
        }
        Some(Self {
            rms,
            thresh: lo + 0.2 * (hi - lo),
        })
    }

    fn loud(&self, frame: usize) -> bool {
        self.rms.get(frame).is_some_and(|v| *v > self.thresh)
    }

    /// End (ms) of the last loud frame within `[start_ms, end_ms)`.
    pub(crate) fn last_loud_end(&self, start_ms: i64, end_ms: i64) -> Option<i64> {
        let first = (start_ms.max(0) / FRAME_MS) as usize;
        let last = ((end_ms.max(0) + FRAME_MS - 1) / FRAME_MS) as usize;
        (first..last.min(self.rms.len()))
            .rev()
            .find(|f| self.loud(*f))
            .map(|f| (f as i64 + 1) * FRAME_MS)
    }

    /// Centre (ms) of the quietest whole frame inside `[lo_ms, hi_ms]`; ties go
    /// to the frame nearest `target_ms`, then the earlier one.
    pub(crate) fn quietest(&self, lo_ms: i64, hi_ms: i64, target_ms: i64) -> Option<i64> {
        let first = (lo_ms.max(0) + FRAME_MS - 1) / FRAME_MS;
        let last = hi_ms / FRAME_MS; // exclusive: frame must end by hi_ms
        let mut best: Option<(f32, i64, i64)> = None;
        for f in first..last {
            let Some(v) = self.rms.get(f as usize) else {
                break;
            };
            let centre = f * FRAME_MS + FRAME_MS / 2;
            let key = (*v, (centre - target_ms).abs(), centre);
            let better = match best {
                None => true,
                Some(b) => {
                    key.0
                        .total_cmp(&b.0)
                        .then(key.1.cmp(&b.1))
                        .then(key.2.cmp(&b.2))
                        == std::cmp::Ordering::Less
                }
            };
            if better {
                best = Some(key);
            }
        }
        best.map(|b| b.2)
    }
}
