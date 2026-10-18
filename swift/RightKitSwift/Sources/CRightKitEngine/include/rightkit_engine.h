/*
 * rightkit_engine.h: the C ABI of rightkit-engine-native.
 *
 * Hand-written. Every rkp_* declaration, struct and constant is transcribed from
 * rightkit-job-queue 0.3.1, src/ffi.rs (feature "ffi", ABI_VERSION 1, SCHEMA_VERSION 1); the
 * rke_* function is defined by this package's native/src/lib.rs. Change this file only together
 * with a new pinned rightkit-job-queue version, and check rkp_versions().abi on start.
 *
 * Rules that come from ffi.rs:
 *  - Handles (host, connection) are opaque u64 ids that never repeat in a process; a released or
 *    stale one is refused with RKP_STALE_HANDLE. Release calls are idempotent.
 *  - Rust owns every RkpBuf it returns: free it with rkp_buf_free (zeroes it; freeing twice is a
 *    no-op). Out parameters are zeroed on entry once validated, so a failed call leaves no stale
 *    buffer or handle behind.
 *  - Every blocking call takes a relative timeout_ms fixed at entry.
 *  - The event callback gets bytes borrowed for that call only. It must not block and must not
 *    call back into this ABI. When rkp_host_call returns, no callback is running or can start.
 *  - RKP_NATIVE errors carry JSON {"code","message","retryable"} in the error message.
 */
#ifndef RIGHTKIT_ENGINE_H
#define RIGHTKIT_ENGINE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#pragma clang assume_nonnull begin

/* ---- limits (informational; enforced by the library) ---------------------------------- */

static const uint32_t RKP_ABI_VERSION = 1;      /* ffi.rs ABI_VERSION */
static const uint32_t RKP_SCHEMA_VERSION = 1;   /* ffi.rs SCHEMA_VERSION */
static const uint32_t RKP_MAX_HOSTS = 8;
static const uint32_t RKP_MAX_CONNECTIONS = 16;
static const size_t RKP_MAX_INPUT_BYTES = 16u * 1024u * 1024u;
static const size_t RKP_MAX_EVENT_BYTES = 1024u * 1024u;
static const size_t RKP_MAX_EVENTS_PER_CALL = 4096;

/* ---- status codes (i32), the return value of every rkp_* call ------------------------- */

static const int32_t RKP_OK = 0;
static const int32_t RKP_INVALID_ARGUMENT = 1;
static const int32_t RKP_NOT_READY = 2;
static const int32_t RKP_DEADLINE = 3;
static const int32_t RKP_SHUTTING_DOWN = 4;
static const int32_t RKP_INCOMPATIBLE = 5;
static const int32_t RKP_STALE_HANDLE = 6;
/* An engine or service error; the message is JSON {"code","message","retryable"}. */
static const int32_t RKP_NATIVE = 7;
static const int32_t RKP_PANIC = 8;
static const int32_t RKP_NOT_CONFIGURED = 9;
static const int32_t RKP_STALE_GENERATION = 10;
static const int32_t RKP_TOO_MANY_HOSTS = 11;
static const int32_t RKP_TOO_MANY_CONNECTIONS = 12;

/* ---- resume policy (u32), the `resume` argument of rkp_host_call ---------------------- */

static const uint32_t RKP_RESUME = 0; /* the job survives a detached wait */
static const uint32_t RKP_NEVER = 1;  /* the job dies with its caller */

/* ---- lifecycle codes (u32 from rkp_host_snapshot, i32 from rkp_host_lifecycle) -------- */

static const uint32_t RKP_BOOTING = 0;
static const uint32_t RKP_READY = 1;
static const uint32_t RKP_RECOVERING = 2;
static const uint32_t RKP_DOWN = 3;

/* ---- shutdown outcome codes (u32), `out_outcome` of rkp_host_shutdown ----------------- */

static const uint32_t RKP_SHUTDOWN_NO_ENGINE = 0;
static const uint32_t RKP_SHUTDOWN_ALREADY = 1;
static const uint32_t RKP_SHUTDOWN_EXITED = 2;
static const uint32_t RKP_SHUTDOWN_KILLED = 3;
static const uint32_t RKP_SHUTDOWN_UNCONFIRMED = 4;

/* ---- structs (all #[repr(C)] in ffi.rs) ------------------------------------------------ */

typedef struct RkpVersions {
    uint32_t abi;
    uint32_t contract;
    uint32_t schema;
} RkpVersions;

/* Rust-allocated bytes; release with rkp_buf_free. ptr is null when len is zero. */
typedef struct RkpBuf {
    uint8_t *_Nullable ptr;
    size_t len;
} RkpBuf;

