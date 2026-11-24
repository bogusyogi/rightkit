#ifndef RIGHTKIT_DARWIN_NOTIFY_H
#define RIGHTKIT_DARWIN_NOTIFY_H

#if defined(__APPLE__)
#include <stdint.h>
#include <dispatch/dispatch.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (^rkd_notify_handler_t)(int token);

uint32_t rkd_notify_register_dispatch(const char *name, int *out_token,
    dispatch_queue_t queue, rkd_notify_handler_t handler);
uint32_t rkd_notify_cancel(int token);
uint32_t rkd_notify_post(const char *name);
uint32_t rkd_notify_status_ok(void);

#ifdef __cplusplus
}
#endif
#endif /* __APPLE__ */
#endif /* RIGHTKIT_DARWIN_NOTIFY_H */
