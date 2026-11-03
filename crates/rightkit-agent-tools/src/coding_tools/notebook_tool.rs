//! `notebook` — cell-aware .ipynb editing.
//!
//! Parse Jupyter notebooks (JSON), ops: read_cell(index), edit_cell(index, source),
//! insert_cell(index, kind, source), delete_cell(index).
//! Preserves nbformat structure.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_NOTEBOOK_CELL_SOURCE_BYTES: usize = 1024 * 1024;

pub struct NotebookTool;

#[async_trait::async_trait]
impl Tool for NotebookTool {
    fn name(&self) -> &str {
        "notebook"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::WriteFile)
    }

    fn capability_class_for(&self, args: &Value) -> Option<CapabilityClass> {
        if args.get("operation").and_then(Value::as_str) == Some("read_cell") {
            Some(CapabilityClass::ReadFile)
        } else {
            Some(CapabilityClass::WriteFile)
        }
    }

    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        if is_mutation_operation(args.get("operation").and_then(Value::as_str)) {
            crate::coding_tools::durable_write::path_arg_kind(args)
        } else if args.get("notebook_path").is_some() {
            args.get("notebook_path")
                .and_then(Value::as_str)
                .and_then(|p| {
                    crate::coding_tools::durable_write::repository_policy_kind(
                        std::path::Path::new(p),
                    )
                })
        } else {
            None
        }
    }

    fn description(&self) -> &str {
        "Cell-aware .ipynb editing. Legacy mode uses path/operation/index/kind/source; Claude-compatible mode uses notebook_path/cell_id/new_source/cell_type/edit_mode (replace, insert, delete). Preserves nbformat."
    }

    fn input_schema(&self) -> Value {
        json!({"oneOf":[
            {"type":"object","required":["path","operation"],"properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["read_cell","edit_cell","insert_cell","delete_cell"]},"index":{"type":"integer"},"kind":{"type":"string","enum":["code","markdown","raw"]},"source":{"type":"string"}}},
            {"type":"object","additionalProperties":false,"required":["notebook_path","new_source"],"properties":{"notebook_path":{"type":"string"},"cell_id":{"type":"string"},"new_source":{"type":"string"},"cell_type":{"type":"string","enum":["code","markdown"]},"edit_mode":{"type":"string","enum":["replace","insert","delete"],"default":"replace"}}}
        ]})
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if args.get("notebook_path").is_some() {
            return donor_execute(args, ctx);
        }
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };

        let operation = match args["operation"].as_str() {
            Some(op) => op,
            None => return ToolResult::error("Missing required argument: operation"),
        };

        let path = match crate::coding_tools::read_file::resolve_in_workspace(
            path_str,
            &ctx.workspace_root,
        ) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        if operation == "read_cell" {
            if let Err(error) = authorize_notebook_read(&path, ctx) {
                return ToolResult::error(error);
            }
        } else if is_mutation_operation(Some(operation)) {
            if let Err(error) = authorize_notebook_mutation(path_str, &path, ctx) {
                return ToolResult::error(error);
            }
        }

        match operation {
            "read_cell" => {
                let index = match args["index"].as_u64() {
                    Some(i) => i as usize,
                    None => return ToolResult::error("Missing index for read_cell"),
                };
                read_cell(&path, index)
            }
            "edit_cell" => {
                let index = match args["index"].as_u64() {
                    Some(i) => i as usize,
                    None => return ToolResult::error("Missing index for edit_cell"),
                };
                let source = match args["source"].as_str() {
                    Some(s) => s,
                    None => return ToolResult::error("Missing source for edit_cell"),
                };
                edit_cell(&path, index, source, ctx)
            }
            "insert_cell" => {
                let index = match args["index"].as_u64() {
                    Some(i) => i as usize,
                    None => return ToolResult::error("Missing index for insert_cell"),
                };
                let kind = match args["kind"].as_str() {
                    Some(k) => k,
                    None => return ToolResult::error("Missing kind for insert_cell"),
                };
                let source = match args["source"].as_str() {
                    Some(s) => s,
                    None => return ToolResult::error("Missing source for insert_cell"),
                };
                insert_cell(&path, index, kind, source, ctx)
            }
            "delete_cell" => {
                let index = match args["index"].as_u64() {
                    Some(i) => i as usize,
                    None => return ToolResult::error("Missing index for delete_cell"),
                };
                delete_cell(&path, index, ctx)
            }
            other => ToolResult::error(format!("Unknown operation: {}", other)),
        }
    }
}

