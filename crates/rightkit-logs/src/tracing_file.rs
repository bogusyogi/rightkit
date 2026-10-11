//! Daily-rolling JSONL file logs for `tracing` (feature `tracing`).
//!
//! Files are named `<file_prefix>.YYYY-MM-DD.jsonl` by UTC day. Opening a day's file prunes
//! the directory to the newest `max_files` matching files, today's included, oldest first.
//! Lines go through one worker thread behind a bounded queue. A full queue drops the line and
//! counts it, so callers never block. Dropping the [`TracingGuard`] writes every line accepted
//! before the drop, then joins the worker.

use crate::{
    collect_diagnostics, now_ms, BundleMetadata, CollectionPolicy, DiagnosticsBundle, LogSource,
};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

const DEFAULT_MAX_FILES: usize = 3;
/// Default bounded queue depth, matching `tracing-appender`'s buffered-lines limit.
const DEFAULT_QUEUE_LINES: usize = 128_000;
/// How long shutdown keeps retrying to queue its stop request on a full queue.
const DEFAULT_SHUTDOWN_SEND_TIMEOUT: Duration = Duration::from_millis(100);
/// How long shutdown waits for the writer to acknowledge the stop request.
const DEFAULT_SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
const DEFAULT_DIRECTIVE: &str = "info";
const MS_PER_DAY: i64 = 86_400_000;

/// Days since the Unix epoch (UTC) for the current writer. Injected so tests can roll days.
type DayClock = Box<dyn Fn() -> i64 + Send>;

/// Settings for daily-rolling JSONL file logs. [`TracingFileConfig::new`] sets the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracingFileConfig {
    /// Directory that receives the daily files. Created when missing.
    pub dir: PathBuf,
    /// Stem of the daily file names, e.g. `scraperight` for `scraperight.2026-10-11.jsonl`.
    /// Must be non-empty and free of path separators.
    pub file_prefix: String,
    /// Daily files kept, today's included. `0` behaves as `1`.
    pub max_files: usize,
    /// `true` writes JSON lines; `false` writes plain text lines.
    pub json: bool,
    /// JSON only: event fields appear at the top level instead of under `fields`.
    pub flatten_event: bool,
    /// Filter directive such as `info,engine=debug`. `None` reads `RUST_LOG`, then `info`.
    pub env_filter: Option<String>,
    /// Lines the writer queue holds before a line is dropped and counted. `0` behaves as `1`.
    /// Default 128_000, the same limit `tracing-appender` buffers before it drops.
    pub queue_lines: usize,
    /// Guard drop: how long to keep retrying the stop request when the queue is full. Default
    /// 100 ms.
    pub shutdown_send_timeout: Duration,
    /// Guard drop: how long to wait for the writer to flush and acknowledge. Default 1 s. When
    /// either timeout passes, the guard detaches the worker and adds the unwritten lines to
    /// [`TracingGuard::dropped_events`], so a stalled filesystem cannot hang shutdown.
    pub shutdown_wait: Duration,
}

impl TracingFileConfig {
    /// Defaults: 3 daily files, JSON lines, flattened events, no explicit filter, and a
    /// 128_000-line queue.
    pub fn new(dir: impl Into<PathBuf>, file_prefix: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            file_prefix: file_prefix.into(),
            max_files: DEFAULT_MAX_FILES,
            json: true,
            flatten_event: true,
            env_filter: None,
            queue_lines: DEFAULT_QUEUE_LINES,
            shutdown_send_timeout: DEFAULT_SHUTDOWN_SEND_TIMEOUT,
            shutdown_wait: DEFAULT_SHUTDOWN_WAIT,
        }
    }
}

/// Keeps the file writer alive. Dropping it writes every line accepted so far and joins the
/// worker thread. Hold it for the life of the process, or until shutdown.
#[must_use = "dropping the guard flushes and stops the log writer"]
pub struct TracingGuard {
    sender: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    /// Lines accepted into the queue and not yet written (or failed).
    pending: Arc<AtomicU64>,
    queue_lines: usize,
    worker: Option<JoinHandle<()>>,
    shutdown_send_timeout: Duration,
    shutdown_wait: Duration,
}

