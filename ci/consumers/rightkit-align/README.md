# rightkit-align 0.1.0 registry consumer (draft, PK21)

Standalone consumer, outside the workspace, pinning `rightkit-align =0.1.0` exactly. It exercises
the local alignment route: `ScriptSpec::from_text` and `validate`, `asr_words_from_seconds`, `align`
with default `AlignOptions`, `Alignment::anchor`, `accepted_span`, `to_contract`, and
`Coverage::meets_floor`. One designated token is accepted with its span in milliseconds and
samples. One designated token the ASR omitted is refused as `Unaligned` and does not satisfy
coverage.

State: pure computation over fixed words. No audio, no models, no files, no network, no real user
data. Run only by the CI consumer lane.

Not covered: ambiguity and `WordDiffers` rejection, clause output, `boundary_errors`, lattice
confidence values, the JSON schema and serde round trips, and the CutRight narration fixtures. PK21-2
may remove families that no consumer names, so this recipe must be revisited if the list changes.