fn donor_execute(args: Value, ctx: &ToolContext) -> ToolResult {
    let raw = match args.get("notebook_path").and_then(Value::as_str) {
        Some(v) => v,
        None => return ToolResult::error("Missing required argument: notebook_path"),
    };
    let path = match crate::coding_tools::read_file::resolve_in_workspace(raw, &ctx.workspace_root)
    {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    for access in [
        crate::coding_tools::read_file::PathAccess::Read,
        crate::coding_tools::read_file::PathAccess::Write,
    ] {
        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(ctx, &path, access) {
            return ToolResult::error(error);
        }
    }
    if path.extension().and_then(|v| v.to_str()) != Some("ipynb") {
        return ToolResult::error("File must be a Jupyter notebook (.ipynb file). For editing other file types, use the FileEdit tool.");
    }
    let mode = args
        .get("edit_mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");
    if !matches!(mode, "replace" | "insert" | "delete") {
        return ToolResult::error("Edit mode must be replace, insert, or delete.");
    }
    let id = args.get("cell_id").and_then(Value::as_str);
    if mode != "insert" && id.is_none() {
        return ToolResult::error("Cell ID must be specified when not inserting a new cell.");
    }
    let source = args.get("new_source").and_then(Value::as_str).unwrap_or("");
    if source.len() > MAX_NOTEBOOK_CELL_SOURCE_BYTES {
        return ToolResult::error("Notebook cell source exceeds maximum supported size");
    }
    let kind = args.get("cell_type").and_then(Value::as_str);
    if mode == "insert" && !matches!(kind, Some("code") | Some("markdown")) {
        return ToolResult::error("Cell type is required when using edit_mode=insert.");
    }
    if let Some(k) = kind {
        if !matches!(k, "code" | "markdown") {
            return ToolResult::error("Cell type must be code or markdown.");
        }
    }
    if let Err(e) = authorize_notebook_mutation(raw, &path, ctx) {
        return ToolResult::error(e);
    }
    if let Err(e) = ctx.file_service.record_read(&path, true) {
        return ToolResult::error(e);
    }
    let mut notebook = match load_notebook(&path) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let cells = match notebook.get_mut("cells").and_then(Value::as_array_mut) {
        Some(v) => v,
        None => return ToolResult::error("Notebook has no cells array"),
    };
    let found = id.and_then(|wanted| {
        cells
            .iter()
            .position(|c| c.get("id").and_then(Value::as_str) == Some(wanted))
            .or_else(|| {
                wanted
                    .strip_prefix("cell-")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|i| *i < cells.len())
            })
    });
    let index = match (mode, id, found) {
        ("insert", None, _) => 0,
        ("insert", Some(_), Some(i)) => i + 1,
        (_, _, Some(i)) => i,
        _ => return ToolResult::error("Cell ID not found in notebook"),
    };
    let effective = if mode == "replace" && index == cells.len() {
        "insert"
    } else {
        mode
    };
    if effective == "insert" {
        let generated = format!("cell-{}", cells.len());
        let cell = if kind == Some("markdown") {
            json!({"cell_type":"markdown","id":generated,"source":source,"metadata":{}})
        } else {
            json!({"cell_type":"code","id":generated,"source":source,"metadata":{},"execution_count":null,"outputs":[]})
        };
        cells.insert(index.min(cells.len()), cell);
    } else if effective == "delete" {
        cells.remove(index);
    } else {
        let cell = &mut cells[index];
        cell["source"] = Value::String(source.to_string());
        if cell.get("cell_type").and_then(Value::as_str) == Some("code") {
            cell["execution_count"] = Value::Null;
            cell["outputs"] = json!([]);
        }
        if let Some(k) = kind {
            cell["cell_type"] = Value::String(k.to_string());
        }
    }
    let encoded = match serde_json::to_vec_pretty(&notebook) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(format!("Failed to serialize notebook: {e}")),
    };
    if let Err(e) = ctx.file_service.write_with_receipt_intent(
        crate::coding_tools::file_service::MutationIntentKind::Update,
        &path,
        &encoded,
        false,
    ) {
        return ToolResult::error(e.to_string());
    }
    ToolResult::ok(format!(
        "{} cell {}",
        if effective == "insert" {
            "Inserted"
        } else if effective == "delete" {
            "Deleted"
        } else {
            "Updated"
        },
        id.unwrap_or("(new)")
    ))
}

