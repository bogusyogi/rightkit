/* rightkit-dictation-ffi 0.2.1: the one C header for the rightkit_dictation_* surface.
 *
 * Source-reviewed header; cbindgen reproduction & CI comparison remain unverified.
 * See cbindgen.toml for proposed generation settings. Copy this header into build output
 * from exact linked crate version.
 *
 * Keyword spotting: the section between `#if defined(RIGHTKIT_DICTATION_KWS)` and its `#endif`
 * is declared only when the including shell defines RIGHTKIT_DICTATION_KWS, which it does only
 * for a library built with the crate's `kws` feature (the default). A library built without
 * `kws` does not export those symbols. Detokenization still requires rightkit-asr with
 * transitive rightkit-onnx/session & ort dependencies; disabling kws does not remove them.
 *
 * Ownership: every `char *` this library returns (including `*error_out`) is released with
 * rightkit_dictation_string_free; every RightKitOwnedBytes with
 * rightkit_dictation_owned_bytes_free; a spotter with rightkit_dictation_kws_spotter_destroy; a
 * stream with rightkit_dictation_kws_stream_destroy. Pointer release functions ignore NULL;
 * owned_bytes_free accepts a by-value {NULL, 0}. Never release with free().
 * Inputs are borrowed for each call. Preserve returned pointers & lengths until release;
 * never reuse a released handle. Serialise handle use, including destruction.
 */
#ifndef RIGHTKIT_DICTATION_H
#define RIGHTKIT_DICTATION_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#if defined(__clang__)
#define RK_NULLABLE _Nullable
#else
#define RK_NULLABLE
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Owned UTF-8 bytes returned by this library. Empty is {NULL, 0}. Release with
 * rightkit_dictation_owned_bytes_free; the layout is fixed (pointer, then length). */
typedef struct RightKitOwnedBytes {
    uint8_t *RK_NULLABLE ptr;
    size_t len;
} RightKitOwnedBytes;

/* L0 polish of a finished transcript; aggressive also strips spoken fillers. NULL for NULL or
 * invalid UTF-8 input. Release the result with rightkit_dictation_string_free. */
char *RK_NULLABLE rightkit_dictation_polish(const char *RK_NULLABLE input, bool aggressive);

/* Installs the process-wide brand homophone table rightkit_dictation_polish applies. Call once
 * at startup, before the first polish. json is a NUL-terminated UTF-8 object
 * {"brand": "Name", "context_terms": ["app"], "phrases": [{"phrase": "two words", "guard": G}]}
 * with G one of "always", "needs_brand_context", {"literal_before": ["w"]},
 * {"literal_after": ["w"]}; phrases and context terms are lowercase.
 * Returns 0 installed, 1 json is NULL, 2 invalid UTF-8 or shape (nothing installed), 3 a table
 * is already installed (the first one stays). Input is copied; callers retain ownership.
 * Malformed input & losing concurrent attempts retain no allocations. One successful table
 * remains allocated for process lifetime. NULL returns 1 even after installation. */
int32_t rightkit_dictation_configure_brand_homophones_json(const char *RK_NULLABLE json);

/* Releases a char * returned by this library. NULL is ignored; double release is undefined. */
void rightkit_dictation_string_free(char *RK_NULLABLE value);

/* Conditions len mono float samples in place through the canonical ASR policy. Returns false
 * only when len > 0 and samples is NULL. */
bool rightkit_dictation_condition_for_asr(float *RK_NULLABLE samples, size_t len, uint32_t sample_rate);

/* Routes the final-ASR transform suffix grammar. *mode receives 1 (normal polish), 2 (prompt)
 * or 3 (summarize). NULL when input or mode is NULL or input is invalid UTF-8. Release the
 * result with rightkit_dictation_string_free. */
char *RK_NULLABLE rightkit_dictation_route_ai_transform(const char *RK_NULLABLE input, uint8_t *RK_NULLABLE mode);

/* UTF-8 JSON array of SentencePiece strings -> canonical Rust detokenized UTF-8. *out_text is
 * reset to {NULL, 0} first when out_text is non-NULL. Pass an empty output slot; an existing
 * allocation must be released before reuse. Returns 0 on success, 1 json_ptr is NULL with json_len != 0,
 * 2 malformed UTF-8 or JSON, 3 out_text is NULL. Release a non-empty *out_text with
 * rightkit_dictation_owned_bytes_free. */