/// Runtime figures of the log writer, computed on call by [`TracingGuard::status`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogWriterStatus {
    /// Lines accepted into the queue and not yet written (the backlog).
    pub pending_lines: u64,
    /// Lines dropped so far (see [`TracingGuard::dropped_events`]).
    pub dropped_lines: u64,
    /// The queue limit the writer was configured with.
    pub queue_lines: usize,
    /// Resident memory, CPU time and I/O counters of the whole process (the writer shares it),
    /// or `None` when the host cannot report them. See `rightkit_process::ProcessUsage`.
    pub process: Option<rightkit_process::ProcessUsage>,
}

impl TracingGuard {
    /// Backlog, drops and process resource usage, read now. No polling thread runs for this.
    pub fn status(&self) -> LogWriterStatus {
        LogWriterStatus {
            pending_lines: self.pending.load(Ordering::Relaxed),
            dropped_lines: self.dropped.load(Ordering::Relaxed),
            queue_lines: self.queue_lines,
            process: rightkit_process::process_usage().ok(),
        }
    }

    /// Lines dropped because the queue was full, the writer had stopped, or shutdown gave up on
    /// a stalled writer. A line counted at shutdown may still be written later by the detached
    /// worker, so the count is an upper bound for that case.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Detaches the worker (the handle is dropped, which leaves the thread running) and counts
    /// the lines it has not finished writing.
    fn abandon(&mut self, _detached_worker: JoinHandle<()>) {
        let unwritten = self.pending.load(Ordering::Relaxed);
        self.dropped.fetch_add(unwritten, Ordering::Relaxed);
    }
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        // The stop request queues behind every accepted line, so the writer flushes them all
        // before it acknowledges. Both waits are bounded.
        let (ack_tx, ack_rx) = mpsc::sync_channel::<()>(1);
        let deadline = Instant::now() + self.shutdown_send_timeout;
        let mut message = Message::Stop(ack_tx);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => {
                    let _ = worker.join();
                    return;
                }
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        self.abandon(worker);
                        return;
                    }
                    message = returned;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        match ack_rx.recv_timeout(self.shutdown_wait) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
            }
            Err(RecvTimeoutError::Timeout) => self.abandon(worker),
        }
    }
}

/// Install a global `tracing` subscriber that writes daily JSONL files under `config.dir`.
///
/// Fails on an invalid prefix, an invalid filter directive, an unusable directory, or when a
/// global subscriber is already installed. Retention runs before this returns.
pub fn install_tracing(config: TracingFileConfig) -> io::Result<TracingGuard> {
    validate_prefix(&config.file_prefix)?;
    let filter = parse_filter(config.env_filter.as_deref())?;
    let mut writer = DailyJsonlWriter::new(&config, Box::new(utc_day_now));
    writer.refresh()?;
    let (channel, guard) = spawn_worker(writer, &config)?;
    tracing_subscriber::registry()
        .with(file_layer::<Registry>(&config, channel, filter))
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(guard)
}

/// A file layer for apps that compose their own subscriber, such as one that also writes
/// stderr. Opens today's file and applies retention before it returns. Setup failures are
/// reported on stderr and retried on the next write, so this never fails the caller.
pub fn tracing_layer<S>(
    config: TracingFileConfig,
) -> (Box<dyn Layer<S> + Send + Sync>, TracingGuard)
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let filter = match parse_filter(config.env_filter.as_deref()) {
        Ok(filter) => filter,
        Err(error) => {
            report(&config.dir, &error);
            EnvFilter::new(DEFAULT_DIRECTIVE)
        }
    };
    let mut writer = DailyJsonlWriter::new(&config, Box::new(utc_day_now));
    if let Err(error) = writer.refresh() {
        report(&config.dir, &error);
    }
    let (channel, guard) = match spawn_worker(writer, &config) {
        Ok(pair) => pair,
        Err(error) => {
            report(&config.dir, &error);
            detached()
        }
    };
    (file_layer::<S>(&config, channel, filter), guard)
}

/// Checked form of [`tracing_layer`]. Returns `Err` instead of reporting to stderr when the
/// prefix, the filter directive, the directory, or today's file open fails, or when the writer
/// thread cannot start. The never-failing [`tracing_layer`] is unchanged.
pub fn tracing_layer_checked<S>(
    config: TracingFileConfig,
) -> io::Result<(Box<dyn Layer<S> + Send + Sync>, TracingGuard)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    validate_prefix(&config.file_prefix)?;
    let filter = parse_filter(config.env_filter.as_deref())?;
    let mut writer = DailyJsonlWriter::new(&config, Box::new(utc_day_now));
    writer.refresh()?;
    let (channel, guard) = spawn_worker(writer, &config)?;
    Ok((file_layer::<S>(&config, channel, filter), guard))
}

