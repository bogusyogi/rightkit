//! iOS-owned WasmKit bridge for the shared `wasm_compute` model tool.

use rightkit_agent_tools::coding_tools::wasm_compute::{
    WasmComputeError, WasmComputeTool, WasmExecutionBounds, WasmExecutionRequest, WasmRuntime,
};
use std::ffi::{c_char, CStr, CString};
use std::sync::{Arc, OnceLock};

type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);

#[derive(Clone, Copy)]
struct WasmBridge {
    execute: ExecuteFn,
    free: FreeFn,
}

static WASM_BRIDGE: OnceLock<WasmBridge> = OnceLock::new();

/// Whether iOS installed the bounded Swift callback for this process.
///
/// Callers use this to withhold the model tool and capability claim until the
/// callback is real. It intentionally reports callback installation, not just
/// the presence of Rust's adapter type.
pub fn is_wasm_executor_installed() -> bool {
    WASM_BRIDGE.get().is_some()
}

/// Install host callbacks before constructing a mobile runtime.
#[no_mangle]
pub extern "C" fn host_mobile_install_wasm_executor(
    execute: Option<ExecuteFn>,
    free: Option<FreeFn>,
) -> i32 {
    let (Some(execute), Some(free)) = (execute, free) else {
        return -1;
    };
    let _ = WASM_BRIDGE.set(WasmBridge { execute, free });
    0
}

/// Exercise the production Rust tool -> iOS callback -> WasmKit path with a
/// fixed, import-free module. This has no caller-controlled executable input;
/// it exists solely for an iOS XCTest qualification of the FFI route.
#[no_mangle]
pub extern "C" fn host_mobile_wasm_executor_self_test() -> *mut c_char {
    // (module (func (export "host_compute") (result i32) i32.const 42))
    const MODULE: &[u8] = &[
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F,
        0x03, 0x02, 0x01, 0x00, 0x07, 0x15, 0x01, 0x11, 0x63, 0x6F, 0x64, 0x65, 0x72, 0x69, 0x67,
        0x68, 0x74, 0x5F, 0x63, 0x6F, 0x6D, 0x70, 0x75, 0x74, 0x65, 0x00, 0x00, 0x0A, 0x06, 0x01,
        0x04, 0x00, 0x41, 0x2A, 0x0B,
    ];
    let tool = WasmComputeTool::with_runtime(Arc::new(MobileWasmRuntime));
    let result = tool.execute_request(WasmExecutionRequest {
        module: MODULE.to_vec(),
        input: Vec::new(),
        bounds: WasmExecutionBounds {
            max_memory_bytes: 1_048_576,
            max_fuel: 100_000,
            max_wall_time_ms: 250,
        },
    });
    let response = match result {
        Ok(output) => serde_json::json!({
            "ok": true,
            "output_base64": rightkit_crypto::base64_encode(&output),
        }),
        Err(error) => serde_json::json!({"ok": false, "error": error.to_string()}),
    };
    CString::new(response.to_string())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Adapter whose bounded implementation remains inside iOS's import-free
/// pure-Swift WasmKit package.
pub struct MobileWasmRuntime;

impl WasmRuntime for MobileWasmRuntime {
    fn execute(
        &self,
        module: &[u8],
        input: &[u8],
        bounds: WasmExecutionBounds,
    ) -> Result<Vec<u8>, WasmComputeError> {
        let bridge = WASM_BRIDGE
            .get()
            .copied()
            .ok_or(WasmComputeError::RuntimeNotConfigured)?;
        let request = serde_json::json!({
            "module_base64": rightkit_crypto::base64_encode(module),
            "input_base64": rightkit_crypto::base64_encode(input),
            "max_memory_bytes": bounds.max_memory_bytes,
            "max_fuel": bounds.max_fuel,
            "max_wall_time_ms": bounds.max_wall_time_ms,
        });
        let request = CString::new(request.to_string())
            .map_err(|error| WasmComputeError::Runtime(error.to_string()))?;
        let response_ptr = unsafe { (bridge.execute)(request.as_ptr()) };
        if response_ptr.is_null() {
            return Err(WasmComputeError::Runtime(
                "iOS WasmKit bridge returned no response".into(),
            ));
        }
        let response = unsafe { CStr::from_ptr(response_ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { (bridge.free)(response_ptr) };
        let response: serde_json::Value = serde_json::from_str(&response).map_err(|error| {
            WasmComputeError::Runtime(format!("invalid bridge response: {error}"))
        })?;
        if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(WasmComputeError::Runtime(
                response
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("iOS WasmKit execution failed")
                    .to_string(),
            ));
        }
        let output = response
            .get("output_base64")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| WasmComputeError::Runtime("bridge response omitted output".into()))?;
        rightkit_crypto::base64_decode(output)
            .map_err(|error| WasmComputeError::Runtime(format!("invalid bridge output: {error}")))
    }
}
