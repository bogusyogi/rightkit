package com.orthiclabs.rightkit.android

/**
 * Thrown by [RightKitKeystore]. The native side reads only [code] (never the message), so a
 * provider message can never reach a Rust error or log. See rightkit-credentials' `BridgeError`.
 */
class RightKitBridgeException(val code: Int) : RuntimeException("rightkit bridge failure ($code)") {
    // Called from native code by name through JNI ("getCode", "()I"): the `val` above generates it.
}
