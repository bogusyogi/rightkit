// A target-free key post cannot authorize fresh control, security or host policy.
// Keep legacy signatures fail closed; paste_delivery owns permitted effects.
use super::*;

pub(crate) fn send_paste_keystroke() -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_DELIVERY_HOST_REQUIRED",
        "delivery_host_required",
    ))
}

pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_DELIVERY_HOST_REQUIRED",
        "delivery_host_required",
    ))
}
