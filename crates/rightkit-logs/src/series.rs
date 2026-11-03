//! Metric series: counters, gauges and fixed-bucket latency histograms, rolled up into tiers with
//! time-based retention, a bounded total size and a range reader (issue #1277).
//!
//! Counts and numbers only: a series name is 1 to 64 characters of `a-z 0-9 . _ -` and no sample
//! carries text, so nothing here can hold a path, a name or a message.
//!
//! Storage is one JSONL file per tier in the store's directory (`<tier>.jsonl`), one record per
//! series per window. Samples accumulate in memory for the finest window; a window is written when
//! it closes. [`MetricsStore::maintain`] rolls closed windows of each finer tier into the next
//! coarser tier, deletes records older than each tier's retention, and, while the files together
//! exceed `max_total_bytes`, drops the oldest records of the finest tier first. Rollup progress
//! is kept per tier in `rollup.json`, so a restart neither repeats nor skips a window.
//!
//! Default tiers are 10 s kept 6 hours, 1 min kept 2 days, 15 min kept 14 days and 1 h kept 90
//! days.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Upper bounds, in milliseconds, of the latency histogram buckets. A final implicit bucket holds
/// everything above the last bound.
pub const LATENCY_BUCKETS_MS: [f64; 13] = [
    1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];
const BUCKETS: usize = LATENCY_BUCKETS_MS.len() + 1;

/// Which kind of series a record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesKind {
    /// A running count: windows add.
    Counter,
    /// A level: a window keeps count, sum, min, max and the last value.
    Gauge,
    /// A latency in milliseconds: a window keeps the above plus the bucket counts.
    Histogram,
}

/// One sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    Counter(u64),
    Gauge(f64),
    /// A latency in milliseconds.
    Latency(f64),
}

impl Sample {
    fn kind(self) -> SeriesKind {
        match self {
            Self::Counter(_) => SeriesKind::Counter,
            Self::Gauge(_) => SeriesKind::Gauge,
            Self::Latency(_) => SeriesKind::Histogram,
        }
    }
}

/// One rollup tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    /// File stem and identity: 1 to 32 characters of `a-z 0-9 _ -`.
    pub name: String,
    /// Window length in seconds. Each tier's window must be a multiple of the previous tier's.
    pub window_secs: u64,
    /// Records older than this are removed.
    pub retention_secs: u64,
}

impl Tier {
    pub fn new(name: &str, window_secs: u64, retention_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            window_secs,
            retention_secs,
        }
    }
}

/// Settings of a [`MetricsStore`].
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    pub dir: PathBuf,
    /// Finest tier first. Must be non-empty with strictly increasing windows, each a multiple of
    /// the one before.
    pub tiers: Vec<Tier>,
    /// All tier files together stay under this many bytes after [`MetricsStore::maintain`].
    pub max_total_bytes: u64,
}

impl MetricsConfig {
    /// The default tiers (10 s, 1 min, 15 min, 1 h) and a 64 MiB bound.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            tiers: vec![
                Tier::new("t10s", 10, 6 * 3600),
                Tier::new("t1m", 60, 2 * 86_400),
                Tier::new("t15m", 900, 14 * 86_400),
                Tier::new("t1h", 3600, 90 * 86_400),
            ],
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// One window of one series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Window start, Unix seconds.
    pub t: u64,
    pub kind: SeriesKind,
    /// Samples in the window (for a counter, the number of `Counter` calls).
    pub count: u64,
    /// Counter: the total added. Gauge and histogram: the sum of the values.
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    /// The most recent value (gauge, histogram).
    pub last: f64,
    /// Histogram bucket counts, one per [`LATENCY_BUCKETS_MS`] bound plus the overflow bucket;
    /// empty for other kinds.
    pub buckets: Vec<u64>,
}

