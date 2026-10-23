//! Deadline-bounded execution of UI Automation work. UIA calls have no cancellation: a target
//! whose UIA server is hung blocks the calling thread indefinitely. These helpers run the call
//! on a COM-initialised worker thread and give up when the deadline passes, so the caller always
//! gets an answer in time. A timed-out call may stay stuck on its worker; the lane stays occupied
//! until it returns, so a hung target never accumulates threads. A caller that arrives while the
//! lane is occupied is refused at once (`None`), never queued behind the stuck call.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

/// One persistent MTA worker thread that refuses new work while it is busy.
pub(crate) struct Lane {
    tx: Option<SyncSender<Job>>,
    /// True from the moment a call is accepted until its job finishes, is skipped, panics or is
    /// dropped. A caller that finds it set returns `None` at once.
    busy: Arc<AtomicBool>,
}

/// Clears the lane's busy flag on drop, so every exit path of a job (return, stale skip, panic
/// unwinding, job dropped unrun) releases the lane.
struct BusySlot(Arc<AtomicBool>);
impl Drop for BusySlot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Lane {
    pub(crate) fn spawn(name: &str) -> Lane {
        let (tx, rx) = sync_channel::<Job>(1);
        let started = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                super::uia::ensure_com();
                while let Ok(job) = rx.recv() {
                    // A panicking probe must not kill the lane; its caller sees a dropped reply.
                    let _ = catch_unwind(AssertUnwindSafe(job));
                }
            })
            .is_ok();
        Lane {
            tx: started.then_some(tx),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run `f` on the lane. `None` when `timeout` is zero, the lane is busy or dead, `f` did not
    /// start before the deadline, or it did not finish by then.
    pub(crate) fn run<T, F>(&self, timeout: Duration, f: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(Instant) -> T + Send + 'static,
    {
        if timeout.is_zero() {
            return None;
        }
        let tx = self.tx.as_ref()?;
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let slot = BusySlot(Arc::clone(&self.busy));
        let deadline = Instant::now() + timeout;
        let (reply, receive) = sync_channel::<Option<T>>(1);
        let job: Job = Box::new(move || {
            // Defensive: a job that somehow started after its deadline is stale; skip it.
            let value = (Instant::now() < deadline).then(|| f(deadline));
            // Free the lane before replying, so a caller that calls again right after its reply
            // never sees a false busy. A panic in `f` drops `slot` while unwinding.
            drop(slot);
            let _ = reply.send(value);
        });
        // A dead lane drops the job, and with it the slot, which clears the flag.
        tx.try_send(job).ok()?;
        match receive.recv_timeout(timeout) {
            Ok(v) => v,
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }
}

fn read_lane() -> &'static Lane {
    static LANE: OnceLock<Lane> = OnceLock::new();
    LANE.get_or_init(|| Lane::spawn("rightkit-uia-mta"))
}

/// Run read-only UIA work on RightKit's shared COM-initialised worker thread and return its
/// result, or `None` when `timeout` passes first (or the worker is busy with an earlier stuck
/// call, which also returns `None` immediately: a caller arriving during a stall is refused at
/// once, never queued).
///
/// `f` receives the absolute deadline so a long walk can stop early. COM is initialised
/// (multithreaded apartment) on the worker by RightKit; the caller's thread is never touched.
/// [`El`](super::uia::El) is `Send`, so elements may be captured by `f` and returned.
///
/// Use [`with_deadline_mutation`] for work that changes the target.
pub fn with_deadline<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Instant) -> T + Send + 'static,
{
    read_lane().run(timeout, f)
}

fn security_lane() -> &'static Lane {
    static LANE: OnceLock<Lane> = OnceLock::new();
    LANE.get_or_init(|| Lane::spawn("rightkit-uia-security"))
}

/// [`with_deadline`] on a second, separate worker, for security checks (is the focused field a
/// password?). A hung content probe on the shared worker must not starve the security check,
/// so the two never queue behind each other.
pub fn with_deadline_security<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Instant) -> T + Send + 'static,
{
    security_lane().run(timeout, f)
}

static MUTATION_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static MUTATION_CIRCUIT_OPEN: AtomicBool = AtomicBool::new(false);

