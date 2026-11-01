//! Host-owned H2 assurance broker.
//!
//! This module owns task/turn identity and trusted tool receipts.  It does
//! not evaluate proof itself: the Sentinel/Beacon adapter consumes the
//! projection through `AssurancePlane`.  Keeping receipt issuance here makes
//! it impossible for model-authored tool output to become trusted evidence.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "orthic.tool-receipt.v1";
pub const HOST_ISSUER: &str = "rightkit-agent-host";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckSpec {
    pub criterion_id: String,
    pub critical: bool,
    pub evidence_class: String,
    pub operation: String,
    pub source_generation_required: bool,
    pub command_pattern_or_test_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskContract {
    pub schema: String,
    pub task_id: String,
    pub trace_id: String,
    pub session_id: String,
    pub user_request_digest: String,
    pub repository_scope: Vec<String>,
    pub criteria: Vec<CheckSpec>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolReceipt {
    pub schema: String,
    pub task_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub issuer_id: String,
    pub issuer_capability_digest: String,
    pub tool_class: String,
    pub operation: String,
    pub scope: Vec<String>,
    pub input_digest: String,
    pub output_digest: String,
    pub exit_status: i32,
    pub started_at: String,
    pub completed_at: String,
    pub source_generation_id: Option<String>,
    pub signature_or_mac: String,
    pub criterion_ids: Vec<String>,
    pub invalidated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateDecision {
    pub allowed: bool,
    pub reason: String,
    pub repair_packet: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceLocator {
    pub path: String,
    pub start_line: Option<u32>,
    pub start_column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
    pub symbol: Option<String>,
    pub source_digest: String,
}

/// Parse `path:line:column[-line:column]` without treating `C:` as a range
/// separator.  A `#` separator is also accepted for paths containing colons.
pub fn source_locator(raw: &str, source: &[u8]) -> SourceLocator {
    let raw = raw.trim();
    let (path, range): (String, Option<String>) = raw
        .rsplit_once('#')
        .map(|(path, range)| (path.to_string(), Some(range.to_string())))
        .or_else(|| split_line_suffix(raw))
        .unwrap_or((raw.to_string(), None));
    let (start_line, start_column, end_line, end_column) = range
        .as_deref()
        .and_then(parse_range)
        .unwrap_or((None, None, None, None));
    SourceLocator {
        path: normalize_path(&path),
        start_line,
        start_column,
        end_line,
        end_column,
        symbol: None,
        source_digest: digest(source),
    }
}

fn split_line_suffix(raw: &str) -> Option<(String, Option<String>)> {
    if let Some(dash) = raw.rfind('-') {
        let (left, right) = raw.split_at(dash);
        let right = right.strip_prefix('-')?;
        let (end_line, end_column) = right.split_once(':')?;
        if end_line.parse::<u32>().is_ok() && end_column.parse::<u32>().is_ok() {
            let (prefix, start_column) = left.rsplit_once(':')?;
            let (path, start_line) = prefix.rsplit_once(':')?;
            if start_line.parse::<u32>().is_ok() && start_column.parse::<u32>().is_ok() {
                return Some((
                    path.to_string(),
                    Some(format!(
                        "{start_line}:{start_column}-{end_line}:{end_column}"
                    )),
                ));
            }
        }
    }
    let mut pieces = raw.rsplitn(3, ':');
    let last = pieces.next()?;
    let middle = pieces.next()?;
    let path = pieces.next()?;
    if last.parse::<u32>().is_err() || middle.parse::<u32>().is_err() {
        return None;
    }
    // A Windows drive prefix has no bearing on this suffix split: rsplitn
    // finds the final two numeric fields rather than the first colon.
    Some((path.to_string(), Some(format!("{middle}:{last}"))))
}

/// Start line/column and end line/column, each absent when the range omits it.
type ParsedRange = (Option<u32>, Option<u32>, Option<u32>, Option<u32>);

/// Everything one tool receipt records. Grouped rather than passed positionally
/// because eleven same-typed `impl Into<String>` parameters in a row are
/// trivially transposable at a call site and the compiler cannot catch it.
pub struct ReceiptRequest<'a> {
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_class: String,
    pub operation: String,
    pub scope: Vec<String>,
    pub input: &'a [u8],
    pub output: &'a [u8],
    pub exit_status: i32,
    pub source_generation_id: Option<String>,
    pub criterion_ids: Vec<String>,
}

fn parse_range(range: &str) -> Option<ParsedRange> {
    let (start, end) = range
        .split_once('-')
        .map_or((range, None), |(a, b)| (a, Some(b)));
    let (start_line, start_column) = start.split_once(':')?;
    let (end_line, end_column) = match end {
        Some(value) => value
            .split_once(':')
            .map_or((None, None), |(line, column)| (Some(line), Some(column))),
        None => (None, None),
    };
    Some((
        start_line.parse().ok(),
        start_column.parse().ok(),
        end_line.and_then(|value| value.parse().ok()),
        end_column.and_then(|value| value.parse().ok()),
    ))
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProjectionRecord {
    Task {
        contract: TaskContract,
    },
    Receipt {
        receipt: ToolReceipt,
    },
    Close {
        task_id: String,
        ledger_digest: String,
    },
}

/// Persistent, incremental host projection.  The append-only ledger is a
/// local transport for the Beacon adapter; it is not a second proof store.
pub struct AssuranceBroker {
    path: PathBuf,
    contract: Option<TaskContract>,
    receipts: HashMap<String, ToolReceipt>,
    closed_ledger_digest: Option<String>,
}

impl AssuranceBroker {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let mut broker = Self {
            path,
            contract: None,
            receipts: HashMap::new(),
            closed_ledger_digest: None,
        };
        broker.replay()?;
        Ok(broker)
    }

    pub fn contract(&self) -> Option<&TaskContract> {
        self.contract.as_ref()
    }

    pub fn receipts(&self) -> impl Iterator<Item = &ToolReceipt> {
        self.receipts.values()
    }

    pub fn receipt(&self, tool_call_id: &str) -> Option<&ToolReceipt> {
        self.receipts.get(tool_call_id)
    }

    pub fn begin_task(
        &mut self,
        session_id: impl Into<String>,
        task_id: impl Into<String>,
        trace_id: impl Into<String>,
        user_request: &str,
        repository_scope: Vec<String>,
        criteria: Vec<CheckSpec>,
    ) -> std::io::Result<TaskContract> {
        let contract = TaskContract {
            schema: "orthic.task-envelope.v1".into(),
            task_id: task_id.into(),
            trace_id: trace_id.into(),
            session_id: session_id.into(),
            user_request_digest: digest(user_request.as_bytes()),
            repository_scope,
            criteria,
            created_at: now(),
        };
        self.append(&ProjectionRecord::Task {
            contract: contract.clone(),
        })?;
        self.contract = Some(contract.clone());
        self.receipts.clear();
        self.closed_ledger_digest = None;
        Ok(contract)
    }

    /// Safety gate owned by the host.  Beacon may strengthen this decision;
    /// this method only rejects calls that are outside the declared scope.
    pub fn safety_gate(&self, scope: &[String]) -> GateDecision {
        let Some(contract) = &self.contract else {
            return GateDecision::blocked("task contract has not been created");
        };
        let allowed = scope.iter().all(|item| {
            contract.repository_scope.is_empty()
                || contract.repository_scope.iter().any(|root| {
                    rightkit_agent_tools::coding_tools::read_file::path_is_within(
                        std::path::Path::new(item),
                        std::path::Path::new(root),
                    )
                })
        });
        if allowed {
            GateDecision::allow()
        } else {
            GateDecision::blocked("tool scope exceeds task repository grant")
        }
    }

    pub fn issue_receipt(&mut self, request: ReceiptRequest<'_>) -> std::io::Result<ToolReceipt> {
        let ReceiptRequest {
            turn_id,
            tool_call_id,
            tool_class,
            operation,
            scope,
            input,
            output,
            exit_status,
            source_generation_id,
            criterion_ids,
        } = request;
        let Some(contract) = &self.contract else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "task contract has not been created",
            ));
        };
        let safety = self.safety_gate(&scope);
        if !safety.allowed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                safety.reason,
            ));
        }
        let started_at = now();
        let completed_at = now();
        let input_digest = digest(input);
        let output_digest = digest(output);
        let issuer_capability_digest = digest(b"rightkit-agent-host:tool-broker:v1");
        let signature_or_mac = digest(
            format!(
                "{}/{}/{}/{}/{}/{}/{}",
                contract.task_id,
                turn_id,
                tool_call_id,
                input_digest,
                output_digest,
                exit_status,
                issuer_capability_digest
            )
            .as_bytes(),
        );
        let criterion_ids = if criterion_ids.is_empty() {
            contract
                .criteria
                .iter()
                .filter(|criterion| criterion.operation == operation.as_str())
                .map(|criterion| criterion.criterion_id.clone())
                .collect()
        } else {
            criterion_ids
        };
        let receipt = ToolReceipt {
            schema: SCHEMA.into(),
            task_id: contract.task_id.clone(),
            turn_id,
            tool_call_id,
            issuer_id: HOST_ISSUER.into(),
            issuer_capability_digest,
            tool_class,
            operation,
            scope,
            input_digest,
            output_digest,
            exit_status,
            started_at,
            completed_at,
            source_generation_id,
            signature_or_mac,
            criterion_ids,
            invalidated: false,
        };
        self.append(&ProjectionRecord::Receipt {
            receipt: receipt.clone(),
        })?;
        self.receipts
            .insert(receipt.tool_call_id.clone(), receipt.clone());
        Ok(receipt)
    }

    pub fn completion_gate(&self) -> GateDecision {
        let Some(contract) = &self.contract else {
            return GateDecision::blocked("task contract has not been created");
        };
        let mut missing = Vec::new();
        for spec in contract.criteria.iter().filter(|spec| spec.critical) {
            let found = self.receipts.values().any(|receipt| {
                !receipt.invalidated
                    && receipt.exit_status == 0
                    && receipt.operation == spec.operation
                    && receipt
                        .criterion_ids
                        .iter()
                        .any(|id| id == &spec.criterion_id)
                    && (!spec.source_generation_required || receipt.source_generation_id.is_some())
            });
            if !found {
                missing.push(format!("missing valid evidence for {}", spec.criterion_id));
            }
        }
        if missing.is_empty() {
            GateDecision::allow()
        } else {
            GateDecision {
                allowed: false,
                reason: "critical criteria are not proven".into(),
                repair_packet: missing,
            }
        }
    }

    /// Response gate: an outgoing completion claim cannot exceed the host's
    /// current criterion proof. The response text is intentionally not trusted
    /// as evidence; only the already-issued receipts can authorize completion.
    pub fn response_gate(&self) -> GateDecision {
        self.completion_gate()
    }

    pub fn close_transactionally(&mut self) -> std::io::Result<GateDecision> {
        let gate = self.completion_gate();
        if !gate.allowed {
            return Ok(gate);
        }
        let ledger_digest = self.ledger_digest();
        self.append(&ProjectionRecord::Close {
            task_id: self
                .contract
                .as_ref()
                .map(|c| c.task_id.clone())
                .unwrap_or_default(),
            ledger_digest: ledger_digest.clone(),
        })?;
        self.closed_ledger_digest = Some(ledger_digest);
        Ok(gate)
    }

    fn ledger_digest(&self) -> String {
        let mut ids: Vec<_> = self
            .receipts
            .values()
            .map(|r| r.tool_call_id.as_str())
            .collect();
        ids.sort_unstable();
        digest(ids.join("\n").as_bytes())
    }

    fn append(&self, record: &ProjectionRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        line.push(b'\n');
        rightkit_fs::append_record(&self.path, &line, rightkit_fs::AppendOptions::default())
            .map(|_| ())
            .map_err(std::io::Error::other)
    }

    fn replay(&mut self) -> std::io::Result<()> {
        let Ok(body) = fs::read_to_string(&self.path) else {
            return Ok(());
        };
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            let record: ProjectionRecord =
                serde_json::from_str(line).map_err(std::io::Error::other)?;
            match record {
                ProjectionRecord::Task { contract } => {
                    self.contract = Some(contract);
                    self.receipts.clear();
                    self.closed_ledger_digest = None;
                }
                ProjectionRecord::Receipt { receipt } => {
                    self.receipts.insert(receipt.tool_call_id.clone(), receipt);
                }
                ProjectionRecord::Close { ledger_digest, .. } => {
                    self.closed_ledger_digest = Some(ledger_digest);
                }
            }
        }
        Ok(())
    }
}

