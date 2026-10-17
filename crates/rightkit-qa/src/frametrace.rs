//! Owned PresentMon frame-timing sessions (#433): start a trace for one process, confirm it is
//! recording, stop it and summarise the frame times. PresentMon is an external Windows tool; the
//! caller supplies its path. The stats come from its CSV output (`MsBetweenPresents` and, when
//! present, `Dropped`). A trace that is dropped unstopped kills its PresentMon child.
use crate::util::{err, Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The process to trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// By process id (`-process_id`).
    Pid(u32),
    /// By executable name, for example `CutRight.exe` (`-process_name`).
    Name(String),
}

/// Frame timing of one trace. Percentiles are nearest-rank over the frame intervals in ms.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrameStats {
    pub present_count: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// Presents PresentMon marked as dropped (`Dropped` column is `1` or `true`).
    pub drops: u64,
}

/// A running PresentMon session. Call [`FrameTrace::stop`] to end it and read the stats.
pub struct FrameTrace {
    child: Option<Child>,
    output: PathBuf,
}

/// Start PresentMon for `target`, writing its CSV to `output`. The arguments are PresentMon 1.x's
/// command line (`-process_id` or `-process_name`, `-output_file`, `-no_top`,
/// `-stop_existing_session`).
pub fn start(presentmon: &Path, target: &Target, output: &Path) -> Result<FrameTrace> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(output);
    let mut cmd = Command::new(presentmon);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: PresentMon writes its CSV; never flash a console
    }
    match target {
        Target::Pid(pid) => {
            cmd.arg("-process_id").arg(pid.to_string());
        }
        Target::Name(name) => {
            cmd.arg("-process_name").arg(name);
        }
    }
    cmd.arg("-output_file")
        .arg(output)
        .arg("-no_top")
        .arg("-stop_existing_session")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd
        .spawn()
        .map_err(|e| Error(format!("cannot start {}: {e}", presentmon.display())))?;
    Ok(FrameTrace {
        child: Some(child),
        output: output.to_path_buf(),
    })
}

impl FrameTrace {
    /// Wait up to `wait` for the session to be recording: the PresentMon process is still alive
    /// and its CSV has started. Fails when it exits early or writes nothing in time.
    pub fn confirm_running(&mut self, wait: Duration) -> Result<()> {
        let deadline = Instant::now() + wait;
        loop {
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| Error("frame trace already stopped".into()))?;
            if let Some(status) = child.try_wait()? {
                return err(format!("PresentMon exited early ({status})"));
            }
            if std::fs::metadata(&self.output)
                .map(|m| m.len())
                .unwrap_or(0)
                > 0
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return err(format!(
                    "PresentMon wrote nothing to {} within {}ms",
                    self.output.display(),
                    wait.as_millis()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop the session and summarise its CSV.
    pub fn stop(mut self) -> Result<FrameStats> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let text = std::fs::read_to_string(&self.output)?;
        parse_stats(&text)
    }
}

impl Drop for FrameTrace {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Nearest-rank percentile of an ascending slice (`0.0` when empty).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Frame statistics from PresentMon CSV text. Rows whose `MsBetweenPresents` does not parse are
/// skipped. Fails when the header has no `MsBetweenPresents` column.
pub fn parse_stats(csv: &str) -> Result<FrameStats> {
    let mut lines = csv.lines();
    let header: Vec<&str> = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .collect();
    let ms_col = header
        .iter()
        .position(|h| *h == "MsBetweenPresents")
        .ok_or_else(|| Error("no MsBetweenPresents column in the PresentMon CSV".into()))?;
    let drop_col = header.iter().position(|h| *h == "Dropped");
    let mut ms: Vec<f64> = Vec::new();
    let mut drops = 0u64;
    for line in lines {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(v) = cols.get(ms_col).and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        ms.push(v);
        if drop_col
            .and_then(|c| cols.get(c))
            .is_some_and(|s| *s == "1" || s.eq_ignore_ascii_case("true"))
        {
            drops += 1;
        }
    }
    ms.sort_by(|a, b| a.total_cmp(b));
    Ok(FrameStats {
        present_count: ms.len() as u64,
        p50_ms: percentile(&ms, 50.0),
        p95_ms: percentile(&ms, 95.0),
        p99_ms: percentile(&ms, 99.0),
        drops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_from_presentmon_csv() {
        let csv = "Application,MsBetweenPresents,Dropped\napp,16.0,0\napp,17.0,1\napp,33.0,0\n";
        let s = parse_stats(csv).unwrap();
        assert_eq!(s.present_count, 3);
        assert_eq!(s.drops, 1);
        assert_eq!(s.p50_ms, 17.0);
        assert_eq!(s.p95_ms, 33.0);
        assert_eq!(s.p99_ms, 33.0);
    }

    #[test]
    fn rows_without_a_number_are_skipped_and_missing_column_is_an_error() {
        let s = parse_stats("App,MsBetweenPresents\na,10\na,oops\n").unwrap();
        assert_eq!(s.present_count, 1);
        assert_eq!(s.drops, 0);
        assert!(parse_stats("App,Other\na,1\n").is_err());
    }

    #[test]
    fn empty_trace_has_zero_stats() {
        let s = parse_stats("App,MsBetweenPresents\n").unwrap();
        assert_eq!((s.present_count, s.p50_ms, s.drops), (0, 0.0, 0));
    }
}