/// Daily files that `collect` should read, oldest first, named `<source_name>-<index>`.
///
/// Returns the newest `max_files` matching files. With none, or when the directory cannot be
/// listed, it returns one source for today's expected file, which the collector reports as
/// missing or unreadable.
pub fn tracing_log_sources(config: &TracingFileConfig, source_name: &str) -> Vec<LogSource> {
    let today = config
        .dir
        .join(daily_file_name(&config.file_prefix, utc_day_now()));
    let names = match daily_file_names(&config.dir, &config.file_prefix) {
        Ok(names) => names,
        Err(_) => return vec![LogSource::new(source_name, today)],
    };
    let keep = config.max_files.max(1);
    let selected = &names[names.len().saturating_sub(keep)..];
    if selected.is_empty() {
        return vec![LogSource::new(source_name, today)];
    }
    selected
        .iter()
        .enumerate()
        .map(|(index, name)| {
            LogSource::new(format!("{source_name}-{index}"), config.dir.join(name))
        })
        .collect()
}

/// Support-bundle collection over the daily files from [`tracing_log_sources`], using the same
/// bounded, redacted policy as [`collect_diagnostics`].
pub fn collect_tracing_diagnostics(
    config: &TracingFileConfig,
    source_name: &str,
    metadata: BundleMetadata,
    policy: &CollectionPolicy,
) -> DiagnosticsBundle {
    collect_diagnostics(&tracing_log_sources(config, source_name), metadata, policy)
}

fn file_layer<S>(
    config: &TracingFileConfig,
    channel: Channel,
    filter: EnvFilter,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    if config.json {
        fmt::layer()
            .json()
            .flatten_event(config.flatten_event)
            .with_target(true)
            .with_ansi(false)
            .with_writer(channel)
            .with_filter(filter)
            .boxed()
    } else {
        fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(channel)
            .with_filter(filter)
            .boxed()
    }
}

fn parse_filter(directive: Option<&str>) -> io::Result<EnvFilter> {
    match directive {
        Some(directive) => EnvFilter::try_new(directive)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string())),
        None => {
            Ok(EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_DIRECTIVE)))
        }
    }
}

fn validate_prefix(prefix: &str) -> io::Result<()> {
    if prefix.is_empty() || prefix.contains(['/', '\\', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file_prefix must be non-empty and contain no path separators",
        ));
    }
    Ok(())
}

fn report(dir: &Path, error: &io::Error) {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = writeln!(
        stderr,
        "[rightkit-logs] tracing file logs in {} unavailable ({error})",
        dir.display()
    );
}

/// Owns the open daily file and the retention rules. Used only on the worker thread.
struct DailyJsonlWriter {
    dir: PathBuf,
    prefix: String,
    max_files: usize,
    today: DayClock,
    day: Option<i64>,
    file: Option<File>,
}

impl DailyJsonlWriter {
    fn new(config: &TracingFileConfig, today: DayClock) -> Self {
        Self {
            dir: config.dir.clone(),
            prefix: config.file_prefix.clone(),
            max_files: config.max_files.max(1),
            today,
            day: None,
            file: None,
        }
    }

    /// Open the file for the current UTC day when the day changed, then apply retention.
    fn refresh(&mut self) -> io::Result<()> {
        let day = (self.today)();
        if self.file.is_some() && self.day == Some(day) {
            return Ok(());
        }
        self.file = None;
        self.day = None;
        validate_prefix(&self.prefix)?;
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(daily_file_name(&self.prefix, day));
        self.file = Some(OpenOptions::new().create(true).append(true).open(path)?);
        self.day = Some(day);
        // Retention is best-effort: a failed prune must not stop the day's writes.
        if let Err(error) = prune(&self.dir, &self.prefix, self.max_files) {
            report(&self.dir, &error);
        }
        Ok(())
    }
}

