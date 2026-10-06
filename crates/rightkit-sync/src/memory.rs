//! In-process zone with real change-feed semantics. Reference behaviour for
//! real backends and the fixture for the convergence tests. Clones share one
//! zone, so two engines on two clones play two devices.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::{SyncError, SyncResult};
use crate::record::RawRecord;
use crate::traits::{Backend, ChangePage, RecordOutcome, SavePolicy};

#[derive(Default)]
struct Zone {
    records: BTreeMap<String, (u64, RawRecord)>,
    seq: u64,
    deletions: Vec<(u64, String)>,
    /// Page size for delta fetches (0 = unlimited) to exercise `more_coming`.
    page_size: usize,
    offline: bool,
    expire_token_once: bool,
    /// Succeed for this many record saves, then fail the rest of the call.
    fail_after_saves: Option<usize>,
    save_calls: usize,
    saved_records: usize,
    /// Another device's write that lands just before the next save call is
    /// evaluated (the window between the engine's fetch and its save).
    concurrent_write: Option<RawRecord>,
}

#[derive(Clone, Default)]
pub struct MemoryBackend {
    zone: Arc<Mutex<Zone>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_offline(&self, offline: bool) {
        self.zone.lock().unwrap().offline = offline;
    }
    pub fn set_page_size(&self, n: usize) {
        self.zone.lock().unwrap().page_size = n;
    }
    pub fn expire_token_once(&self) {
        self.zone.lock().unwrap().expire_token_once = true;
    }
    /// Next `save_records` calls accept `n` records in total, then fail the
    /// rest with a transient error (partial batch success).
    pub fn fail_after_saves(&self, n: Option<usize>) {
        self.zone.lock().unwrap().fail_after_saves = n;
    }
    /// Simulate a concurrent write by another device between fetch and save.
    pub fn concurrent_write_before_next_save(&self, record: RawRecord) {
        self.zone.lock().unwrap().concurrent_write = Some(record);
    }
    pub fn save_calls(&self) -> usize {
        self.zone.lock().unwrap().save_calls
    }
    pub fn saved_records(&self) -> usize {
        self.zone.lock().unwrap().saved_records
    }
    pub fn put_record(&self, mut record: RawRecord) {
        let mut z = self.zone.lock().unwrap();
        z.seq += 1;
        let seq = z.seq;
        record.modified_ms = Some(seq as i64);
        record.change_tag = Some(seq.to_string());
        z.records.insert(record.record_name.clone(), (seq, record));
    }
    pub fn delete_record(&self, name: &str) {
        let mut z = self.zone.lock().unwrap();
        z.seq += 1;
        let seq = z.seq;
        z.records.remove(name);
        z.deletions.push((seq, name.to_string()));
    }
    pub fn record(&self, name: &str) -> Option<RawRecord> {
        self.zone
            .lock()
            .unwrap()
            .records
            .get(name)
            .map(|(_, r)| r.clone())
    }
    pub fn record_count(&self) -> usize {
        self.zone.lock().unwrap().records.len()
    }

    fn check(&self) -> SyncResult<()> {
        if self.zone.lock().unwrap().offline {
            Err(SyncError::RetryAfter {
                secs: 15.0,
                detail: "no network".into(),
            })
        } else {
            Ok(())
        }
    }
}

impl Backend for MemoryBackend {
    fn prepare(&self) -> SyncResult<()> {
        self.check()
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        self.check()?;
        let mut z = self.zone.lock().unwrap();
        if z.expire_token_once && since.is_some() {
            z.expire_token_once = false;
            return Err(SyncError::TokenExpired);
        }
        let token = since
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0);
        let mut changed: Vec<(u64, RawRecord)> = z
            .records
            .values()
            .filter(|(s, _)| *s > token)
            .cloned()
            .collect();
        changed.sort_by_key(|(s, _)| *s);
        let limit = if z.page_size == 0 {
            usize::MAX
        } else {
            z.page_size
        };
        let more = changed.len() > limit;
        changed.truncate(limit);
        let new_token = if more {
            changed.last().map(|(s, _)| *s).unwrap_or(token)
        } else {
            z.seq
        };
        let deleted: Vec<String> = z
            .deletions
            .iter()
            .filter(|(s, _)| *s > token && *s <= new_token)
            .map(|(_, n)| n.clone())
            .collect();
        Ok(ChangePage {
            records: changed.into_iter().map(|(_, r)| r).collect(),
            deleted_record_names: deleted,
            new_token: Some(new_token.to_le_bytes().to_vec()),
            more_coming: more,
        })
    }

    fn fetch_record(&self, name: &str) -> SyncResult<Option<RawRecord>> {
        self.check()?;
        Ok(self.record(name))
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        self.check()?;
        let injected = self.zone.lock().unwrap().concurrent_write.take();
        if let Some(r) = injected {
            self.put_record(r);
        }
        let mut out = Vec::with_capacity(records.len());
        let mut budget = {
            let mut z = self.zone.lock().unwrap();
            z.save_calls += 1;
            z.fail_after_saves
        };
        for r in records {
            match budget {
                Some(0) => out.push(RecordOutcome {
                    record_name: r.record_name.clone(),
                    result: Err(SyncError::Transient("injected save failure".into())),
                    server_record: None,
                }),
                _ => {
                    if policy == SavePolicy::IfServerUnchanged {
                        let server = self.record(&r.record_name);
                        let stale = match (&server, &r.change_tag) {
                            (Some(s), tag) => s.change_tag != *tag,
                            (None, Some(_)) => {
                                out.push(RecordOutcome {
                                    record_name: r.record_name.clone(),
                                    result: Err(SyncError::Transient("unknown item".into())),
                                    server_record: None,
                                });
                                continue;
                            }
                            (None, None) => false,
                        };
                        if stale {
                            out.push(RecordOutcome {
                                record_name: r.record_name.clone(),
                                result: Err(SyncError::Conflict),
                                server_record: server,
                            });
                            continue;
                        }
                    }
                    if let Some(b) = budget.as_mut() {
                        *b -= 1;
                    }
                    self.put_record(r.clone());
                    self.zone.lock().unwrap().saved_records += 1;
                    out.push(RecordOutcome {
                        record_name: r.record_name.clone(),
                        result: Ok(()),
                        server_record: None,
                    });
                }
            }
        }
        if let Some(b) = budget {
            self.zone.lock().unwrap().fail_after_saves = Some(b);
        }
        Ok(out)
    }
}
