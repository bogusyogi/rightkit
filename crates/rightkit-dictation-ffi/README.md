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

- `rightkit_dictation_configure_brand_homophones_json`: installs the brand homophone table `rightkit_dictation_polish` applies (call once at startup).
- `include/rightkit_dictation.h`: the one C header. `RIGHTKIT_DICTATION_KWS` selects the keyword-spotting section; define it only for a build with the `kws` feature (default).

- `kws` (cargo feature, default on): streaming keyword spotting over `rightkit_asr::kws` (`rightkit_dictation_kws_*`): bind
  ONNX Runtime once (`rightkit_dictation_kws_runtime_init`), then create spotters and streams,
  feed 16 kHz PCM and read Sherpa-shaped JSON results. The model payload is the Zipformer2
  transducer files named by `kws::ENCODER_FILE` and friends.

## Brand configuration & ownership

Host supplies one app-owned UTF-8 JSON resource to desktop & iOS. Keep product names,
homophone phrases & guards in that resource; desktop parses it & iOS passes its contents
to `rightkit_dictation_configure_brand_homophones_json` before first polish. Avoid separate
Rust constants & Swift JSON literals. This crate embeds no product table.

Schema: `{"brand":"Example","context_terms":["app"],"phrases":[{"phrase":"example name","guard":"always"}]}`.
Guards accept `"always"`, `"needs_brand_context"`, `{"literal_before":["word"]}` or
`{"literal_after":["word"]}`; phrases, context terms & guard words use lowercase.
`literal_before` preserves literal meaning when next word matches; `literal_after` checks
previous word, matching `HomophoneGuard` semantics.

Return codes: 0 installed, 1 NULL input, 2 malformed UTF-8/JSON/schema, 3 already installed.
NULL takes precedence over already installed. Failed parsing releases temporary allocations;
concurrent valid calls install exactly one table. Only winning initializer retains table
storage for process lifetime. Configuration copies input; host may release input after return.
No configuration preserves previous polish behavior.

Returned strings, including KWS errors, require `rightkit_dictation_string_free` exactly once.
Release `RightKitOwnedBytes` by value with `rightkit_dictation_owned_bytes_free`, preserving
pointer & length. Detokenization overwrites output slot; release its previous allocation first.
Borrowed input buffers remain valid for each call. Serialise KWS handle use & destruction;
release streams & spotters through their matching destroy functions.

## ABI adoption & qualification

`kws` remains default-on, preserving all 17 exports from 0.2.0; configuration adds export 18.
Without default features, 8 core exports remain; KWS exports are absent. Detokenization still
requires `rightkit-asr`, whose transitive `rightkit-onnx/session` & `ort` dependencies remain.
Disabling `kws` promises no dependency-free or ONNX-free build. Define `RIGHTKIT_DICTATION_KWS` when including
header only if linked library enables `kws`. Pointer nullability imports nullable Swift pointers;
existing caller guards & `defer` releases match ownership contract.

`include/rightkit_dictation.h` is source-reviewed. `cbindgen.toml` proposes generation settings;
header reproduction, nullability generation & automated header diff are unverified. Copy header
from exact pinned crate into each XCFramework slice when rebuilding it.

Source baseline: crates.io `rightkit-dictation-ffi` 0.2.0 archive SHA-256
`e3af4f1e359ee1ae901a14d826dc6abaa155cabe4ff340b9b3277e43d35df5ce`, checked against
registry checksum; `src/lib.rs` & `src/kws.rs` matched handoff #19 baseline byte-for-byte.
Existing C signatures & owned-bytes layout remain unchanged; KWS implementation remains unchanged.

Before adoption, qualify default & no-default feature builds, configuration rejection/repeat/race
cases, no-table behavior & shared product-table golden corpus, C/Objective-C header compilation,
cbindgen reproduction, 18/8 binary exports, both iOS XCFramework slices & Swift callers.
Rebuild XCFramework, update all headers/callers/build receipts & change dependency pin together.