impl Write for DailyJsonlWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.refresh()?;
        match self.file.as_mut() {
            Some(file) => file.write(buf),
            None => Err(io::Error::other("tracing log file is not open")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

enum Message {
    Line(Vec<u8>),
    /// Flush, then acknowledge on the given channel and stop.
    Stop(SyncSender<()>),
}

/// Producer side of the queue. Cloned per event by `MakeWriter`; `write` never blocks.
#[derive(Clone)]
struct Channel {
    sender: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    pending: Arc<AtomicU64>,
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Count before queueing so the writer's decrement can never run first.
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.sender.try_send(Message::Line(buf.to_vec())).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Channel {
    type Writer = Channel;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn spawn_worker(
    writer: DailyJsonlWriter,
    config: &TracingFileConfig,
) -> io::Result<(Channel, TracingGuard)> {
    let (sender, receiver) = mpsc::sync_channel(config.queue_lines.max(1));
    let dropped = Arc::new(AtomicU64::new(0));
    let pending = Arc::new(AtomicU64::new(0));
    let worker = {
        let pending = Arc::clone(&pending);
        thread::Builder::new()
            .name("rightkit-tracing-writer".into())
            .spawn(move || run_worker(writer, receiver, pending))?
    };
    let channel = Channel {
        sender: sender.clone(),
        dropped: Arc::clone(&dropped),
        pending: Arc::clone(&pending),
    };
    Ok((
        channel,
        TracingGuard {
            sender,
            dropped,
            pending,
            queue_lines: config.queue_lines.max(1),
            worker: Some(worker),
            shutdown_send_timeout: config.shutdown_send_timeout,
            shutdown_wait: config.shutdown_wait,
        },
    ))
}

/// Fallback when the worker cannot start: the receiver is gone, so every line is counted as
/// dropped instead of failing the caller.
fn detached() -> (Channel, TracingGuard) {
    let (sender, _receiver) = mpsc::sync_channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let pending = Arc::new(AtomicU64::new(0));
    let channel = Channel {
        sender: sender.clone(),
        dropped: Arc::clone(&dropped),
        pending: Arc::clone(&pending),
    };
    (
        channel,
        TracingGuard {
            sender,
            dropped,
            pending,
            queue_lines: 1,
            worker: None,
            shutdown_send_timeout: DEFAULT_SHUTDOWN_SEND_TIMEOUT,
            shutdown_wait: DEFAULT_SHUTDOWN_WAIT,
        },
    )
}

fn run_worker(mut writer: DailyJsonlWriter, receiver: Receiver<Message>, pending: Arc<AtomicU64>) {
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Line(bytes) => {
                if let Err(error) = writer.write_all(&bytes) {
                    report(&writer.dir, &error);
                }
                pending.fetch_sub(1, Ordering::Relaxed);
            }
            Message::Stop(ack) => {
                let _ = writer.flush();
                let _ = ack.send(());
                return;
            }
        }
    }
    let _ = writer.flush();
}

fn utc_day_now() -> i64 {
    now_ms().div_euclid(MS_PER_DAY)
}

fn daily_file_name(prefix: &str, day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    format!("{prefix}.{year:04}-{month:02}-{date:02}.jsonl")
}

/// Civil (proleptic Gregorian) date for days since 1970-01-01, after Howard Hinnant.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn is_daily_file_name(name: &str, prefix: &str) -> bool {
    let Some(stem) = name
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".jsonl"))
    else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// Matching daily file names, oldest first. A missing directory has none.
fn daily_file_names(dir: &Path, prefix: &str) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_daily_file_name(name, prefix))
        .collect();
    // `YYYY-MM-DD` sorts chronologically.
    names.sort();
    Ok(names)
}

