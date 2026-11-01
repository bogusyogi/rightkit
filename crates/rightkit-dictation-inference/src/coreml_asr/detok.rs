/// Detokenize a bare id stream, on the SAME policy as every other path.
///
/// One policy, `canonical_rust()`, everywhere — a second production-reachable
/// detokenizer under a different policy is how a fix lands in only one of
/// them. Callers that want raw pieces for scoring should ask the core for
/// `sentencepiece_only` explicitly instead.
pub(crate) fn detok(ids: &[usize], pieces: &[String]) -> String {
    rightkit_asr::parakeet_core::assemble_pieces(
        ids.iter()
            .filter_map(|&i| pieces.get(i).map(|s| s.as_str())),
        rightkit_asr::parakeet_core::DetokPolicy::canonical_rust(),
    )
    .text
}
