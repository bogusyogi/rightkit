// Resampled-output staging helpers.
//
// `out_buf_f32` is consumed via a `start` cursor so reads do not front-drain
// the Vec on every call; `take_resampled` compacts only when the cursor
// passes half the allocation.

#[inline]
pub(super) fn pending_resampled_len(staged: &[f32], start: usize) -> usize {
    staged.len() - start
}

pub(super) fn take_resampled(
    staged: &mut Vec<f32>,
    start: &mut usize,
    max_samples: usize,
) -> Vec<f32> {
    let take = pending_resampled_len(staged, *start).min(max_samples);
    let end = *start + take;
    let mut chunk = Vec::with_capacity(take);
    chunk.extend_from_slice(&staged[*start..end]);
    *start = end;
    if *start == staged.len() {
        staged.clear();
        *start = 0;
    } else if *start >= staged.capacity() / 2 {
        let remaining = pending_resampled_len(staged, *start);
        staged.copy_within(*start.., 0);
        staged.truncate(remaining);
        *start = 0;
    }
    chunk
}
