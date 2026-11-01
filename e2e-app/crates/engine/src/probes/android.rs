//! rightkit-android: the Android bridge is compiled only for Android targets; its probe runs in the Android e2e lane (emulator),
//! never on a desktop host. On Android it checks the bridge's JNI context capture is reachable.
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    Err("needs an Android run: rightkit-android is exercised by the Android e2e lane (emulator), not this desktop engine".into())
}