struct MutationSlot;
impl Drop for MutationSlot {
    fn drop(&mut self) {
        MUTATION_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Like [`with_deadline`] for work that mutates the target (for example a `SetValue`). It runs on
/// a fresh thread, one at a time, and a timeout opens a circuit: a detached mutation may still
/// land later, so no further mutation runs through this function until
/// [`reset_mutation_circuit`] is called. Returns `None` for a zero timeout, a busy slot, an open
/// circuit, a thread that could not start, or a timeout.
pub fn with_deadline_mutation<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Instant) -> T + Send + 'static,
{
    if timeout.is_zero() || MUTATION_CIRCUIT_OPEN.load(Ordering::Acquire) {
        return None;
    }
    if MUTATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let slot = MutationSlot;
    let deadline = Instant::now() + timeout;
    let (reply, receive) = sync_channel::<T>(1);
    let spawned = std::thread::Builder::new()
        .name("rightkit-uia-mutation".into())
        .spawn(move || {
            let _slot = slot;
            super::uia::ensure_com();
            if Instant::now() >= deadline {
                return;
            }
            if let Ok(v) = catch_unwind(AssertUnwindSafe(|| f(deadline))) {
                let _ = reply.send(v);
            }
        });
    if spawned.is_err() {
        // `slot` moved into the closure that never ran and was dropped with it.
        return None;
    }
    match receive.recv_timeout(timeout) {
        Ok(v) => Some(v),
        Err(RecvTimeoutError::Timeout) => {
            MUTATION_CIRCUIT_OPEN.store(true, Ordering::Release);
            None
        }
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

/// True after a [`with_deadline_mutation`] call timed out.
pub fn mutation_circuit_open() -> bool {
    MUTATION_CIRCUIT_OPEN.load(Ordering::Acquire)
}

/// Close the mutation circuit again. Only do this once you know the timed-out mutation is no
/// longer running or no longer matters.
pub fn reset_mutation_circuit() {
    MUTATION_CIRCUIT_OPEN.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_returns_value_within_deadline() {
        let lane = Lane::spawn("test-lane-ok");
        assert_eq!(lane.run(Duration::from_secs(2), |_| 7), Some(7));
    }

    #[test]
    fn lane_zero_timeout_is_none() {
        let lane = Lane::spawn("test-lane-zero");
        assert_eq!(lane.run(Duration::ZERO, |_| 7), None);
    }

    #[test]
    fn lane_times_out_and_stays_busy_until_the_call_returns() {
        let lane = Lane::spawn("test-lane-slow");
        let r = lane.run(Duration::from_millis(50), |_| {
            std::thread::sleep(Duration::from_millis(400));
            1
        });
        assert_eq!(r, None);
        // The worker is still inside the slow call: the next call is refused at once, not queued.
        let started = Instant::now();
        assert_eq!(lane.run(Duration::from_secs(5), |_| 2), None);
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(lane.run(Duration::from_secs(2), |_| 3), Some(3));
    }

    /// Retry until the lane has released a finished job (the worker clears the flag just before
    /// it replies, but a timed-out caller may return earlier than the job ends).
    fn recover(lane: &Lane, value: i32) {
        for _ in 0..1000 {
            if lane.run(Duration::from_secs(2), move |_| value) == Some(value) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("lane never recovered");
    }

    #[test]
    fn lane_refuses_immediately_while_busy() {
        use std::sync::mpsc::channel;
        let lane = Lane::spawn("test-lane-busy");
        let (entered_tx, entered_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let a = lane.run(Duration::from_millis(50), move |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
            1
        });
        assert_eq!(a, None);
        entered_rx.recv().expect("job A entered");
        let ran = Arc::new(AtomicBool::new(false));
        let ran_b = Arc::clone(&ran);
        let started = Instant::now();
        let b = lane.run(Duration::from_secs(5), move |_| {
            ran_b.store(true, Ordering::SeqCst);
            2
        });
        assert_eq!(b, None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!ran.load(Ordering::SeqCst), "refused job must never run");
        release_tx.send(()).expect("release A");
        recover(&lane, 3);
    }

    #[test]
    fn lane_back_to_back_calls_never_see_false_busy() {
        let lane = Lane::spawn("test-lane-b2b");
        for i in 0..1000 {
            assert_eq!(lane.run(Duration::from_secs(2), move |_| i), Some(i));
        }
    }

    #[test]
    fn lane_survives_a_panicking_job() {
        let lane = Lane::spawn("test-lane-panic");
        let r: Option<i32> = lane.run(Duration::from_secs(2), |_| panic!("probe panic"));
        assert_eq!(r, None);
        recover(&lane, 5);
    }

    #[test]
    fn security_lane_answers_while_the_read_lane_is_held() {
        use std::sync::mpsc::channel;
        let (entered_tx, entered_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let held = with_deadline(Duration::from_millis(50), move |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
            1
        });
        assert_eq!(held, None);
        entered_rx.recv().expect("read lane held");
        assert_eq!(with_deadline(Duration::from_secs(5), |_| 2), None);
        assert_eq!(
            with_deadline_security(Duration::from_secs(2), |_| 9),
            Some(9)
        );
        release_tx.send(()).expect("release");
    }
}
