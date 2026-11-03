// ---- helpers ----------------------------------------------------------------

// Small numeric helper shared by the decode paths (`decode_window`, `decode_topk`): plain argmax.
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