fn is_mutation_operation(operation: Option<&str>) -> bool {
    matches!(operation, Some("edit_cell" | "insert_cell" | "delete_cell"))
}

fn authorize_notebook_mutation(
    _path_arg: &str,
    path: &PathBuf,
    ctx: &ToolContext,
) -> Result<(), String> {
    crate::coding_tools::durable_write::require_path_authorization(path, ctx)?;
    crate::coding_tools::read_file::enforce_path_deny(
        ctx,
        path,
        crate::coding_tools::read_file::PathAccess::Write,
    )?;
    crate::coding_tools::read_file::enforce_path_deny(
        ctx,
        path,
        crate::coding_tools::read_file::PathAccess::Read,
    )?;
    let policy_path = crate::coding_tools::read_file::policy_target_path(path, &ctx.workspace_root);
    let verdict = PermissionEngine::new().evaluate(
        CapabilityClass::WriteFile,
        ctx.permission_mode,
        ctx.workspace_trust,
        Some(&policy_path),
        false,
    );
    match verdict.decision {
        PermissionDecision::Allow => Ok(()),
        PermissionDecision::Ask => Err(format!(
            "Tool requires approval before proceeding: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        PermissionDecision::Deny => Err(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
    }
}

fn authorize_notebook_read(path: &PathBuf, ctx: &ToolContext) -> Result<(), String> {
    crate::coding_tools::read_file::enforce_path_deny(
        ctx,
        path,
        crate::coding_tools::read_file::PathAccess::Read,
    )?;
    let policy_path = crate::coding_tools::read_file::policy_target_path(path, &ctx.workspace_root);
    let verdict = PermissionEngine::new().evaluate(
        CapabilityClass::ReadFile,
        ctx.permission_mode,
        ctx.workspace_trust,
        Some(&policy_path),
        false,
    );
    if verdict.decision == PermissionDecision::Deny {
        Err(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        ))
    } else {
        Ok(())
    }
}

fn read_cell(path: &PathBuf, index: usize) -> ToolResult {
    let notebook = match load_notebook(path) {
        Ok(notebook) => notebook,
        Err(error) => return ToolResult::error(error),
    };

    let cells = match notebook.get("cells").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return ToolResult::error("Notebook has no cells array"),
    };

    if index >= cells.len() {
        return ToolResult::error(format!(
            "Cell index {} out of range (notebook has {} cells)",
            index,
            cells.len()
        ));
    }

    let cell = &cells[index];
    let cell_type = cell
        .get("cell_type")
        .and_then(|ct| ct.as_str())
        .unwrap_or("unknown");
    let source = cell
        .get("source")
        .and_then(|s| {
            if s.is_array() {
                Some(
                    s.as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(""),
                )
            } else {
                s.as_str().map(String::from)
            }
        })
        .unwrap_or_default();

    ToolResult::ok(format!(
        "Cell [{}] (type: {})\n{}",
        index, cell_type, source
    ))
}

fn edit_cell(path: &PathBuf, index: usize, source: &str, ctx: &ToolContext) -> ToolResult {
    if let Err(error) = validate_source_size(source) {
        return ToolResult::error(error);
    }
    if let Err(error) = ctx.file_service.record_read(path, true) {
        return ToolResult::error(error);
    }
    let mut notebook = match load_notebook(path) {
        Ok(notebook) => notebook,
        Err(error) => return ToolResult::error(error),
    };

    let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return ToolResult::error("Notebook has no cells array"),
    };

    if index >= cells.len() {
        return ToolResult::error(format!(
            "Cell index {} out of range (notebook has {} cells)",
            index,
            cells.len()
        ));
    }

    // Update source field
    cells[index]["source"] = Value::String(source.to_string());

    match write_notebook(path, &notebook, ctx) {
        Ok(_) => ToolResult::ok(format!("Edited cell [{}]", index)),
        Err(error) => ToolResult::error(error),
    }
}

