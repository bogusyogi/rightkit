//! Send limits: how many messages an app may send an hour and a day, counted from a small log file.
//!
//! The log is a JSON array of Unix-second send times. A send is counted BEFORE it leaves, so an
//! uncertain answer still counts, and a log that cannot be read or written stops the send: no
//! message goes out without its count.

use std::path::Path;

/// The most sends allowed in the last hour and in the last day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendLimits {
    pub per_hour: usize,
    pub per_day: usize,
}

/// Why a send may not go ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendLimitError {
    /// The log could not be read (or no log path exists): nothing is sent.
    Unreadable,
    /// The log could not be written: nothing is sent.
    Unwritable,
    /// More than the hourly or daily limit.
    Reached,
}

fn now() -> i64 {
    rightkit_time::unix_millis_now() / 1000
}

/// The times of the sends in the last day. A log that cannot be read is an error.
pub fn recent(log: Option<&Path>) -> Result<Vec<i64>, SendLimitError> {
    let path = log.ok_or(SendLimitError::Unreadable)?;
    let mut times: Vec<i64> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| SendLimitError::Unreadable)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err(SendLimitError::Unreadable),
    };
    let cutoff = now() - 86_400;
    times.retain(|t| *t > cutoff);
    Ok(times)
}

/// `Reached` when one more send would pass a limit.
pub fn check(log: Option<&Path>, limits: SendLimits) -> Result<(), SendLimitError> {
    let times = recent(log)?;
    let hour = times.iter().filter(|t| **t > now() - 3600).count();
    if hour >= limits.per_hour || times.len() >= limits.per_day {
        return Err(SendLimitError::Reached);
    }
    Ok(())
}

/// Counts a send before it leaves.
pub fn count(log: Option<&Path>) -> Result<(), SendLimitError> {
    let mut times = recent(log)?;
    times.push(now());
    let path = log.ok_or(SendLimitError::Unwritable)?;
    let bytes = serde_json::to_vec(&times).map_err(|_| SendLimitError::Unwritable)?;
    rightkit_fs::atomic_write_private(path, &bytes).map_err(|_| SendLimitError::Unwritable)
}
