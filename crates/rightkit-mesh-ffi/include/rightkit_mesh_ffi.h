/* rightkit-mesh-ffi: C ABI of the device mesh for a phone (Joiner side of SAS pairing, plus one
 * request per stream to a paired device). Hand-written; keep in step with src/lib.rs.
 *
 * Every function that returns `char *` returns a NUL-terminated UTF-8 JSON object that the caller
 * frees with rkmesh_free_string. Success: {"ok":true, ...}. Failure: {"ok":false,"code":"...",
 * "message":"...","retryable":false}. All calls block; call them from a background thread. All
 * calls are thread-safe. Strings passed in are borrowed for the call. */
#ifndef RIGHTKIT_MESH_FFI_H
#define RIGHTKIT_MESH_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Start (or restart) the endpoint. `seed_32` points at exactly 32 bytes: the Ed25519 identity seed,
 * which is also the iroh secret key. It is read once, copied into memory that is wiped on stop, and
 * never written to disk; the caller wipes its own copy. `relay_url_or_null` is the relay to use, or
 * NULL to use the relay stored with the most recently paired computer (none: direct and local only).
 * `pair_alpn` is the protocol name the host serves its pairing on.
 * Result: {"ok":true,"endpointId":"<64 hex>","deviceId":"dev_..."} */
char *rkmesh_start(const char *state_dir, const char *relay_url_or_null, const char *device_name,
               const uint8_t *seed_32, const char *pair_alpn);

/* Begin pairing from a scanned URI (`<scheme>://pair?v=1&e=..&p=..&x=..&r=..&n=..`; any scheme).
 * Dials the host, runs the pairing up to the point where the user must type the code (no code when
 * the URI carries `s=`). A relay `r=` must be https, a dotted DNS name with an optional port, at most
 * 256 characters, else `invalid_relay` before any dial; a repeated field is `invalid_uri`.
 * Result: {"ok":true,"pairingId","hostName","hostDeviceId","hostPlatform","fingerprint",
 *          "expiresAtMs"} */
char *rkmesh_pair_begin(const char *uri);

/* Supply the code typed by the user and wait (up to the pairing's lifetime) for the host owner's
 * approval. A wrong code ends the pairing. Result: {"ok":true,"peer":{...}} */
char *rkmesh_pair_code(const char *pairing_id, const char *code);

/* Abandon a pairing. Result: {"ok":true} */
char *rkmesh_pair_cancel(const char *pairing_id);

/* Paired computers. Result: {"ok":true,"peers":[{"deviceId","endpointId","name","platform",
 * "relay","pairedAtMs"}]} */
char *rkmesh_peers(void);

/* One request on one stream to a paired computer (`peer` is its deviceId) on `alpn`: the body is
 * written as is, then the sending side is finished; the response is read to the end of the stream.
 * Request and response are each at most 262144 bytes. `timeout_secs` bounds the wait for the
 * response (the connection wait is 15 s). On success *out_body / *out_len receive a buffer to free
 * with rkmesh_free_bytes; on failure they are NULL / 0. Result: {"ok":true,"length":N} */
char *rkmesh_request(const char *peer, const char *alpn, const uint8_t *body, size_t body_len,
                 uint32_t timeout_secs, uint8_t **out_body, size_t *out_len);

/* A long-lived stream to a paired computer on `alpn`, with the computer pushing frames. Frames are a
 * 4-byte big-endian length and the payload in each direction (each at most 262144 bytes); `body` is
 * sent as the first frame. Result: {"ok":true,"streamId":N} */
char *rkmesh_stream_open(const char *peer, const char *alpn, const uint8_t *body, size_t body_len);

/* Wait up to `timeout_ms` for the next pushed frame. Result: {"ok":true,"event":"frame","length":N}
 * with *out_body / *out_len set (free with rkmesh_free_bytes), or {"ok":true,"event":"timeout"}, or
 * {"ok":true,"event":"end"} (the stream is over and closed), or an error (the stream is closed). */
char *rkmesh_stream_next(uint64_t stream_id, uint32_t timeout_ms, uint8_t **out_body, size_t *out_len);

/* Close a stream (also wakes a blocked rkmesh_stream_next with "end"). Result: {"ok":true} */
char *rkmesh_stream_close(uint64_t stream_id);

/* Last known state of a paired computer: "connecting", "direct", "relayed" or "offline" (no live
 * connection; the last attempt failed or none was made). Updated by requests and, every 3 seconds,
 * by each open stream, so a path change shows. Result: {"ok":true,"state":"..."} */
char *rkmesh_peer_state(const char *peer);

/* Ask the paired computer `peer` for its signed circle member list and the signed removals it
 * holds for this phone. The list is verified with the key this phone paired with and adds a
 * member only when an owner-check endorsement chain this phone trusts carries it, or, for a
 * device that joined by token or QR, a grant pair (an owner-check signed token grant plus the
 * admitting computer's device-key redemption, one joiner per pairing id; such a device never
 * gets can_admit); a list never removes a member it omits. A signed removal removes a host it covers (the computer that
 * introduced it, the owner of the key that endorsed it, or the host itself) and the host is not
 * added again. Retry-safe. Result: {"ok":true,"added":n,"removed":n} */
char *rkmesh_circle_sync(const char *peer);

/* Forget a paired computer on this phone. The computer's owner revokes the phone on the computer.
 * Result: {"ok":true} */
char *rkmesh_unpair(const char *peer);

/* Stop the endpoint and wipe the seed from memory. Result: {"ok":true} */
char *rkmesh_stop(void);

void rkmesh_free_string(char *s);
void rkmesh_free_bytes(uint8_t *p, size_t len);

#ifdef __cplusplus
}
#endif
#endif