impl GateDecision {
    fn allow() -> Self {
        Self {
            allowed: true,
            reason: "allowed".into(),
            repair_packet: Vec::new(),
        }
    }

    fn blocked(reason: &str) -> Self {
        Self {
            allowed: false,
            reason: reason.into(),
            repair_packet: vec![reason.into()],
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    format!("sha256:{}", hasher.finalize())
}

fn now() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("unix-ms:{millis}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn spec() -> CheckSpec {
        CheckSpec {
            criterion_id: "tests.focused".into(),
            critical: true,
            evidence_class: "test_run".into(),
            operation: "test".into(),
            source_generation_required: true,
            command_pattern_or_test_id: "cargo test".into(),
        }
    }

    #[test]
    fn receipt_is_host_issued_scope_checked_and_survives_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broker.jsonl");
        let mut broker = AssuranceBroker::open(&path).unwrap();
        broker
            .begin_task(
                "s1",
                "t1",
                "trace-1",
                "run tests",
                vec!["repo".into()],
                vec![spec()],
            )
            .unwrap();
        assert!(!broker.response_gate().allowed);
        let receipt = broker
            .issue_receipt(ReceiptRequest {
                turn_id: "turn-1".into(),
                tool_call_id: "call-1".into(),
                tool_class: "shell".into(),
                operation: "test".into(),
                scope: vec!["repo/tests".into()],
                input: b"cargo test",
                output: b"ok",
                exit_status: 0,
                source_generation_id: Some("gen-1".into()),
                criterion_ids: Vec::new(),
            })
            .unwrap();
        assert_eq!(receipt.schema, SCHEMA);
        assert_eq!(receipt.issuer_id, HOST_ISSUER);
        assert_eq!(receipt.criterion_ids, vec!["tests.focused"]);
        assert!(broker.completion_gate().allowed);
        assert!(broker.response_gate().allowed);
        assert!(broker.close_transactionally().unwrap().allowed);

        let reopened = AssuranceBroker::open(&path).unwrap();
        assert_eq!(reopened.receipts().count(), 1);
        assert!(reopened.completion_gate().allowed);
    }

