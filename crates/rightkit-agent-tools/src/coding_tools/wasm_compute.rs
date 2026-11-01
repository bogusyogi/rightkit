//! ENG-79: bounded WASM compute tool seam.
//!
//! External-artifact gap: `host_tools` has no WASM runtime dependency, so
//! this module does not pretend to execute a module. It validates every bound,
//! exposes a bounded runtime seam, and returns [`WasmComputeError::RuntimeNotConfigured`]
//! until the owning runtime dependency and its mobile packaging are supplied.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::sync::Arc;
use thiserror::Error;

/// Largest linear-memory budget accepted by the tool, preventing a caller from
/// asking a future runtime to allocate an unbounded address space.
pub const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
/// Largest fuel/step budget accepted by the tool, preventing unbounded compute
/// even when a runtime's default budget changes.
pub const MAX_FUEL: u64 = 10_000_000;
/// Largest wall-clock budget accepted by the tool, keeping runtime watchdogs
/// finite when a runtime implementation is eventually supplied.
pub const MAX_WALL_TIME_MS: u64 = 30_000;
/// Largest module accepted before a runtime is asked to compile it.
pub const MAX_MODULE_BYTES: usize = 2 * 1024 * 1024;

/// Typed failures for validation and for the deliberately unconfigured runtime
/// seam. Keeping the unavailable path distinct prevents callers from treating
/// a missing execution artifact as a successful computation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WasmComputeError {
    /// The request shape or decoded payload cannot be accepted.
    #[error("wasm_compute invalid request: {0}")]
    InvalidRequest(String),
    /// A module or input field is not valid base64.
    #[error("wasm_compute invalid base64 in {field}: {reason}")]
    InvalidBase64 { field: String, reason: String },
    /// A caller requested more of one resource than this tool permits.
    #[error("wasm_compute {resource} bound {requested} exceeds maximum {maximum}")]
    BoundExceeded {
        /// Resource whose ceiling was exceeded.
        resource: &'static str,
        /// Requested resource ceiling.
        requested: u64,
        /// Tool-wide maximum resource ceiling.
        maximum: u64,
    },
    /// A zero limit would fail to bound execution meaningfully.
    #[error("wasm_compute {resource} bound must be greater than zero")]
    ZeroBound {
        /// Resource whose limit was zero.
        resource: &'static str,
    },
    /// No real WASM engine is packaged for this target.
    #[error("wasm_compute runtime is not configured; no WASM execution artifact is available")]
    RuntimeNotConfigured,
    /// A configured runtime rejected or failed the bounded invocation.
    #[error("wasm_compute runtime failed: {0}")]
    Runtime(String),
}

/// Resource limits passed to a WASM implementation. All three values are
/// required so a runtime cannot accidentally inherit an unbounded default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmExecutionBounds {
    /// Maximum linear memory the runtime may allocate.
    pub max_memory_bytes: u64,
    /// Maximum fuel/steps the runtime may consume.
    pub max_fuel: u64,
    /// Maximum wall-clock duration the runtime may permit.
    pub max_wall_time_ms: u64,
}

impl WasmExecutionBounds {
    /// Construct limits only when memory, fuel, and wall-clock ceilings are
    /// individually valid for a hostile module.
    pub fn new(
        max_memory_bytes: u64,
        max_fuel: u64,
        max_wall_time_ms: u64,
    ) -> Result<Self, WasmComputeError> {
        check_bound("memory", max_memory_bytes, MAX_MEMORY_BYTES)?;
        check_bound("fuel", max_fuel, MAX_FUEL)?;
        check_bound("wall_time_ms", max_wall_time_ms, MAX_WALL_TIME_MS)?;
        Ok(Self {
            max_memory_bytes,
            max_fuel,
            max_wall_time_ms,
        })
    }
}

fn check_bound(
    resource: &'static str,
    requested: u64,
    maximum: u64,
) -> Result<(), WasmComputeError> {
    if requested == 0 {
        return Err(WasmComputeError::ZeroBound { resource });
    }
    if requested > maximum {
        return Err(WasmComputeError::BoundExceeded {
            resource,
            requested,
            maximum,
        });
    }
    Ok(())
}

/// A decoded module invocation handed to the bounded runtime seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExecutionRequest {
    /// Raw WASM module bytes, bounded before runtime dispatch.
    pub module: Vec<u8>,
    /// Raw input bytes supplied to the module.
    pub input: Vec<u8>,
    /// Validated memory, fuel, and wall-clock ceilings.
    pub bounds: WasmExecutionBounds,
}

