//! The calling process's own resource readings, read straight from the OS, and a sliding window
//! over them, so a host can judge a sustained condition (CPU, footprint, threads, bytes written)
//! without a "since the last call" mark shared with another reader: each monitor keeps a
//! [`Window`] of its own.
//!
//! Readings: CPU time of the whole process (user plus system), footprint (macOS phys_footprint,
//! the figure the OS itself judges; Windows private bytes), thread count (the owner's
//! `process_usage_with_threads`), and bytes written (macOS `ri_diskio_byteswritten`, Windows
//! `WriteTransferCount`, which also counts pipe and network writes). A reading the OS did not
//! give is `None`, never zero. On other hosts [`read`] gives `None`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far a window may fall short of the span asked for and still answer.
const SLACK: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: Instant,
    /// CPU seconds used by the whole process since it started.
    pub cpu_secs: f64,
    pub footprint: Option<u64>,
    pub threads: Option<u32>,
    pub written: Option<u64>,
}

/// One reading of this process, or `None` when the OS gave no CPU time.
pub fn read() -> Option<Sample> {
    imp::read()
}

/// A bounded run of samples, oldest first.
pub struct Window {
    samples: VecDeque<Sample>,
    keep: usize,
}

impl Window {
    pub fn new(keep: usize) -> Window {
        Window {
            samples: VecDeque::new(),
            keep: keep.max(2),
        }
    }

    pub fn push(&mut self, sample: Sample) {
        self.samples.push_back(sample);
        while self.samples.len() > self.keep {
            self.samples.pop_front();
        }
    }

    pub fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }

    /// The samples of the last `span`, oldest first, or `None` when the window does not reach
    /// back that far yet (a sustained condition is never judged from a short history).
    fn covering(&self, span: Duration) -> Option<Vec<&Sample>> {
        let newest = self.samples.back()?;
        let inside: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| newest.at.duration_since(s.at) <= span + SLACK)
            .collect();
        let oldest = inside.first()?;
        (newest.at.duration_since(oldest.at) + SLACK >= span).then_some(inside)
    }

    /// Mean CPU over the last `span`, in percent of one core.
    pub fn cpu_mean(&self, span: Duration) -> Option<f64> {
        let inside = self.covering(span)?;
        let (first, last) = (inside.first()?, inside.last()?);
        let wall = last.at.duration_since(first.at).as_secs_f64();
        (wall > 0.0).then(|| ((last.cpu_secs - first.cpu_secs).max(0.0) / wall) * 100.0)
    }

    /// CPU percent of each of the last `n` intervals between samples, oldest first; `None` until
    /// `n` intervals exist.
    pub fn cpu_intervals(&self, n: usize) -> Option<Vec<f64>> {
        if self.samples.len() < n + 1 {
            return None;
        }
        let tail: Vec<&Sample> = self
            .samples
            .iter()
            .skip(self.samples.len() - n - 1)
            .collect();
        Some(
            tail.windows(2)
                .map(|pair| {
                    let wall = pair[1].at.duration_since(pair[0].at).as_secs_f64();
                    if wall > 0.0 {
                        ((pair[1].cpu_secs - pair[0].cpu_secs).max(0.0) / wall) * 100.0
                    } else {
                        0.0
                    }
                })
                .collect(),
        )
    }

    /// The smallest footprint seen over the last `span` (over budget for the whole span means
    /// the minimum is over it).
    pub fn footprint_min(&self, span: Duration) -> Option<u64> {
        let inside = self.covering(span)?;
        inside
            .iter()
            .map(|s| s.footprint)
            .collect::<Option<Vec<u64>>>()?
            .into_iter()
            .min()
    }

    pub fn threads_min(&self, span: Duration) -> Option<u32> {
        let inside = self.covering(span)?;
        inside
            .iter()
            .map(|s| s.threads)
            .collect::<Option<Vec<u32>>>()?
            .into_iter()
            .min()
    }

    /// Bytes written per hour over the last `span`.
    pub fn written_per_hour(&self, span: Duration) -> Option<f64> {
        let inside = self.covering(span)?;
        let (first, last) = (inside.first()?, inside.last()?);
        let wall = last.at.duration_since(first.at).as_secs_f64();
        let (a, b) = (first.written?, last.written?);
        (wall > 0.0).then(|| (b.saturating_sub(a) as f64) / wall * 3600.0)
    }
}

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use super::Sample;
    use std::time::Instant;

    /// One reading through the owner (`rightkit-child-process`): the usage counters and the live
    /// thread count from one `process_usage_with_threads` snapshot (the owner documents it as the
    /// costlier read, since it also opens each thread to name it). A host that cannot list
    /// threads still gives the usage, with the thread count unread (`None`, never zero).
    pub fn read() -> Option<Sample> {
        let (usage, threads) = match rightkit_child_process::process_usage_with_threads() {
            Ok(snapshot) => {
                let count = u32::try_from(snapshot.threads.len()).ok();
                (snapshot.usage, count)
            }
            Err(_) => (rightkit_child_process::process_usage().ok()?, None),
        };
        Some(Sample {
            at: Instant::now(),
            cpu_secs: usage.cpu_seconds,
            footprint: Some(usage.footprint_bytes),
            threads,
            written: usage.disk_write_bytes,
        })
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::Sample;

    pub fn read() -> Option<Sample> {
        None
    }
}