int32_t rightkit_dictation_detokenize_json(
    const uint8_t *RK_NULLABLE json_ptr,
    size_t json_len,
    RightKitOwnedBytes *RK_NULLABLE out_text);

/* Releases bytes returned by rightkit_dictation_detokenize_json. {NULL, x} is ignored; double
 * release is undefined. */
void rightkit_dictation_owned_bytes_free(RightKitOwnedBytes bytes);

/* Call only after confirmed KWS cut exactly at its onset. NULL for NULL or invalid UTF-8.
 * Release the result with rightkit_dictation_string_free. */
char *RK_NULLABLE rightkit_dictation_strip_kws_onset_cut_residue(const char *RK_NULLABLE input);

#if defined(RIGHTKIT_DICTATION_KWS)

/* Streaming keyword spotting over rightkit_asr::kws. Shipped settings: 1 CPU thread, 0 trailing
 * blanks, keyword score 1.0, 38-frame active-path expiry. Serialise calls on one spotter and its
 * streams. Every function returns failure instead of unwinding. */
typedef struct RightKitKwsSpotter RightKitKwsSpotter;
typedef struct RightKitKwsStream RightKitKwsStream;

/* Bind ONNX Runtime once per process, before any spotter. NULL keeps a runtime already bound
 * here, else binds the statically linked onnxruntime (iOS only); otherwise pass an absolute
 * library path, which cannot replace a different binding. Returns 0 on success, -1 on failure
 * with *error_out set when error_out is not NULL (release with rightkit_dictation_string_free). */
int32_t rightkit_dictation_kws_runtime_init(const char *RK_NULLABLE ort_library_path, char *RK_NULLABLE *RK_NULLABLE error_out);

/* model_dir holds the payload's encoder, decoder, joiner and tokens.txt; keywords_file is
 * relative to model_dir unless absolute. NULL on failure with *error_out set. */
RightKitKwsSpotter *RK_NULLABLE rightkit_dictation_kws_spotter_create(
    const char *RK_NULLABLE model_dir,
    const char *RK_NULLABLE keywords_file,
    float keywords_threshold,
    int32_t max_active_paths,
    char *RK_NULLABLE *RK_NULLABLE error_out);
void rightkit_dictation_kws_spotter_destroy(RightKitKwsSpotter *RK_NULLABLE spotter);
RightKitKwsStream *RK_NULLABLE rightkit_dictation_kws_stream_create(RightKitKwsSpotter *RK_NULLABLE spotter);
void rightkit_dictation_kws_stream_destroy(RightKitKwsStream *RK_NULLABLE stream);
/* Appends 16 kHz mono samples. Returns 0 on success, -1 on failure. */
int32_t rightkit_dictation_kws_stream_accept(
    RightKitKwsStream *RK_NULLABLE stream,
    const float *RK_NULLABLE samples,
    int32_t count);
/* 1 when a chunk is ready to decode, 0 when not, -1 on failure. */
int32_t rightkit_dictation_kws_stream_is_ready(
    RightKitKwsSpotter *RK_NULLABLE spotter,
    RightKitKwsStream *RK_NULLABLE stream);
/* Decodes one chunk. Returns 0 on success, -1 on failure. */
int32_t rightkit_dictation_kws_stream_decode(
    RightKitKwsSpotter *RK_NULLABLE spotter,
    RightKitKwsStream *RK_NULLABLE stream);
/* Newest detection, reported once, as JSON:
 * {"keyword": "...", "timestamps": [seconds, 2 decimals], "tokens": [...]}.
 * keyword is empty when there is nothing new. NULL on failure. Release with
 * rightkit_dictation_string_free. */
char *RK_NULLABLE rightkit_dictation_kws_stream_result_json(
    RightKitKwsSpotter *RK_NULLABLE spotter,
    RightKitKwsStream *RK_NULLABLE stream);
/* Clears the beam and encoder cache. Returns 0 on success, -1 on failure. */
int32_t rightkit_dictation_kws_stream_reset(
    RightKitKwsSpotter *RK_NULLABLE spotter,
    RightKitKwsStream *RK_NULLABLE stream);

#endif /* RIGHTKIT_DICTATION_KWS */

#ifdef __cplusplus
}
#endif

#endif /* RIGHTKIT_DICTATION_H */
