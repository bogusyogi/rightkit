//! Recursive typed streams, with backend errors and rescan notices preserved.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvError, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::WatchEventRecord;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// A watch startup failure or a gap in a live stream, independent of `notify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchError {
    Backend {
        message: String,
        paths: Vec<PathBuf>,
    },
    /// Events were dropped; reconcile a filesystem snapshot before claiming freshness.
    Overflow,
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend { message, .. } => f.write_str(message),
            Self::Overflow => f.write_str("filesystem events were dropped; rescan required"),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<notify::Error> for WatchError {
    fn from(error: notify::Error) -> Self {
        Self::Backend {
            message: error.to_string(),
            paths: error.paths,
        }
    }
}

/// Filesystem hints. Backends may duplicate or coalesce hints; snapshots remain truth.
/// Paths use the backend's spelling; the registered root is canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Create(PathBuf),
    Modify(PathBuf),
    Delete(PathBuf),
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Error(WatchError),
    /// Backend overflow/dropped-event/rescan signal, even when it has no paths.
    Overflow,
}

/// Owns a recursive OS watch and a typed event stream. No async runtime required.
/// The queue is unbounded: callers should drain it regularly. Dropping this handle
/// stops the OS watch and joins its worker. Half-renames wait at most 250 ms for
/// a mate, then become Delete/Create. Tracker IDs take precedence; untracked
/// halves pair only when adjacent, as on ReadDirectoryChangesW.
pub struct TypedWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<thread::JoinHandle<()>>,
    rx: mpsc::Receiver<WatchEventRecord>,
}