/// Runtime seam for a real WASM engine supplied by the owning host.
///
/// Implementations must install a memory limiter, fuel/step accounting, and a
/// wall-clock watchdog from `bounds` before entering module code. The narrow
/// bounded signature prevents an implementation from silently accepting a
/// request without all three controls.
pub trait WasmRuntime: Send + Sync {
    /// Execute one module invocation under all supplied resource ceilings.
    fn execute(
        &self,
        module: &[u8],
        input: &[u8],
        bounds: WasmExecutionBounds,
    ) -> Result<Vec<u8>, WasmComputeError>;
}

/// Whether a WASM execution runtime artifact is packaged for this host.
///
/// Always `false` today: `host_tools` ships the bounded seam without a
/// runtime dependency, so `wasm_compute` must never reach model-callable
/// advertising — the registry consults this probe before registering the
/// tool, and unavailable tools are omitted from capabilities, tool_search
/// metadata, and request schemas alike. Flip this when the owning runtime
/// dependency and its packaging land; until then the seam stays testable
/// through [`WasmComputeTool::new`] / [`WasmComputeTool::with_runtime`].
pub fn wasm_runtime_configured() -> bool {
    false
}

/// Tool surface for sandboxed WASM computation.
///
/// The default instance has no runtime because this crate intentionally does
/// not add a WASM dependency. Tests and a future host may inject an engine
/// through [`Self::with_runtime`] without changing the tool protocol.
pub struct WasmComputeTool {
    runtime: Option<Arc<dyn WasmRuntime>>,
}

impl WasmComputeTool {
    /// Create the safe, explicitly unavailable tool used when no runtime is
    /// packaged for the current target.
    pub fn new() -> Self {
        Self { runtime: None }
    }

    /// Attach a runtime that owns the actual sandbox implementation.
    pub fn with_runtime(runtime: Arc<dyn WasmRuntime>) -> Self {
        Self {
            runtime: Some(runtime),
        }
    }

    /// Validate and dispatch a decoded request without manufacturing a result
    /// when the external WASM artifact is absent.
    pub fn execute_request(
        &self,
        mut request: WasmExecutionRequest,
    ) -> Result<Vec<u8>, WasmComputeError> {
        request.bounds = WasmExecutionBounds::new(
            request.bounds.max_memory_bytes,
            request.bounds.max_fuel,
            request.bounds.max_wall_time_ms,
        )?;
        if request.module.len() > MAX_MODULE_BYTES {
            return Err(WasmComputeError::InvalidRequest(format!(
                "module is {} bytes; maximum is {MAX_MODULE_BYTES}",
                request.module.len()
            )));
        }
        if !request.input.is_empty() {
            return Err(WasmComputeError::InvalidRequest(
                "input_base64 is reserved and must decode to an empty payload".into(),
            ));
        }
        let Some(runtime) = &self.runtime else {
            return Err(WasmComputeError::RuntimeNotConfigured);
        };
        runtime.execute(&request.module, &request.input, request.bounds)
    }
}

impl Default for WasmComputeTool {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_request(args: &Value) -> Result<WasmExecutionRequest, WasmComputeError> {
    let module_base64 = args
        .get("module_base64")
        .and_then(Value::as_str)
        .ok_or_else(|| WasmComputeError::InvalidRequest("module_base64 is required".into()))?;
    let module = decode_base64("module_base64", module_base64)?;
    let input = match args.get("input_base64") {
        None => Vec::new(),
        Some(value) => {
            let encoded = value.as_str().ok_or_else(|| {
                WasmComputeError::InvalidRequest("input_base64 must be a string".into())
            })?;
            decode_base64("input_base64", encoded)?
        }
    };
    let bounds = WasmExecutionBounds::new(
        required_u64(args, "max_memory_bytes")?,
        required_u64(args, "max_fuel")?,
        required_u64(args, "max_wall_time_ms")?,
    )?;
    Ok(WasmExecutionRequest {
        module,
        input,
        bounds,
    })
}

fn required_u64(args: &Value, key: &str) -> Result<u64, WasmComputeError> {
    args.get(key).and_then(Value::as_u64).ok_or_else(|| {
        WasmComputeError::InvalidRequest(format!("{key} is required and must be an integer"))
    })
}

fn decode_base64(field: &str, encoded: &str) -> Result<Vec<u8>, WasmComputeError> {
    rightkit_crypto::base64_decode(encoded).map_err(|error| WasmComputeError::InvalidBase64 {
        field: field.to_string(),
        reason: error.to_string(),
    })
}

#[async_trait::async_trait]
impl Tool for WasmComputeTool {
    fn name(&self) -> &str {
        "wasm_compute"
    }

