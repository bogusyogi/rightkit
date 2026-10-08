//! Consumer-side pacing. The capture delivers at the content's rate and the
//! worker keeps frames on the requested fps grid, dropping the rest. Pure, so
//! it is unit tested with synthetic timestamps and needs no window.

pub const NS_PER_SEC: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Drop,
}

#[derive(Debug)]
pub struct Pacer {
    interval_ns: i64,
    tolerance_ns: i64,
    next_due_ns: Option<i64>,
}

impl Pacer {
    /// `fps` must be 30 or 60 (checked by the CLI).
    pub fn new(fps: u32) -> Self {
        let interval_ns = NS_PER_SEC / i64::from(fps);
        // A frame is accepted when it lands up to a third of an interval early.
        // That admits normal jitter but rejects the in-between frame of a 2:1 source.
        Self { interval_ns, tolerance_ns: interval_ns / 3, next_due_ns: None }
    }

    /// Offers one frame timestamp (host ns, monotonic, non-decreasing).
    pub fn offer(&mut self, host_ns: i64) -> Decision {
        match self.next_due_ns {
            None => {
                self.next_due_ns = Some(host_ns + self.interval_ns);
                Decision::Keep
            }
            Some(due) if host_ns + self.tolerance_ns >= due => {
                // Advance the grid by one interval. If the source stalled, re-anchor
                // to the arriving frame so the output does not burst to catch up.
                let next = (due + self.interval_ns).max(host_ns + self.interval_ns - self.tolerance_ns);
                self.next_due_ns = Some(next);
                Decision::Keep
            }
            Some(_) => Decision::Drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame timestamps for `count` frames at `source_fps`, starting at `start_ns`.
    fn ideal(source_fps: i64, count: i64, start_ns: i64) -> Vec<i64> {
        (0..count).map(|k| start_ns + k * NS_PER_SEC / source_fps).collect()
    }

    fn kept_count(fps: u32, times: &[i64]) -> usize {
        let mut pacer = Pacer::new(fps);
        times.iter().filter(|&&t| pacer.offer(t) == Decision::Keep).count()
    }

    #[test]
    fn first_frame_is_always_kept() {
        let mut pacer = Pacer::new(30);
        assert_eq!(pacer.offer(123_456_789), Decision::Keep);
    }

    #[test]
    fn sixty_source_paced_to_thirty_keeps_every_other_frame() {
        assert_eq!(kept_count(30, &ideal(60, 60, 0)), 30);
    }

    #[test]
    fn sixty_source_at_sixty_keeps_all() {
        assert_eq!(kept_count(60, &ideal(60, 60, 0)), 60);
    }

    #[test]
    fn thirty_source_at_thirty_keeps_all() {
        assert_eq!(kept_count(30, &ideal(30, 30, 0)), 30);
    }

    #[test]
    fn thirty_source_never_duplicates_for_sixty_target() {
        // The pacer only drops; a slower source is kept as delivered.
        assert_eq!(kept_count(60, &ideal(30, 30, 0)), 30);
    }

    #[test]
    fn small_jitter_does_not_change_the_thirty_fps_count() {
        // Alternating +-1 ms on a 60 Hz source.
        let times: Vec<i64> = ideal(60, 60, 0)
            .into_iter()
            .enumerate()
            .map(|(k, t)| if k % 2 == 0 { t + 1_000_000 } else { t - 1_000_000 })
            .collect();
        let kept = kept_count(30, &times);
        assert!((29..=31).contains(&kept), "kept {kept}");
    }

    #[test]
    fn stall_reanchors_without_bursting() {
        // One frame, a 1 s stall, then 60 Hz resumes. The 60 fps grid must
        // keep about one frame per 16.7 ms in the following second, not a burst.
        let mut times = vec![0];
        times.extend(ideal(60, 60, 1_000_000_000));
        let kept = kept_count(60, &times);
        assert!((59..=61).contains(&kept), "kept {kept}");
    }

    #[test]
    fn drops_are_reported_as_drop_not_error() {
        let mut pacer = Pacer::new(30);
        assert_eq!(pacer.offer(0), Decision::Keep);
        // 16 ms after the first frame is half an interval early for a 30 fps grid.
        assert_eq!(pacer.offer(16_000_000), Decision::Drop);
    }
}