fn prune(dir: &Path, prefix: &str, keep: usize) -> io::Result<()> {
    let names = daily_file_names(dir, prefix)?;
    let excess = names.len().saturating_sub(keep);
    for name in &names[..excess] {
        match fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rightkit-tracing-unit-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create tracing unit fixture");
        path
    }

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(
            daily_file_name("scraperight", 19_782),
            "scraperight.2024-02-29.jsonl"
        );
    }

    #[test]
    fn only_exact_daily_names_match_the_prefix() {
        assert!(is_daily_file_name(
            "scraperight.2026-10-11.jsonl",
            "scraperight"
        ));
        for other in [
            "scraperight-engine.2026-10-11.jsonl",
            "scraperight.current.jsonl",
            "scraperight.2026-10-11.jsonl.1",
            "scraperight.2026-1-11.jsonl",
            "scraperight.2026/10/11.jsonl",
            "xscraperight.2026-10-11.jsonl",
        ] {
            assert!(!is_daily_file_name(other, "scraperight"), "{other}");
        }
    }

    #[test]
    fn rolling_to_a_new_day_opens_a_new_file_and_prunes_oldest() {
        let dir = temp_dir("rotation");
        let day = Arc::new(AtomicI64::new(100));
        let clock = Arc::clone(&day);
        let config = TracingFileConfig {
            max_files: 2,
            ..TracingFileConfig::new(&dir, "p")
        };
        let mut writer =
            DailyJsonlWriter::new(&config, Box::new(move || clock.load(Ordering::SeqCst)));

        writer.write_all(b"a\n").expect("write day 100");
        day.store(101, Ordering::SeqCst);
        writer.write_all(b"b\n").expect("write day 101");
        assert_eq!(
            daily_file_names(&dir, "p").expect("list"),
            vec!["p.1970-04-11.jsonl", "p.1970-04-12.jsonl"]
        );

        day.store(102, Ordering::SeqCst);
        writer.write_all(b"c\n").expect("write day 102");
        writer.flush().expect("flush");
        assert_eq!(
            daily_file_names(&dir, "p").expect("list"),
            vec!["p.1970-04-12.jsonl", "p.1970-04-13.jsonl"]
        );
        let newest = fs::read_to_string(dir.join("p.1970-04-13.jsonl")).expect("read newest");
        assert_eq!(newest, "c\n");

        fs::remove_dir_all(dir).expect("remove tracing unit fixture");
    }

    #[test]
    fn zero_max_files_keeps_the_current_file() {
        let dir = temp_dir("zero-keep");
        let config = TracingFileConfig {
            max_files: 0,
            ..TracingFileConfig::new(&dir, "p")
        };
        let mut writer = DailyJsonlWriter::new(&config, Box::new(|| 5));
        writer.refresh().expect("open");
        assert_eq!(daily_file_names(&dir, "p").expect("list").len(), 1);
        fs::remove_dir_all(dir).expect("remove tracing unit fixture");
    }

    #[cfg(unix)]
    #[test]
    fn stalled_writer_does_not_hang_guard_drop() {
        use std::os::fd::OwnedFd;
        let dir = temp_dir("stall");
        let config = TracingFileConfig {
            shutdown_send_timeout: Duration::from_millis(50),
            shutdown_wait: Duration::from_millis(200),
            ..TracingFileConfig::new(&dir, "p")
        };
        let mut writer = DailyJsonlWriter::new(&config, Box::new(|| 1));
        // A pipe nobody reads: once its buffer fills, the worker's write blocks, like a stalled
        // filesystem. Day 1 matches the writer's clock, so no file is reopened.
        let (reader, pipe_writer) = std::io::pipe().expect("pipe");
        writer.file = Some(File::from(OwnedFd::from(pipe_writer)));
        writer.day = Some(1);
        let (channel, guard) = spawn_worker(writer, &config).expect("worker starts");
        let dropped = Arc::clone(&guard.dropped);
        let subscriber = Registry::default().with(file_layer::<Registry>(
            &config,
            channel,
            EnvFilter::new("info"),
        ));
        let payload = "x".repeat(512);
        tracing::subscriber::with_default(subscriber, || {
            for sequence in 0..2_000 {
                tracing::info!(sequence, payload = payload.as_str(), "stall");
            }
        });
        let started = std::time::Instant::now();
        drop(guard);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "guard drop must be bounded, took {:?}",
            started.elapsed()
        );
        assert!(
            dropped.load(Ordering::Relaxed) > 0,
            "unwritten lines are counted"
        );
        // Let the detached worker finish: closing the reader makes its writes fail fast.
        drop(reader);
        fs::remove_dir_all(dir).expect("remove tracing unit fixture");
    }

    #[test]
    fn detached_fallback_counts_lines_without_failing() {
        let (mut channel, guard) = detached();
        channel.write_all(b"lost\n").expect("write never fails");
        assert_eq!(guard.dropped_events(), 1);
    }
}