impl TypedWatcher {
    pub(crate) fn start(path: &Path) -> Result<Self, WatchError> {
        let root = path.canonicalize().map_err(|error| WatchError::Backend {
            message: error.to_string(),
            paths: vec![path.to_path_buf()],
        })?;
        let (raw_tx, raw_rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |result| {
            let _ = raw_tx.send(result);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("rightkit-watch-typed".into())
            .spawn(move || run_translated(raw_rx, |record| tx.send(record).is_ok()))
            .map_err(|error| WatchError::Backend {
                message: error.to_string(),
                paths: vec![root],
            })?;
        Ok(Self {
            watcher: Some(watcher),
            thread: Some(thread),
            rx,
        })
    }

    /// Wait for a typed event. Backend failures arrive as events, not receive errors.
    pub fn recv(&self) -> Result<WatchEvent, RecvError> {
        self.rx.recv().map(|record| record.event)
    }

    /// Poll without blocking; Empty means there are no events ready yet.
    pub fn try_recv(&self) -> Result<WatchEvent, TryRecvError> {
        self.rx.try_recv().map(|record| record.event)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<WatchEvent, RecvTimeoutError> {
        self.rx.recv_timeout(timeout).map(|record| record.event)
    }

    /// Receive typed output with its original backend kind. Mixed typed/record
    /// receive calls consume the same queue, with no duplicate delivery.
    pub fn recv_record(&self) -> Result<WatchEventRecord, RecvError> {
        self.rx.recv()
    }

    pub fn try_recv_record(&self) -> Result<WatchEventRecord, TryRecvError> {
        self.rx.try_recv()
    }

    pub fn recv_record_timeout(
        &self,
        timeout: Duration,
    ) -> Result<WatchEventRecord, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}

impl Drop for TypedWatcher {
    fn drop(&mut self) {
        self.watcher.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const RENAME_WAIT: Duration = Duration::from_millis(250);

struct HalfRename {
    path: PathBuf,
    from: bool,
    deadline: Instant,
    raw_kind: EventKind,
}

impl HalfRename {
    fn degrade(self) -> WatchEventRecord {
        let event = if self.from {
            WatchEvent::Delete(self.path)
        } else {
            WatchEvent::Create(self.path)
        };
        WatchEventRecord {
            event,
            raw_kind: Some(self.raw_kind),
        }
    }
}

#[derive(Default)]
pub(crate) struct Translator {
    halves: HashMap<Option<usize>, HalfRename>,
    // inotify emits From, To, then Both with the same tracker. Suppress that echo.
    paired: HashMap<usize, (PathBuf, PathBuf, Instant)>,
}

impl Translator {
    #[cfg(test)]
    fn expire(&mut self, now: Instant) -> Vec<WatchEvent> {
        self.expire_records(now)
            .into_iter()
            .map(|record| record.event)
            .collect()
    }

    pub(crate) fn expire_records(&mut self, now: Instant) -> Vec<WatchEventRecord> {
        self.paired.retain(|_, (_, _, deadline)| *deadline > now);
        let keys: Vec<_> = self
            .halves
            .iter()
            .filter(|(_, half)| half.deadline <= now)
            .map(|(key, _)| *key)
            .collect();
        keys.into_iter()
            .filter_map(|key| self.halves.remove(&key))
            .map(HalfRename::degrade)
            .collect()
    }

    #[cfg(test)]
    fn flush(&mut self) -> Vec<WatchEvent> {
        self.flush_records()
            .into_iter()
            .map(|record| record.event)
            .collect()
    }

    pub(crate) fn flush_records(&mut self) -> Vec<WatchEventRecord> {
        self.paired.clear();
        self.halves
            .drain()
            .map(|(_, half)| half.degrade())
            .collect()
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.halves.values().map(|half| half.deadline).min()
    }

    // Pure seam: injected time and backend results, no filesystem or sleeping.
    #[cfg(test)]
    fn translate(&mut self, result: notify::Result<Event>, now: Instant) -> Vec<WatchEvent> {
        self.translate_records(result, now)
            .into_iter()
            .map(|record| record.event)
            .collect()
    }

    pub(crate) fn translate_records(
        &mut self,
        result: notify::Result<Event>,
        now: Instant,
    ) -> Vec<WatchEventRecord> {
        let mut out = self.expire_records(now);
        let event = match result {
            Ok(event) => event,
            Err(error) => {
                out.extend(self.flush_records());
                out.push(WatchEventRecord {
                    event: WatchEvent::Error(error.into()),
                    raw_kind: None,
                });
                return out;
            }
        };
        if event.need_rescan() {
            out.extend(self.flush_records());
            out.push(WatchEventRecord {
                event: WatchEvent::Overflow,
                raw_kind: Some(event.kind),
            });
        }
        let event_kind = event.kind;
        let record = |event| WatchEventRecord {
            event,
            raw_kind: Some(event_kind),
        };
        let tracker = event.tracker();
        // Without a correlation ID, only adjacent complementary halves can pair.
        let complementary = matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::To))
        ) && tracker.is_none()
            && event.paths.len() == 1;
        let completes_untracked = tracker.is_none()
            && matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            )
            && event.paths.len() == 2;
        if !complementary && !completes_untracked {
            if let Some(half) = self.halves.remove(&None) {
                out.push(half.degrade());
            }
        }
        match event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::To)) => {
                let from = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Name(RenameMode::From))
                );
                for path in event.paths {
                    if let Some(old) = self.halves.remove(&tracker) {
                        if old.from != from {
                            let (from, to) = if from {
                                (path, old.path)
                            } else {
                                (old.path, path)
                            };
                            if let Some(tracker) = tracker {
                                self.paired
                                    .insert(tracker, (from.clone(), to.clone(), now + RENAME_WAIT));
                            }
                            out.push(record(WatchEvent::Rename { from, to }));
                            continue;
                        }
                        out.push(old.degrade());
                    }
                    self.halves.insert(
                        tracker,
                        HalfRename {
                            path,
                            from,
                            deadline: now + RENAME_WAIT,
                            raw_kind: event_kind,
                        },
                    );
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if let [from, to] = event.paths.as_slice() {
                    if let Some(half) = self.halves.remove(&tracker) {
                        let endpoint = if half.from { from } else { to };
                        if half.path != *endpoint {
                            out.push(half.degrade());
                        }
                    }
                    let echo = tracker
                        .and_then(|id| self.paired.get(&id))
                        .is_some_and(|(old_from, old_to, _)| old_from == from && old_to == to);
                    if !echo {
                        out.push(record(WatchEvent::Rename {
                            from: from.clone(),
                            to: to.clone(),
                        }));
                    }
                } else {
                    // Malformed/unpaired "Both" still provides conservative path hints.
                    out.extend(
                        event
                            .paths
                            .into_iter()
                            .map(|path| record(WatchEvent::Modify(path))),
                    );
                }
            }
            EventKind::Create(_) => out.extend(
                event
                    .paths
                    .into_iter()
                    .map(|path| record(WatchEvent::Create(path))),
            ),
            EventKind::Remove(_) => out.extend(
                event
                    .paths
                    .into_iter()
                    .map(|path| record(WatchEvent::Delete(path))),
            ),
            EventKind::Modify(_) | EventKind::Any => {
                out.extend(
                    event
                        .paths
                        .into_iter()
                        .map(|path| record(WatchEvent::Modify(path))),
                );
            }
            EventKind::Access(_) | EventKind::Other => {}
        }
        out
    }
}