typedef struct RkpError {
    int32_t code;
    RkpBuf message;
} RkpError;

/* Event callback: borrowed bytes, valid until it returns. */
typedef void (*RkpEventFn)(void *_Nullable ctx, const uint8_t *_Nullable ptr, size_t len);

/* ---- entry points (ffi.rs, #[no_mangle] extern "C") ------------------------------------ */

/* Versions of this ABI, the job-queue wire contract and the schema. */
int32_t rkp_versions(RkpVersions *out);

/* Register the complete initialization snapshot (JSON) before rkp_host_start. A generation older
 * than the registered one, or the same generation with different bytes, is refused. */
int32_t rkp_register_snapshot(const uint8_t *_Nullable snapshot, size_t len, uint64_t generation,
                              RkpError *_Nullable err);

/* Start the host; returns once its engine is Ready with the registered snapshot replayed. */
int32_t rkp_host_start(uint64_t *out_host, RkpError *_Nullable err);

/* Snapshot generation, lifecycle code and a JSON status payload (free it with rkp_buf_free). */
int32_t rkp_host_snapshot(uint64_t host, uint64_t *out_generation, uint32_t *out_lifecycle,
                          RkpBuf *out_payload, RkpError *_Nullable err);

/* Call an engine method under one deadline. resume is RKP_RESUME or RKP_NEVER. on_event may be
 * null. On success `out` holds the result JSON. */
int32_t rkp_host_call(uint64_t host, const uint8_t *_Nullable method, size_t method_len,
                      const uint8_t *_Nullable params, size_t params_len, uint32_t resume,
                      uint64_t timeout_ms, RkpEventFn _Nullable on_event, void *_Nullable ctx,
                      RkpBuf *out, RkpError *_Nullable err);

/* Block until the lifecycle differs from `since` (-1 = any) or timeout_ms passes (RKP_DEADLINE).
 * out_lifecycle is -1 when the host was released. */
int32_t rkp_host_lifecycle(uint64_t host, int32_t since, uint64_t timeout_ms,
                           int32_t *out_lifecycle, RkpError *_Nullable err);

/* Shut the host down, answering within budget_ms. The host stays registered until released. */
int32_t rkp_host_shutdown(uint64_t host, uint64_t budget_ms, uint32_t *out_outcome,
                          uint64_t *out_unreaped, RkpError *_Nullable err);

/* Connect the service client for this host (not used by a supervised engine). */
int32_t rkp_service_connect(uint64_t host, uint64_t timeout_ms, uint64_t *out_conn,
                            RkpError *_Nullable err);

/* Call a service method under one deadline. On success `out` holds the result JSON. */
int32_t rkp_service_call(uint64_t conn, const uint8_t *_Nullable method, size_t method_len,
                         const uint8_t *_Nullable params, size_t params_len, uint64_t timeout_ms,
                         RkpBuf *out, RkpError *_Nullable err);

/* Release a host handle. Idempotent. Stops the engine in the background if not shut down. */
void rkp_release_host(uint64_t host);

/* Release a service connection handle. Idempotent. */
void rkp_release_connection(uint64_t conn);

/* Free a buffer this library returned and zero it. Null is a no-op; freeing twice is a no-op. */
void rkp_buf_free(RkpBuf *_Nullable buf);

/* ---- rke_*: defined by rightkit-engine-native (native/src/lib.rs), not by ffi.rs -------- */

/* Install the engine every later rkp_host_start spawns (process-wide; replaces an earlier one).
 *
 *  engine_path      the executable, UTF-8, not empty.
 *  args, argc       its arguments; args may be null when argc is 0.
 *  env_keys,        environment variable names passed through beyond the default allowlist
 *  env_count        (PATH, HOME, USER, LANG, TMPDIR, USERPROFILE, LOCALAPPDATA, ...); the engine
 *                   otherwise starts with a cleared environment. May be null when the count is 0.
 *  snapshot_method  the engine method that receives the registered snapshot; null or empty
 *                   means "init".
 *  start_budget_ms  how long rkp_host_start waits for the first Ready; 0 means 60000.
 *
 * Returns RKP_OK, RKP_INVALID_ARGUMENT or RKP_PANIC; err (when not null) carries the message. */
int32_t rke_install_engine(const char *engine_path, const char *_Nullable const *_Nullable args,
                           size_t argc, const char *_Nullable const *_Nullable env_keys,
                           size_t env_count, const char *_Nullable snapshot_method,
                           uint32_t start_budget_ms, RkpError *_Nullable err);

#pragma clang assume_nonnull end

#ifdef __cplusplus
}
#endif

#endif /* RIGHTKIT_ENGINE_H */