fn insert_cell(
    path: &PathBuf,
    index: usize,
    kind: &str,
    source: &str,
    ctx: &ToolContext,
) -> ToolResult {
    if let Err(error) = validate_source_size(source) {
        return ToolResult::error(error);
    }
    if let Err(error) = ctx.file_service.record_read(path, true) {
        return ToolResult::error(error);
    }
    let mut notebook = match load_notebook(path) {
        Ok(notebook) => notebook,
        Err(error) => return ToolResult::error(error),
    };

    let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return ToolResult::error("Notebook has no cells array"),
    };
    if cells.len() >= crate::coding_tools::read_file::MAX_NOTEBOOK_CELLS {
        return ToolResult::error(format!(
            "Notebook already has the maximum supported count of {} cells",
            crate::coding_tools::read_file::MAX_NOTEBOOK_CELLS
        ));
    }

    // Create new cell
    let new_cell = match kind {
        "code" => json!({
            "cell_type": "code",
            "execution_count": Value::Null,
            "metadata": {},
            "outputs": [],
            "source": source
        }),
        "markdown" => json!({
            "cell_type": "markdown",
            "metadata": {},
            "source": source
        }),
        "raw" => json!({
            "cell_type": "raw",
            "metadata": {},
            "source": source
        }),
        other => return ToolResult::error(format!("Invalid cell kind: {}", other)),
    };

    cells.insert(index.min(cells.len()), new_cell);

    match write_notebook(path, &notebook, ctx) {
        Ok(_) => ToolResult::ok(format!("Inserted {} cell at index [{}]", kind, index)),
        Err(error) => ToolResult::error(error),
    }
}

fn delete_cell(path: &PathBuf, index: usize, ctx: &ToolContext) -> ToolResult {
    if let Err(error) = ctx.file_service.record_read(path, true) {
        return ToolResult::error(error);
    }
    let mut notebook = match load_notebook(path) {
        Ok(notebook) => notebook,
        Err(error) => return ToolResult::error(error),
    };

    let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return ToolResult::error("Notebook has no cells array"),
    };

    if index >= cells.len() {
        return ToolResult::error(format!(
            "Cell index {} out of range (notebook has {} cells)",
            index,
            cells.len()
        ));
    }

    cells.remove(index);

    match write_notebook(path, &notebook, ctx) {
        Ok(_) => ToolResult::ok(format!("Deleted cell [{}]", index)),
        Err(error) => ToolResult::error(error),
    }
}

fn load_notebook(path: &PathBuf) -> Result<Value, String> {
    let bytes = crate::coding_tools::read_file::read_bounded_bytes(
        path,
        crate::coding_tools::read_file::MAX_NOTEBOOK_BYTES,
        "notebook",
    )?;
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err("Invalid notebook JSON: expected a top-level object".to_string());
    }
    let notebook: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid notebook JSON: {error}"))?;
    let cells = notebook
        .get("cells")
        .and_then(Value::as_array)
        .ok_or_else(|| "Notebook has no cells array".to_string())?;
    if cells.len() > crate::coding_tools::read_file::MAX_NOTEBOOK_CELLS {
        return Err(format!(
            "Notebook has {} cells; maximum supported count is {}",
            cells.len(),
            crate::coding_tools::read_file::MAX_NOTEBOOK_CELLS
        ));
    }
    Ok(notebook)
}

fn validate_source_size(source: &str) -> Result<(), String> {
    if source.len() > MAX_NOTEBOOK_CELL_SOURCE_BYTES {
        Err(format!(
            "Notebook cell source is {} bytes; maximum supported size is {MAX_NOTEBOOK_CELL_SOURCE_BYTES} bytes",
            source.len()
        ))
    } else {
        Ok(())
    }
}

fn write_notebook(path: &PathBuf, notebook: &Value, ctx: &ToolContext) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(notebook)
        .map_err(|error| format!("Failed to serialize notebook: {error}"))?;
    if encoded.len() > crate::coding_tools::read_file::MAX_NOTEBOOK_BYTES {
        return Err(format!(
            "Edited notebook would be {} bytes; maximum supported size is {} bytes",
            encoded.len(),
            crate::coding_tools::read_file::MAX_NOTEBOOK_BYTES
        ));
    }
    ctx.file_service
        .write_with_receipt_intent(
            crate::coding_tools::file_service::MutationIntentKind::Update,
            path,
            &encoded,
            false,
        )
        .map_err(|error| format!("Failed to write notebook: {error}"))
        .map(|_| ())
}
