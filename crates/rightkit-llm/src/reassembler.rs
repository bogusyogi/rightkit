//! Provider-agnostic tool-call reassembly with ordered early drain
//! (Orthic-Labs/rightkit#664).
//!
//! Streamed tool calls arrive as fragments keyed by an index (the SSE `index`). Feed each
//! fragment to [`ToolCallChunkReassembler::process_delta`]. Then:
//!
//! - [`ToolCallChunkReassembler::drain_ready`] releases only a run of calls that starts at the
//!   next declared index (0, then 1, and so on) and whose id and name are known and whose
//!   arguments are a complete JSON object. A call is never released before an earlier index, even
//!   when a later index finished first, and a tool can start while later calls still stream. A
//!   stream whose first index is not 0 is released only by `drain_completed`.
//! - [`ToolCallChunkReassembler::drain_completed`] is called once the stream has ended. It
//!   releases everything not yet released, in index order, and fails if a remaining call is
//!   missing its id or name or has arguments that are not JSON.
//!
//! A released call is never changed. A fragment for a released index is an error
//! ([`ReassembleError::AfterRelease`]).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// A fully reassembled tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassembledToolCall {
    /// The declared index of the call in the stream.
    pub index: u64,
    pub id: String,
    pub name: String,
    /// The concatenated argument text, exactly as streamed.
    pub arguments: String,
}

impl ReassembledToolCall {
    /// The arguments parsed as JSON. Empty arguments are read as an empty object.
    pub fn input(&self) -> Result<Value, ReassembleError> {
        if self.arguments.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_str(&self.arguments).map_err(|e| ReassembleError::BadArguments {
            index: self.index,
            message: e.to_string(),
        })
    }
}

