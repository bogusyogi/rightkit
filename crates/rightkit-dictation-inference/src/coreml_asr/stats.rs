// ---- helpers ----------------------------------------------------------------

// Small numeric helpers shared by the decode paths (`decode_window`,
// `decode_topk`): tensor min/max/mean for debug dumps and plain argmax.
pub(crate) fn stats(v: &[f32]) -> (f32, f32, f32) {
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    let mut sm = 0f32;
    for &x in v {
        mn = mn.min(x);
        mx = mx.max(x);
        sm += x;
    }
    (mn, mx, sm)
}

pub(crate) fn argmax(v: &[f32]) -> usize {
    let mut bi = 0usize;
    let mut bv = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > bv {
            bv = x;
            bi = i;
        }
    }
    bi
}
