# rightkit-dictation-ffi

Stable C surface for native shells (iOS, macOS) over the dictation text pipeline, ASR audio
conditioning and Parakeet detokenization. Builds as `rlib` and `staticlib`.

Moved into RightKit from HeardRight (`heardright_core::ffi`). Part of the `rightkit-dictation`
package, split along the app's crate boundaries. The exported symbols were renamed from
`heardright_core_*` to `rightkit_dictation_*` (and `RightKitOwnedBytes`); native shells must
update their bindings.

- `rightkit_dictation_polish`, `rightkit_dictation_route_ai_transform`,
  `rightkit_dictation_strip_kws_onset_cut_residue`: text in, owned UTF-8 out, released with
  `rightkit_dictation_string_free`.
- `rightkit_dictation_condition_for_asr`: in-place mono `f32` conditioning.
- `rightkit_dictation_detokenize_json` / `rightkit_dictation_owned_bytes_free`: SentencePiece pieces
  to text with the canonical Rust policy.

The streaming keyword-spotter C surface (`kws_ffi`) is not included: it needs `rightkit_asr::kws`,
which is not yet in a published `rightkit-asr`.