    #[test]
    fn model_cannot_forge_success_or_escape_scope() {
        let dir = tempdir().unwrap();
        let mut broker = AssuranceBroker::open(dir.path().join("broker.jsonl")).unwrap();
        broker
            .begin_task(
                "s1",
                "t1",
                "trace-1",
                "edit",
                vec!["repo".into()],
                vec![spec()],
            )
            .unwrap();
        assert!(!broker.safety_gate(&["other/file".into()]).allowed);
        assert!(!broker.completion_gate().allowed);
        assert!(!broker.response_gate().allowed);
        assert!(broker
            .close_transactionally()
            .unwrap()
            .repair_packet
            .iter()
            .any(|p| p.contains("tests.focused")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_scope_equivalence_preserves_assurance_grant() {
        let dir = tempdir().unwrap();
        let mut broker = AssuranceBroker::open(dir.path().join("broker.jsonl")).unwrap();
        broker
            .begin_task(
                "s",
                "t",
                "trace",
                "edit",
                vec![r"\\?\D:\Claude".into()],
                vec![],
            )
            .unwrap();
        assert!(
            broker
                .safety_gate(&[r"D:\Claude\rightkit-agent\new.txt".into()])
                .allowed
        );
        assert!(
            !broker
                .safety_gate(&[r"D:\Claude-other\new.txt".into()])
                .allowed
        );
        assert!(
            !broker
                .safety_gate(&[r"D:\Claude\..\outside.txt".into()])
                .allowed
        );
        assert!(!broker.safety_gate(&[r"E:\Claude\new.txt".into()]).allowed);
    }

    #[test]
    fn locator_keeps_windows_drive_colon_and_hashes_exact_source() {
        let locator = source_locator(r"C:\work\repo\src\lib.rs:12:4-14:9", b"fn main() {}");
        assert_eq!(locator.path, "C:/work/repo/src/lib.rs");
        assert_eq!(locator.start_line, Some(12));
        assert_eq!(locator.start_column, Some(4));
        assert_eq!(locator.end_line, Some(14));
        assert_eq!(locator.end_column, Some(9));
        assert_eq!(locator.source_digest, digest(b"fn main() {}"));
    }
}
