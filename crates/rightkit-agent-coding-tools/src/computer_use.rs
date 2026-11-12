//! `computer`: operate the desktop (parity with Codex and Orca computer use).
//!
//! Linux X11 through `xdotool` (pointer and keyboard) and ImageMagick
//! `import` (screenshots). The tool is advertised only where it can work: a
//! `DISPLAY` is set and both programs are on PATH (tools without a real
//! backend are never advertised).
//!
//! Safety: every action except `screenshot` is treated as destructive, so it
//! asks for approval in every mode except bypass, and a screenshot is a read
//! of whatever is on screen. The helper programs get an environment cleared
//! down to the display variables; no argument ever reaches a shell (xdotool
//! is called with argv, text after `--`, key names validated).

use crate::{Tool, ToolContext, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::time::Duration;

const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;
const MAX_TYPE_CHARS: usize = 4000;

pub struct ComputerUseTool;

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Whether this host can run the tool: Linux with a display and the helpers.
pub fn computer_use_available() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
        && on_path("xdotool")
        && on_path("import")
}

/// Run a helper with argv only and an environment of just the display.
async fn helper(program: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let mut command = rightkit_child_process::tokio_support::command(program);
    command.env_clear();
    for name in ["DISPLAY", "XAUTHORITY", "PATH", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(ACTION_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("{program} timed out"))?
        .map_err(|error| format!("{program} could not start: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn coordinate(args: &Value, key: &str) -> Result<i64, String> {
    args[key]
        .as_i64()
        .filter(|value| (0..=100_000).contains(value))
        .ok_or_else(|| format!("`{key}` must be a non-negative integer pixel coordinate"))
}

/// xdotool key names: letters, digits, `+` between keys, `_` (e.g. `ctrl+s`,
/// `Return`, `super+Page_Down`). Anything else is refused.
fn valid_keys(keys: &str) -> bool {
    !keys.is_empty()
        && keys.len() <= 100
        && keys.split('+').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

async fn screenshot_block() -> Result<String, String> {
    let png = helper("import", &["-window".into(), "root".into(), "png:-".into()]).await?;
    if png.len() > MAX_SCREENSHOT_BYTES {
        return Err(format!("screenshot too large ({} bytes)", png.len()));
    }
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("the screenshot is not a PNG".into());
    }
    let geometry = helper("xdotool", &["getdisplaygeometry".into()])
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().replace(' ', "x"))
        .unwrap_or_else(|_| "unknown".into());
    Ok(format!(
        "[image-block]\npath: screen\nsize: {} bytes\nmime: image/png\ndimensions: {geometry}\ndata_url: data:image/png;base64,{}",
        png.len(),
        rightkit_crypto::base64_encode(&png)
    ))
}

#[async_trait::async_trait]
impl Tool for ComputerUseTool {
    fn name(&self) -> &str {
        "computer"
    }

    fn description(&self) -> &str {
        "Operate the desktop: take a screenshot, move the pointer, click, type text, press keys or \
         scroll. Coordinates are screen pixels from the top-left, as seen in the latest screenshot. \
         Every action returns a fresh screenshot. Actions other than screenshot need approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["screenshot", "move", "click", "double_click", "right_click", "type", "key", "scroll"]
                },
                "x": {"type": "integer", "description": "Pixel column (move, clicks, scroll)"},
                "y": {"type": "integer", "description": "Pixel row (move, clicks, scroll)"},
                "text": {"type": "string", "description": "Text to type (type)"},
                "keys": {"type": "string", "description": "Key combination such as ctrl+s or Return (key)"},
                "direction": {"type": "string", "enum": ["up", "down"], "description": "Scroll direction (scroll)"},
                "amount": {"type": "integer", "description": "Scroll steps, 1 to 20 (scroll)"}
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Unknown)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if !computer_use_available() {
            return ToolResult::error(
                "computer use is not available here: it needs Linux with a DISPLAY, xdotool and ImageMagick import",
            );
        }
        let action = args["action"].as_str().unwrap_or_default().to_string();
        let effectful = action != "screenshot";
        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::Unknown,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            effectful,
        );
        match verdict.decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Deny => {
                return ToolResult::error(format!(
                    "Permission denied: {} (policy: {})",
                    verdict.reason, verdict.policy_source
                ))
            }
            PermissionDecision::Ask => {
                return ToolResult::error(format!(
                    "Tool requires approval: {} (policy: {})",
                    verdict.reason, verdict.policy_source
                ))
            }
        }
        let xdotool: Result<Vec<String>, String> = match action.as_str() {
            "screenshot" => Ok(Vec::new()),
            "move" => (|| {
                Ok(vec![
                    "mousemove".into(),
                    coordinate(&args, "x")?.to_string(),
                    coordinate(&args, "y")?.to_string(),
                ])
            })(),
            "click" | "double_click" | "right_click" => (|| {
                let mut argv = vec![
                    "mousemove".to_string(),
                    coordinate(&args, "x")?.to_string(),
                    coordinate(&args, "y")?.to_string(),
                    "click".to_string(),
                ];
                if action == "double_click" {
                    argv.extend(["--repeat".into(), "2".into()]);
                }
                argv.push(if action == "right_click" { "3" } else { "1" }.into());
                Ok(argv)
            })(),
            "type" => match args["text"].as_str() {
                Some(text) if !text.is_empty() && text.chars().count() <= MAX_TYPE_CHARS => {
                    Ok(vec![
                        "type".into(),
                        "--delay".into(),
                        "12".into(),
                        "--".into(),
                        text.into(),
                    ])
                }
                _ => Err(format!("`text` must be 1 to {MAX_TYPE_CHARS} characters")),
            },
            "key" => match args["keys"].as_str() {
                Some(keys) if valid_keys(keys) => Ok(vec!["key".into(), "--".into(), keys.into()]),
                _ => Err("`keys` must be key names joined by +, such as ctrl+s or Return".into()),
            },
            "scroll" => (|| {
                let button = match args["direction"].as_str() {
                    Some("up") => "4",
                    Some("down") => "5",
                    _ => return Err("`direction` must be up or down".to_string()),
                };
                let amount = args["amount"].as_i64().unwrap_or(3).clamp(1, 20);
                Ok(vec![
                    "mousemove".into(),
                    coordinate(&args, "x")?.to_string(),
                    coordinate(&args, "y")?.to_string(),
                    "click".into(),
                    "--repeat".into(),
                    amount.to_string(),
                    button.into(),
                ])
            })(),
            other => Err(format!("unknown action `{other}`")),
        };
        let argv = match xdotool {
            Ok(argv) => argv,
            Err(message) => return ToolResult::error(message),
        };
        // The action and the pointer read run in one xdotool call (one X
        // connection), so the reported position is the one the action left.
        let mut argv = argv;
        argv.push("getmouselocation".into());
        let location = match helper("xdotool", &argv).await {
            Ok(stdout) => String::from_utf8_lossy(&stdout)
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_string(),
            Err(message) => return ToolResult::error(message),
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        match screenshot_block().await {
            Ok(block) => ToolResult::ok(format!("action: {action}\npointer: {location}\n{block}")),
            Err(message) => {
                ToolResult::error(format!("{action} done; screenshot failed: {message}"))
            }
        }
    }
}
