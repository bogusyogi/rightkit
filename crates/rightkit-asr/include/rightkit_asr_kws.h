/* C surface of rightkit-asr streaming keyword spotter (cargo feature `kws-ffi`).
 *
 * Ownership: every returned `char *` (error messages, result JSON) is released with
 * rightkit_asr_string_free, never free(). Handles are released once with their destroy
 * call. Every destroy call and `error_out` accept NULL.
 * Threading: serialise calls on one spotter and its streams.
 * No call unwinds: a Rust panic is reported as a failed call (-1 or NULL).
 */
#ifndef RIGHTKIT_ASR_KWS_H
#define RIGHTKIT_ASR_KWS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct RightKitAsrKwsSpotter RightKitAsrKwsSpotter;
typedef struct RightKitAsrKwsStream RightKitAsrKwsStream;

/* Release a string returned by this library. NULL is ignored. */
void rightkit_asr_string_free(char *value);

/* Bind ONNX Runtime once per process, before any spotter. NULL keeps a runtime already
 * bound here, else binds the statically linked ONNX Runtime (iOS only); otherwise pass
 * an absolute library path, which cannot replace a different binding.
 * Static binding rejects a pre-existing ort API not installed through this entry
 * point. Failed environment creation retains any successfully installed API;
 * retry NULL for that same static binding, never a different library path.
 * Returns 0 on success, -1 on failure with *error_out set when error_out is not NULL. */
int32_t rightkit_asr_kws_runtime_init(const char *ort_library_path, char **error_out);

/* model_dir holds the zipformer KWS payload (encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx,
 * decoder-epoch-13-avg-2-chunk-16-left-64.onnx, joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx,
 * tokens.txt); keywords_file is relative to model_dir unless absolute. keywords_threshold
 * applies to keyword lines with no #threshold; max_active_paths is the beam width (>= 1);
 * active_path_ttl_frames expires a beam path after that many 40 ms frames, 0 or negative
 * disables the expiry. NULL on failure with *error_out set. */
RightKitAsrKwsSpotter *rightkit_asr_kws_spotter_create(
    const char *model_dir,
    const char *keywords_file,
    float keywords_threshold,
    int32_t max_active_paths,
    int32_t active_path_ttl_frames,
    char **error_out);
/* NULL is ignored. */
void rightkit_asr_kws_spotter_destroy(RightKitAsrKwsSpotter *spotter);

/* NULL on failure. */
RightKitAsrKwsStream *rightkit_asr_kws_stream_create(RightKitAsrKwsSpotter *spotter);
/* NULL is ignored. */
void rightkit_asr_kws_stream_destroy(RightKitAsrKwsStream *stream);

/* Appends count mono 16 kHz samples in [-1, 1]. samples may be NULL only when count is 0.
 * Returns 0 on success, -1 on failure. */
int32_t rightkit_asr_kws_stream_accept(
    RightKitAsrKwsStream *stream,
    const float *samples,
    int32_t count);
/* 1 when a chunk is ready to decode, 0 when not, -1 on failure. */
int32_t rightkit_asr_kws_stream_is_ready(
    RightKitAsrKwsSpotter *spotter,
    RightKitAsrKwsStream *stream);
/* Decodes one chunk. Returns 0 on success, -1 on failure (including a stream made by
 * another spotter). */
int32_t rightkit_asr_kws_stream_decode(
    RightKitAsrKwsSpotter *spotter,
    RightKitAsrKwsStream *stream);
/* Newest detection, reported once, as JSON:
 * {"keyword": "...", "timestamps": [seconds, 2 decimals], "tokens": [...]}.
 * keyword is empty when there is nothing new. NULL on failure; release with
 * rightkit_asr_string_free. */
char *rightkit_asr_kws_stream_result_json(
    RightKitAsrKwsSpotter *spotter,
    RightKitAsrKwsStream *stream);
/* Clears the beam and encoder cache; buffered features stay. Returns 0 on success,
 * -1 on failure. */
int32_t rightkit_asr_kws_stream_reset(
    RightKitAsrKwsSpotter *spotter,
    RightKitAsrKwsStream *stream);

#ifdef __cplusplus
}
#endif

#endif /* RIGHTKIT_ASR_KWS_H */
