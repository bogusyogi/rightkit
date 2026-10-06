//! Continuous foreground measurement for the no-focus-steal guarantee.
//!
//! A before/after comparison misses a transient steal and, worse, passes when the
//! measurement is unavailable (`None == None`). [`FocusWatch`] samples the foreground
//! (pid + top-level window) on its own thread for the whole span of an action and
//! reports every transition. A watch that took no successful sample is **not**
//! proven: [`FocusReport::proven`] is false, and callers must treat that as failure.

use crate::model::FrontApp;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct FocusReport {
    /// Samples that returned a foreground app.
    pub measured: usize,
    /// Samples where the platform could not say (no foreground, query failure).
    pub unavailable: usize,
    /// Distinct foreground states seen, in order (consecutive duplicates collapsed).
    pub states: Vec<(i32, Option<u64>)>,
}

impl FocusReport {
    /// True only when focus was actually measured and never left the starting
    /// (pid, window): at least one good sample, no unavailable sample after the
    /// first, and a single state throughout.
    pub fn proven(&self) -> bool {
        self.measured > 0 && self.unavailable == 0 && self.states.len() == 1
    }
    /// Why `proven` is false, for assertion messages.
    pub fn explain(&self) -> String {
        if self.measured == 0 {
            "foreground could not be measured (an unavailable measurement is never a pass)".into()
        } else if self.unavailable > 0 {
            format!("foreground was unmeasurable for {} sample(s) while acting", self.unavailable)
        } else {
            format!("foreground changed: {:?}", self.states)
        }
    }
}

pub struct FocusWatch {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<FocusReport>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FocusWatch {
    /// Start sampling `front` every `interval`. One sample is taken synchronously
    /// first, so the starting state is always recorded before the action begins.
    pub fn start(front: impl Fn() -> Option<FrontApp> + Send + 'static, interval: Duration) -> FocusWatch {
        let seen = Arc::new(Mutex::new(FocusReport { measured: 0, unavailable: 0, states: vec![] }));
        let record = |seen: &Mutex<FocusReport>, f: Option<FrontApp>| {
            let mut r = seen.lock().unwrap_or_else(|e| e.into_inner());
            match f {
                Some(f) => {
                    r.measured += 1;
                    let st = (f.pid, f.window_id);
                    if r.states.last() != Some(&st) {
                        r.states.push(st);
                    }
                }
                None => r.unavailable += 1,
            }
        };
        record(&seen, front());
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, seen2) = (stop.clone(), seen.clone());
        let thread = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                std::thread::sleep(interval);
                record(&seen2, front());
            }
        });
        FocusWatch { stop, seen, thread: Some(thread) }
    }

    /// Stop sampling and return what was observed.
    pub fn finish(mut self) -> FocusReport {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn app(pid: i32) -> Option<FrontApp> {
        Some(FrontApp { pid, app_id: "x".into(), window_id: Some(1) })
    }

    #[test]
    fn unavailable_measurement_never_proves_and_transients_are_caught() {
        let none = FocusWatch::start(|| None, Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(10));
        let r = none.finish();
        assert!(!r.proven(), "None == None must not pass: {}", r.explain());

        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        // pid 1, then a one-sample flash of pid 2, then pid 1 again: before/after would say "unchanged".
        let w = FocusWatch::start(move || app(if n2.fetch_add(1, Ordering::SeqCst) == 3 { 2 } else { 1 }), Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(30));
        let r = w.finish();
        assert!(!r.proven());
        assert_eq!(r.states.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2, 1]);

        let steady = FocusWatch::start(|| app(1), Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(10));
        assert!(steady.finish().proven());
    }
}
