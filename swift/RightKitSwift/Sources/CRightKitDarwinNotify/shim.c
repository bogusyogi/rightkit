#include "rightkit_darwin_notify.h"

#if defined(__APPLE__)
// Keep SDK notify declarations private to this translation unit. Swift imports
// only our prefixed entry points, avoiding ownership of the SDK's notify.h.
#include <notify.h>

uint32_t rkd_notify_register_dispatch(const char *name, int *out_token,
    dispatch_queue_t queue, rkd_notify_handler_t handler)
{
    return notify_register_dispatch(name, out_token, queue, handler);
}

uint32_t rkd_notify_cancel(int token)
{
    return notify_cancel(token);
}

uint32_t rkd_notify_post(const char *name)
{
    return notify_post(name);
}

uint32_t rkd_notify_status_ok(void)
{
    return NOTIFY_STATUS_OK;
}
#endif /* __APPLE__ */