#[cfg(test)]
fn run(rx: mpsc::Receiver<notify::Result<Event>>, tx: mpsc::Sender<WatchEvent>) {
    run_translated(rx, |record| tx.send(record.event).is_ok());
}

fn run_translated(
    rx: mpsc::Receiver<notify::Result<Event>>,
    mut send: impl FnMut(WatchEventRecord) -> bool,
) {
    let mut translator = Translator::default();
    loop {
        let result = match translator.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let events = match result {
            Ok(result) => translator.translate_records(result, Instant::now()),
            Err(RecvTimeoutError::Timeout) => translator.expire_records(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => {
                for event in translator.flush_records() {
                    if !send(event) {
                        return;
                    }
                }
                return;
            }
        };
        for event in events {
            if !send(event) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, Flag};

    fn half(mode: RenameMode, path: &str, tracker: Option<usize>) -> Event {
        let event = Event::new(EventKind::Modify(ModifyKind::Name(mode))).add_path(path.into());
        match tracker {
            Some(id) => event.set_tracker(id),
            None => event,
        }
    }

    fn rename(from: &str, to: &str) -> WatchEvent {
        WatchEvent::Rename {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn tracker_pairs_interleaved_renames_and_reversed_halves() {
        let now = Instant::now();
        let mut translator = Translator::default();
        assert!(translator
            .translate(Ok(half(RenameMode::From, "a", Some(1))), now)
            .is_empty());
        assert!(translator
            .translate(Ok(half(RenameMode::To, "d", Some(2))), now)
            .is_empty());
        assert_eq!(
            translator.translate(Ok(half(RenameMode::To, "b", Some(1))), now),
            vec![rename("a", "b")]
        );
        assert_eq!(
            translator.translate(Ok(half(RenameMode::From, "c", Some(2))), now),
            vec![rename("c", "d")]
        );
        assert!(translator.expire(now + RENAME_WAIT).is_empty());
    }

    #[test]
    fn untracked_adjacent_halves_pair_but_intervening_event_breaks_pair() {
        let now = Instant::now();
        let mut translator = Translator::default();
        assert!(translator
            .translate(Ok(half(RenameMode::From, "a", None)), now)
            .is_empty());
        assert_eq!(
            translator.translate(Ok(half(RenameMode::To, "b", None)), now),
            vec![rename("a", "b")]
        );

        translator.translate(Ok(half(RenameMode::From, "c", None)), now);
        let event = Event::new(EventKind::Create(CreateKind::File)).add_path("noise".into());
        assert_eq!(
            translator.translate(Ok(event), now),
            vec![
                WatchEvent::Delete("c".into()),
                WatchEvent::Create("noise".into())
            ]
        );
        translator.translate(Ok(half(RenameMode::To, "d", None)), now);
        assert_eq!(
            translator.expire(now + RENAME_WAIT),
            vec![WatchEvent::Create("d".into())]
        );
    }

    #[test]
    fn unmatched_and_mismatched_halves_degrade_after_deadline() {
        let now = Instant::now();
        let mut translator = Translator::default();
        translator.translate(Ok(half(RenameMode::From, "a", Some(1))), now);
        translator.translate(Ok(half(RenameMode::To, "b", Some(2))), now);
        assert!(translator.expire(now + RENAME_WAIT / 2).is_empty());
        let output = translator.expire(now + RENAME_WAIT);
        assert_eq!(output.len(), 2);
        assert!(output.contains(&WatchEvent::Delete("a".into())));
        assert!(output.contains(&WatchEvent::Create("b".into())));
        assert!(translator.halves.is_empty());
    }

    #[test]
    fn explicit_both_discards_pending_half_and_suppresses_inotify_echo() {
        let now = Instant::now();
        let both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("a".into())
            .add_path("b".into())
            .set_tracker(1);
        let mut translator = Translator::default();
        translator.translate(Ok(half(RenameMode::From, "a", Some(1))), now);
        assert_eq!(
            translator.translate(Ok(both.clone()), now),
            vec![rename("a", "b")]
        );
        assert!(translator.expire(now + RENAME_WAIT).is_empty());

        translator.translate(Ok(half(RenameMode::From, "a", Some(1))), now);
        assert_eq!(
            translator.translate(Ok(half(RenameMode::To, "b", Some(1))), now),
            vec![rename("a", "b")]
        );
        assert!(translator.translate(Ok(both), now).is_empty());
    }

    #[test]
    fn untracked_both_consumes_matching_half_without_losing_unrelated_half() {
        let now = Instant::now();
        let both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("a".into())
            .add_path("b".into());
        let mut translator = Translator::default();
        translator.translate(Ok(half(RenameMode::From, "a", None)), now);
        assert_eq!(
            translator.translate(Ok(both.clone()), now),
            vec![rename("a", "b")]
        );
        translator.translate(Ok(half(RenameMode::From, "unrelated", None)), now);
        assert_eq!(
            translator.translate(Ok(both), now),
            vec![WatchEvent::Delete("unrelated".into()), rename("a", "b"),]
        );
        assert!(translator.flush().is_empty());
    }

    #[test]
    fn backend_errors_and_pathless_rescan_reach_output_stream() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let error = notify::Error::generic("backend disconnected").add_path("root".into());
        let expected = WatchError::Backend {
            message: error.to_string(),
            paths: error.paths.clone(),
        };
        raw_tx.send(Err(error)).unwrap();
        raw_tx
            .send(Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)))
            .unwrap();
        drop(raw_tx);
        // Same worker seam used by the real backend; no OS watch or timing needed.
        run(raw_rx, tx);
        assert_eq!(
            rx.into_iter().collect::<Vec<_>>(),
            vec![WatchEvent::Error(expected), WatchEvent::Overflow]
        );
    }

    #[test]
    fn gap_breaks_rename_correlation_and_retains_mutating_rescan_paths() {
        let now = Instant::now();
        let mut translator = Translator::default();
        translator.translate(Ok(half(RenameMode::From, "a", Some(1))), now);
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path("new".into())
            .set_flag(Flag::Rescan);
        assert_eq!(
            translator.translate(Ok(event), now),
            vec![
                WatchEvent::Delete("a".into()),
                WatchEvent::Overflow,
                WatchEvent::Create("new".into()),
            ]
        );
        translator.translate(Ok(half(RenameMode::To, "b", Some(1))), now);
        assert_eq!(translator.flush(), vec![WatchEvent::Create("b".into())]);
    }

    #[test]
    fn stream_disconnect_flushes_unpaired_half() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        raw_tx
            .send(Ok(half(RenameMode::From, "a", Some(1))))
            .unwrap();
        drop(raw_tx);
        run(raw_rx, tx);
        assert_eq!(
            rx.into_iter().collect::<Vec<_>>(),
            vec![WatchEvent::Delete("a".into())]
        );
    }
}