    fn description(&self) -> &str {
        "Run a module under explicit memory, fuel/step, and wall-clock bounds; unavailable when no WASM runtime is configured."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["module_base64", "max_memory_bytes", "max_fuel", "max_wall_time_ms"],
            "properties": {
                "module_base64": {"type": "string", "description": "Base64-encoded WASM module"},
                "input_base64": {"type": "string", "maxLength": 0, "description": "Reserved; this import-free compute ABI accepts no input."},
                "max_memory_bytes": {"type": "integer", "minimum": 1, "maximum": MAX_MEMORY_BYTES},
                "max_fuel": {"type": "integer", "minimum": 1, "maximum": MAX_FUEL, "description": "Fuel/step ceiling"},
                "max_wall_time_ms": {"type": "integer", "minimum": 1, "maximum": MAX_WALL_TIME_MS, "description": "Wall-clock watchdog ceiling"}
            }
        })
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::ProcessControl)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Sandboxed execution is still process control: gate it before parsing
        // or running anything, matching the dev_tunnel_proxy precedent.
        let verdict = rightkit_agent_policy::PermissionEngine::new().evaluate(
            rightkit_agent_policy::CapabilityClass::ProcessControl,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            false,
        );
        match verdict.decision {
            rightkit_agent_policy::PermissionDecision::Allow => {}
            rightkit_agent_policy::PermissionDecision::Ask => {
                return ToolResult::error(format!("Tool requires approval: {}", verdict.reason))
            }
            rightkit_agent_policy::PermissionDecision::Deny => {
                return ToolResult::error(format!("Permission denied: {}", verdict.reason))
            }
        }

        let request = match parse_request(&args) {
            Ok(request) => request,
            Err(error) => return ToolResult::error(error.to_string()),
        };
        match self.execute_request(request) {
            Ok(output) => ToolResult::ok(
                json!({"output_base64": rightkit_crypto::base64_encode(&output)}).to_string(),
            ),
            Err(error) => ToolResult::error(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_bounds(bounds: WasmExecutionBounds) -> WasmExecutionRequest {
        WasmExecutionRequest {
            module: vec![0, 97, 115, 109],
            input: Vec::new(),
            bounds,
        }
    }

    #[test]
    fn bounds_reject_zero_and_over_limit_for_each_resource() {
        assert_eq!(
            WasmExecutionBounds::new(0, 1, 1),
            Err(WasmComputeError::ZeroBound { resource: "memory" })
        );
        assert_eq!(
            WasmExecutionBounds::new(1, MAX_FUEL + 1, 1),
            Err(WasmComputeError::BoundExceeded {
                resource: "fuel",
                requested: MAX_FUEL + 1,
                maximum: MAX_FUEL,
            })
        );
        assert_eq!(
            WasmExecutionBounds::new(1, 1, MAX_WALL_TIME_MS + 1),
            Err(WasmComputeError::BoundExceeded {
                resource: "wall_time_ms",
                requested: MAX_WALL_TIME_MS + 1,
                maximum: MAX_WALL_TIME_MS,
            })
        );
    }

    #[test]
    fn tool_rechecks_bounds_before_runtime_dispatch() {
        let invalid = WasmExecutionBounds {
            max_memory_bytes: 0,
            max_fuel: 100,
            max_wall_time_ms: 100,
        };
        let error = WasmComputeTool::new()
            .execute_request(request_with_bounds(invalid))
            .expect_err("invalid bounds must never reach a runtime");
        assert_eq!(error, WasmComputeError::ZeroBound { resource: "memory" });
    }

    #[test]
    fn missing_runtime_path_returns_typed_error() {
        let bounds = WasmExecutionBounds::new(1024, 100, 100).expect("valid bounds");
        let error = WasmComputeTool::new()
            .execute_request(request_with_bounds(bounds))
            .expect_err("default tool must not fake WASM execution");
        assert_eq!(error, WasmComputeError::RuntimeNotConfigured);
    }

    #[test]
    fn nonempty_input_never_reaches_a_runtime() {
        let bounds = WasmExecutionBounds::new(1024, 100, 100).expect("valid bounds");
        let error = WasmComputeTool::new()
            .execute_request(WasmExecutionRequest {
                module: vec![0, 97, 115, 109],
                input: vec![1],
                bounds,
            })
            .expect_err("the iOS ABI does not support module input");
        assert!(matches!(error, WasmComputeError::InvalidRequest(_)));
    }
}