/// Why reassembly failed. Messages never include argument text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassembleError {
    /// A fragment for an index that was already released.
    AfterRelease(u64),
    /// Two different ids or names were streamed for one index.
    Conflict { index: u64, field: &'static str },
    /// At end of stream, a call lacks its id or name.
    Incomplete { index: u64, missing: &'static str },
    /// At end of stream, a call's arguments are not valid JSON.
    BadArguments { index: u64, message: String },
}

impl std::fmt::Display for ReassembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReassembleError::AfterRelease(i) => {
                write!(f, "tool call {i} received data after it was released")
            }
            ReassembleError::Conflict { index, field } => {
                write!(f, "tool call {index} streamed two different {field} values")
            }
            ReassembleError::Incomplete { index, missing } => {
                write!(f, "tool call {index} ended without its {missing}")
            }
            ReassembleError::BadArguments { index, message } => {
                write!(
                    f,
                    "tool call {index} has arguments that are not JSON: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ReassembleError {}

#[derive(Debug, Default)]
struct Pending {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl Pending {
    /// Ready for early drain: id and name known, and arguments already a complete JSON object.
    /// A bare scalar such as `1` is not enough, because more digits may still stream.
    fn ready(&self) -> bool {
        self.id.is_some() && self.name.is_some() && object_complete(&self.arguments)
    }

    fn finish(self, index: u64) -> ReassembledToolCall {
        ReassembledToolCall {
            index,
            id: self.id.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            arguments: self.arguments,
        }
    }
}

/// True when `text` is a complete JSON object (leading whitespace allowed).
fn object_complete(text: &str) -> bool {
    text.trim_start().starts_with('{')
        && matches!(serde_json::from_str::<Value>(text), Ok(Value::Object(_)))
}

/// Reassembles streamed tool calls. See the module docs.
#[derive(Debug, Default)]
pub struct ToolCallChunkReassembler {
    pending: BTreeMap<u64, Pending>,
    /// Indices already released. Data for these is rejected with `AfterRelease`.
    released: BTreeSet<u64>,
    /// The next declared index that early drain may release.
    next: u64,
}

impl ToolCallChunkReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one fragment for the call at `index`. `id` and `name` are set the first time they
    /// are seen; a later, different value is a [`ReassembleError::Conflict`]. `arguments` is
    /// appended as-is.
    pub fn process_delta(
        &mut self,
        index: u64,
        id: Option<&str>,
        name: Option<&str>,
        arguments: &str,
    ) -> Result<(), ReassembleError> {
        if self.released.contains(&index) {
            return Err(ReassembleError::AfterRelease(index));
        }
        let p = self.pending.entry(index).or_default();
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            match p.id.clone() {
                Some(existing) => {
                    if existing != id {
                        return Err(ReassembleError::Conflict {
                            index,
                            field: "ids",
                        });
                    }
                }
                None => p.id = Some(id.to_owned()),
            }
        }
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            match p.name.clone() {
                Some(existing) => {
                    if existing != name {
                        return Err(ReassembleError::Conflict {
                            index,
                            field: "names",
                        });
                    }
                }
                None => p.name = Some(name.to_owned()),
            }
        }
        p.arguments.push_str(arguments);
        Ok(())
    }

    /// Release the leading run of ready calls (see the module docs). Returns them in index
    /// order. Calls released here are not returned again.
    pub fn drain_ready(&mut self) -> Vec<ReassembledToolCall> {
        let mut out = Vec::new();
        while self.pending.get(&self.next).is_some_and(Pending::ready) {
            let index = self.next;
            if let Some(p) = self.pending.remove(&index) {
                self.released.insert(index);
                out.push(p.finish(index));
            }
            self.next = self.next.saturating_add(1);
        }
        out
    }

    /// Call once the stream has ended. Releases every call not yet released, in index order.
    /// Fails, and releases nothing, when a call lacks its id or name or has arguments that are
    /// not JSON.
    pub fn drain_completed(&mut self) -> Result<Vec<ReassembledToolCall>, ReassembleError> {
        for (index, p) in &self.pending {
            if p.id.is_none() {
                return Err(ReassembleError::Incomplete {
                    index: *index,
                    missing: "id",
                });
            }
            if p.name.is_none() {
                return Err(ReassembleError::Incomplete {
                    index: *index,
                    missing: "name",
                });
            }
            let args = if p.arguments.trim().is_empty() {
                "{}"
            } else {
                p.arguments.as_str()
            };
            if let Err(e) = serde_json::from_str::<Value>(args) {
                return Err(ReassembleError::BadArguments {
                    index: *index,
                    message: e.to_string(),
                });
            }
        }
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::with_capacity(pending.len());
        for (index, p) in pending {
            self.released.insert(index);
            out.push(p.finish(index));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_drain_follows_declared_order_not_observed_order() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(1, Some("b"), Some("t"), "{}").unwrap();
        assert!(r.drain_ready().is_empty());
        r.process_delta(0, Some("a"), Some("t"), "{}").unwrap();
        let order: Vec<u64> = r.drain_ready().iter().map(|c| c.index).collect();
        assert_eq!(order, [0, 1]);
    }

    #[test]
    fn scalar_prefix_is_not_early_complete() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(0, Some("a"), Some("t"), "1").unwrap();
        assert!(r.drain_ready().is_empty());
        // The digit continues, so the call was never released and the fragment is accepted.
        assert!(r.process_delta(0, None, None, "0").is_ok());
        assert!(r.drain_ready().is_empty());
    }

    #[test]
    fn releases_leading_ready_run_only() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(0, Some("a"), Some("read"), "{\"p\":")
            .unwrap();
        assert!(r.drain_ready().is_empty());
        r.process_delta(0, None, None, "1}").unwrap();
        // Index 1 is ready, but index 0 came first and is ready now: both release in order.
        r.process_delta(1, Some("b"), Some("write"), "{}").unwrap();
        r.process_delta(2, Some("c"), Some("grep"), "{\"q\":\"x")
            .unwrap();
        let first = r.drain_ready();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].index, 0);
        assert_eq!(first[0].arguments, "{\"p\":1}");
        assert_eq!(first[1].name, "write");
        // Index 2 is not ready, so nothing more is released.
        assert!(r.drain_ready().is_empty());
        r.process_delta(2, None, None, "\"}").unwrap();
        assert_eq!(r.drain_ready().len(), 1);
    }

    #[test]
    fn a_later_ready_call_waits_behind_an_earlier_incomplete_one() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(0, Some("a"), Some("slow"), "{\"x\"")
            .unwrap();
        r.process_delta(1, Some("b"), Some("fast"), "{}").unwrap();
        assert!(r.drain_ready().is_empty());
    }

    #[test]
    fn released_index_rejects_more_data_and_conflicts_are_errors() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(0, Some("a"), Some("t"), "{}").unwrap();
        assert_eq!(r.drain_ready().len(), 1);
        assert_eq!(
            r.process_delta(0, None, None, "{}"),
            Err(ReassembleError::AfterRelease(0))
        );
        r.process_delta(1, Some("b"), Some("t"), "{").unwrap();
        assert_eq!(
            r.process_delta(1, Some("other"), None, ""),
            Err(ReassembleError::Conflict {
                index: 1,
                field: "ids"
            })
        );
    }

    #[test]
    fn drain_completed_releases_all_or_fails_without_releasing() {
        let mut r = ToolCallChunkReassembler::new();
        r.process_delta(0, Some("a"), Some("t"), "{\"k\":true}")
            .unwrap();
        r.process_delta(1, Some("b"), Some("t"), "").unwrap();
        let done = r.drain_completed().unwrap();
        assert_eq!(done.len(), 2);
        assert_eq!(done[1].input().unwrap(), serde_json::json!({}));

        let mut bad = ToolCallChunkReassembler::new();
        bad.process_delta(0, Some("a"), Some("t"), "{\"k\":")
            .unwrap();
        assert!(matches!(
            bad.drain_completed(),
            Err(ReassembleError::BadArguments { index: 0, .. })
        ));
        // Nothing was released, so the call is still pending.
        assert!(bad.process_delta(0, None, None, "1}").is_ok());
    }
}
