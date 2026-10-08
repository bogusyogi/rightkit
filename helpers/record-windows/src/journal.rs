//! events.ndjson writer. Every event takes the next `seq`, and each kind is
//! checked against record-event.v1 the first time it is written. Generic over
//! the sink so the tests can write into memory and validate the lines.

use std::io::Write;

use rightkit_contracts::types::record_event::DropReason;
use serde_json::Value;

use crate::error::{Code, RecordError};
use crate::record;

pub struct Journal<W: Write> {
    out: W,
    session_id: String,
    seq: u64,
    validated: Vec<String>,
}

impl<W: Write> Journal<W> {
    pub fn new(out: W, session_id: impl Into<String>) -> Self {
        Self { out, session_id: session_id.into(), seq: 0, validated: Vec::new() }
    }

    /// Number of events written so far (the next seq).
    pub fn count(&self) -> u64 {
        self.seq
    }

    pub fn session_start(&mut self, host_ns: u64) -> Result<(), RecordError> {
        let value = record::session_start(&self.session_id, self.seq, host_ns);
        self.write(&value)
    }

    pub fn session_end(&mut self, host_ns: u64) -> Result<(), RecordError> {
        let value = record::session_end(&self.session_id, self.seq, host_ns);
        self.write(&value)
    }

    pub fn frame_ready(&mut self, index: u64, host_ns: u64, sample_pts_ns: u64) -> Result<(), RecordError> {
        let value = record::frame_ready(&self.session_id, self.seq, index, host_ns, sample_pts_ns);
        self.write(&value)
    }

    pub fn frame_dropped(
        &mut self,
        host_ns: u64,
        count: u32,
        interval_ms: f64,
        reason: DropReason,
    ) -> Result<(), RecordError> {
        let value = record::frame_dropped(&self.session_id, self.seq, host_ns, count, interval_ms, reason);
        self.write(&value)
    }

    pub fn flush(&mut self) -> Result<(), RecordError> {
        self.out.flush().map_err(|e| RecordError::new(Code::Io, format!("flush events.ndjson failed: {e}")))
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.out
    }

    /// Validates the first event of each kind, then writes one line. The seq
    /// advances only after a successful write, so a rejected event leaves no gap.
    fn write(&mut self, value: &Value) -> Result<(), RecordError> {
        let kind = value.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !self.validated.iter().any(|done| done == kind) {
            record::validate_event(value)
                .map_err(|e| RecordError::new(Code::Io, format!("record-event invalid ({kind}): {e}")))?;
            self.validated.push(kind.to_string());
        }
        self.out
            .write_all(format!("{value}\n").as_bytes())
            .map_err(|e| RecordError::new(Code::Io, format!("write events.ndjson failed: {e}")))?;
        self.seq += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_contracts::types::record_event::RecordEvent;
    use rightkit_contracts::{parse, Contract};

    fn lines(journal: Journal<Vec<u8>>) -> Vec<String> {
        String::from_utf8(journal.into_inner()).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn one_session_writes_validated_events_with_contiguous_seq() {
        let mut journal = Journal::new(Vec::new(), "bundle.win-1");
        journal.session_start(1_000).unwrap();
        journal.frame_ready(0, 1_100, 0).unwrap();
        journal.frame_ready(1, 1_200, 100).unwrap();
        journal.frame_dropped(1_250, 2, 33.3, DropReason::Backpressure).unwrap();
        journal.frame_dropped(1_300, 5, 83.3, DropReason::CaptureGap).unwrap();
        journal.session_end(2_000).unwrap();
        assert_eq!(journal.count(), 6);
        journal.flush().unwrap();

        let written = lines(journal);
        assert_eq!(written.len(), 6);
        let mut kinds = Vec::new();
        for (expected_seq, line) in written.iter().enumerate() {
            // Each line is checked by the contracts validator and parses as a typed record-event.
            let typed: RecordEvent = parse(Contract::RecordEvent, line)
                .unwrap_or_else(|e| panic!("line {expected_seq} does not validate: {e}"));
            assert_eq!(typed.seq, expected_seq as u64, "seq must be contiguous");
            assert_eq!(typed.contract_version, "1.1.0");
            kinds.push(typed.kind);
        }
        use rightkit_contracts::types::record_event::EventKind::*;
        assert_eq!(kinds, vec![SessionStart, FrameReady, FrameReady, FrameDropped, FrameDropped, SessionEnd]);
    }

    #[test]
    fn a_rejected_event_is_not_written_and_leaves_no_seq_gap() {
        let mut journal = Journal::new(Vec::new(), "bundle.win-1");
        journal.session_start(10).unwrap();
        // count must be at least 1; the validator refuses the event before any write.
        assert!(journal.frame_dropped(20, 0, 0.0, DropReason::Unknown).is_err());
        assert_eq!(journal.count(), 1);
        journal.session_end(30).unwrap();
        let written = lines(journal);
        assert_eq!(written.len(), 2);
        let end: RecordEvent = parse(Contract::RecordEvent, &written[1]).unwrap();
        assert_eq!(end.seq, 1);
    }

    #[test]
    fn frame_dropped_line_carries_the_reason_and_interval() {
        let mut journal = Journal::new(Vec::new(), "bundle.win-1");
        journal.frame_dropped(500, 3, 50.0, DropReason::Encoder).unwrap();
        let line = &lines(journal)[0];
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["frameDropped"]["reason"], "encoder");
        assert_eq!(value["frameDropped"]["count"], 3);
        assert_eq!(value["frameDropped"]["intervalMs"], 50.0);
        assert_eq!(value["hostNs"], 500);
    }
}