impl Point {
    fn new(t: u64, kind: SeriesKind) -> Self {
        Self {
            t,
            kind,
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            last: 0.0,
            buckets: if kind == SeriesKind::Histogram {
                vec![0; BUCKETS]
            } else {
                Vec::new()
            },
        }
    }

    fn add(&mut self, sample: Sample) {
        let value = match sample {
            Sample::Counter(n) => n as f64,
            Sample::Gauge(v) | Sample::Latency(v) => v,
        };
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.last = value;
        if let Sample::Latency(ms) = sample {
            let index = LATENCY_BUCKETS_MS
                .iter()
                .position(|bound| ms <= *bound)
                .unwrap_or(BUCKETS - 1);
            self.buckets[index] += 1;
        }
    }

    /// Fold `later` (a window that starts at or after this one) into this point.
    fn merge(&mut self, later: &Point) {
        if later.count == 0 {
            return;
        }
        self.count += later.count;
        self.sum += later.sum;
        self.min = self.min.min(later.min);
        self.max = self.max.max(later.max);
        self.last = later.last;
        for (mine, theirs) in self.buckets.iter_mut().zip(&later.buckets) {
            *mine += theirs;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    s: String,
    #[serde(flatten)]
    point: Point,
}

#[derive(Default, Serialize, Deserialize)]
struct RollupState {
    /// Tier name -> end (exclusive) of the last finer-tier range already rolled into it.
    rolled_to: BTreeMap<String, u64>,
}

struct Inner {
    /// Series name -> its open finest-tier window.
    open: BTreeMap<String, Point>,
}

/// Counters, gauges and latency histograms with tiered rollup and retention.
pub struct MetricsStore {
    config: MetricsConfig,
    inner: Mutex<Inner>,
}

fn valid_series(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn valid_tier_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn now_secs() -> u64 {
    u64::try_from(crate::now_ms().max(0) / 1000).unwrap_or(0)
}

impl MetricsStore {
    /// Open (creating the directory) a store. Validates the tiers.
    pub fn open(config: MetricsConfig) -> io::Result<Self> {
        let tiers = &config.tiers;
        if tiers.is_empty() {
            return Err(invalid("a metrics store needs at least one tier"));
        }
        for (index, tier) in tiers.iter().enumerate() {
            if !valid_tier_name(&tier.name) || tier.window_secs == 0 || tier.retention_secs == 0 {
                return Err(invalid("invalid metrics tier"));
            }
            if tiers[..index].iter().any(|other| other.name == tier.name) {
                return Err(invalid("duplicate metrics tier name"));
            }
            if index > 0 {
                let finer = tiers[index - 1].window_secs;
                if tier.window_secs <= finer || tier.window_secs % finer != 0 {
                    return Err(invalid(
                        "tier windows must increase, each a multiple of the previous",
                    ));
                }
            }
        }
        fs::create_dir_all(&config.dir)?;
        Ok(Self {
            config,
            inner: Mutex::new(Inner {
                open: BTreeMap::new(),
            }),
        })
    }

    fn tier_path(&self, tier: &Tier) -> PathBuf {
        self.config.dir.join(format!("{}.jsonl", tier.name))
    }

    fn state_path(&self) -> PathBuf {
        self.config.dir.join("rollup.json")
    }

    /// Record `sample` for `series` now.
    pub fn record(&self, series: &str, sample: Sample) -> io::Result<()> {
        self.record_at(series, sample, now_secs())
    }

    /// Record `sample` for `series` at Unix second `at`. A sample for a window that is older
    /// than the series's open window is added to the open window (late samples never reopen a
    /// written one).
    pub fn record_at(&self, series: &str, sample: Sample, at: u64) -> io::Result<()> {
        if !valid_series(series) {
            return Err(invalid(
                "series names are 1 to 64 characters of a-z, 0-9, '.', '_' and '-'",
            ));
        }
        let finest = &self.config.tiers[0];
        let start = at - at % finest.window_secs;
        let mut closed = None;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let kind = sample.kind();
            let slot = inner
                .open
                .entry(series.to_string())
                .or_insert_with(|| Point::new(start, kind));
            if slot.kind != kind {
                return Err(invalid("a series keeps one kind"));
            }
            if start > slot.t {
                closed = Some(std::mem::replace(slot, Point::new(start, kind)));
            }
            slot.add(sample);
        }
        if let Some(point) = closed {
            self.append(&self.config.tiers[0], &[(series.to_string(), point)])?;
        }
        Ok(())
    }

    fn append(&self, tier: &Tier, points: &[(String, Point)]) -> io::Result<()> {
        use std::io::Write;
        if points.is_empty() {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.tier_path(tier))?;
        for (series, point) in points {
            let record = Record {
                s: series.clone(),
                point: point.clone(),
            };
            let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
            line.push(b'\n');
            file.write_all(&line)?;
        }
        Ok(())
    }

    /// Write every open window whose time has passed (`now`, Unix seconds) and run
    /// [`MetricsStore::maintain`].
    pub fn flush(&self, now: u64) -> io::Result<()> {
        let finest = self.config.tiers[0].window_secs;
        let current = now - now % finest;
        let closed: Vec<(String, Point)> = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let names: Vec<String> = inner
                .open
                .iter()
                .filter(|(_, point)| point.t < current)
                .map(|(name, _)| name.clone())
                .collect();
            names
                .into_iter()
                .filter_map(|name| inner.open.remove(&name).map(|point| (name, point)))
                .collect()
        };
        self.append(&self.config.tiers[0], &closed)?;
        self.maintain(now)
    }

    /// Write every open window, closed or not (shutdown), then maintain.
    pub fn close(&self, now: u64) -> io::Result<()> {
        let open: Vec<(String, Point)> = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut inner.open).into_iter().collect()
        };
        self.append(&self.config.tiers[0], &open)?;
        self.maintain(now)
    }

    fn read_tier(&self, tier: &Tier) -> io::Result<Vec<Record>> {
        let text = match fs::read_to_string(self.tier_path(tier)) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str::<Record>(line).ok())
            .collect())
    }

    fn write_tier(&self, tier: &Tier, records: &[Record]) -> io::Result<()> {
        let mut out = Vec::new();
        for record in records {
            serde_json::to_writer(&mut out, record).map_err(io::Error::other)?;
            out.push(b'\n');
        }
        crate::atomic::write_atomic(&self.tier_path(tier), &out)
    }

    fn read_state(&self) -> RollupState {
        fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Roll closed windows into coarser tiers, apply each tier's retention and the total size
    /// bound. Safe to call often; each step is idempotent.
    pub fn maintain(&self, now: u64) -> io::Result<()> {
        let tiers = &self.config.tiers;
        let mut state = self.read_state();
        for index in 1..tiers.len() {
            let (finer, coarse) = (&tiers[index - 1], &tiers[index]);
            let upto = now - now % coarse.window_secs;
            let from = state.rolled_to.get(&coarse.name).copied().unwrap_or(0);
            if upto <= from {
                continue;
            }
            let mut merged: BTreeMap<(u64, String), Point> = BTreeMap::new();
            for record in self.read_tier(finer)? {
                let t = record.point.t;
                if t < from || t >= upto {
                    continue;
                }
                let start = t - t % coarse.window_secs;
                merged
                    .entry((start, record.s))
                    .and_modify(|point| point.merge(&record.point))
                    .or_insert_with(|| Point {
                        t: start,
                        ..record.point.clone()
                    });
            }
            let rolled: Vec<(String, Point)> = merged
                .into_iter()
                .map(|((_, series), point)| (series, point))
                .collect();
            self.append(coarse, &rolled)?;
            state.rolled_to.insert(coarse.name.clone(), upto);
            let bytes = serde_json::to_vec(&state).map_err(io::Error::other)?;
            crate::atomic::write_atomic(&self.state_path(), &bytes)?;
        }
        for tier in tiers {
            let cutoff = now.saturating_sub(tier.retention_secs);
            let records = self.read_tier(tier)?;
            let kept: Vec<Record> = records
                .into_iter()
                .filter(|record| record.point.t >= cutoff)
                .collect();
            let before = fs::metadata(self.tier_path(tier))
                .map(|m| m.len())
                .unwrap_or(0);
            if before > 0 {
                self.write_tier(tier, &kept)?;
            }
        }
        self.enforce_size()
    }

    fn total_bytes(&self) -> u64 {
        self.config
            .tiers
            .iter()
            .filter_map(|tier| fs::metadata(self.tier_path(tier)).ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Drop the oldest records, finest tier first, until the files fit `max_total_bytes`.
    fn enforce_size(&self) -> io::Result<()> {
        for tier in &self.config.tiers {
            let total = self.total_bytes();
            if total <= self.config.max_total_bytes {
                return Ok(());
            }
            let mut records = self.read_tier(tier)?;
            if records.is_empty() {
                continue;
            }
            records.sort_by_key(|record| record.point.t);
            let mut excess = total - self.config.max_total_bytes;
            let mut drop = 0usize;
            for record in &records {
                if excess == 0 {
                    break;
                }
                let size = serde_json::to_vec(record).map_or(0, |bytes| bytes.len() as u64 + 1);
                excess = excess.saturating_sub(size);
                drop += 1;
            }
            self.write_tier(tier, &records[drop..])?;
        }
        Ok(())
    }

    /// Points of `series` between `from` (inclusive) and `to` (exclusive), Unix seconds, merged
    /// into buckets of `step` seconds aligned to `from`. The tier read is the coarsest whose
    /// window fits in `step` and whose retention still reaches back to `from`; when none does, the
    /// tier with the longest retention. Windows still open in memory are not included until
    /// [`MetricsStore::flush`] writes them.
    pub fn range(&self, series: &str, from: u64, to: u64, step: u64) -> io::Result<Vec<Point>> {
        if !valid_series(series) {
            return Err(invalid("invalid series name"));
        }
        if step == 0 || to <= from {
            return Err(invalid("range needs to > from and a non-zero step"));
        }
        let now = now_secs();
        let tiers = &self.config.tiers;
        let tier = tiers
            .iter()
            .rev()
            .filter(|tier| tier.window_secs <= step)
            .find(|tier| now.saturating_sub(from) <= tier.retention_secs)
            .unwrap_or_else(|| {
                tiers
                    .iter()
                    .max_by_key(|tier| tier.retention_secs)
                    .unwrap_or(&tiers[0])
            });
        let mut records: Vec<Record> = self
            .read_tier(tier)?
            .into_iter()
            .filter(|record| record.s == series && record.point.t >= from && record.point.t < to)
            .collect();
        records.sort_by_key(|record| record.point.t);
        let mut out: Vec<Point> = Vec::new();
        for record in records {
            let start = from + (record.point.t - from) / step * step;
            match out.last_mut() {
                Some(last) if last.t == start => last.merge(&record.point),
                _ => out.push(Point {
                    t: start,
                    ..record.point
                }),
            }
        }
        Ok(out)
    }

    /// Series names with at least one written record in the finest tier.
    pub fn series(&self) -> io::Result<Vec<String>> {
        let mut names: Vec<String> = self
            .read_tier(&self.config.tiers[0])?
            .into_iter()
            .map(|record| record.s)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// The directory holding the tier files.
    pub fn dir(&self) -> &Path {
        &self.config.dir
    }

    /// The last point of `series` as JSON, for a status endpoint.
    pub fn latest_json(&self, series: &str, now: u64) -> io::Result<Value> {
        let points = self.range(series, now.saturating_sub(3600), now + 1, 10)?;
        Ok(points
            .last()
            .and_then(|point| serde_json::to_value(point).ok())
            .unwrap_or(Value::Null))
    }
}
