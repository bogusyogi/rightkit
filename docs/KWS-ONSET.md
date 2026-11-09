# KWS onset estimators (rightkit-asr)

Owner: the HeardRight KWS lane (Adrian assigned the KWS objective; Fab2 disclaimed rightkit-asr; the Codex chat is advisory only). Issue: rightkit/rightkit#1273. Status: source only until the owner-approved compile, test and replay run.

## D4 fricative onset estimator

Default off, diagnostic. `OnsetConfig.fricative` (`FricativeParams`: window 250 ms, floor 500 ms, rise 1.0, minimum run 40 ms, vowel window 60 ms, tilt margin 1.0, every value settable) adds `KeywordOnset.fricative`; it never replaces `emission`, `aligned`, `posterior` or `acoustic`. A hiss run counts only when a vowel entry starts within one frame after it and inside the vowel window before the anchor (a temporal-association heuristic, not proof of the same occurrence); a run shorter than the minimum is dropped before ambiguity is counted. It abstains with a named reason (`invalid_evidence`, `floor_short`, `before_epoch`, `no_transition`, `run_too_short`, `ambiguous_transitions`, `window_limit`) and never fabricates a boundary. `KwsSwitches.fricative_strict` (default off) returns no boundary when D4 has no candidate instead of falling back to emission. Qualification is by native replay on the frozen scorer against the matched baseline in the same binary; the owner corpus is evaluation only.
