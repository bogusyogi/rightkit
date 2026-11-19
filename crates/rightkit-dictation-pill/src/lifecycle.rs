//! Startup generations, the cancellation handshake and the lifecycle phase machine.
//!
//! Pure logic: no Win32, no threads, no config. The Windows driver owns the threads and windows
//! and asks this module what is allowed. Statics: `NEXT_GENERATION` (the process-wide generation
//! counter below).
//!
//! The problem it closes: the driver starts an owner thread that creates the overlay windows and
//! the caller waits for it with a timeout. If the caller gives up (timeout, shutdown during
//! startup) the owner must not create or keep any window. Each start attempt therefore gets a
//! [`Startup`] (one generation, one cancellation flag, never reused). The owner observes it before
//! it creates a window and after each window it creates, and it must win a single
//! compare-and-swap ([`Startup::mark_ready`]) before it may publish the windows. The caller's
//! give-up is the opposite compare-and-swap ([`Startup::cancel`]). Exactly one of the two wins, so
//! either the caller owns the windows (the owner won) or the owner destroys them (the caller won).
//! A later start allocates a new `Startup`, so an old owner can never be un-cancelled.

use std::any::Any;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A process-wide, never repeating generation number (`>= 1`; `0` means "none").
pub(crate) fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::SeqCst)
}

const PENDING: u8 = 0;
const READY: u8 = 1;
const CANCELLED: u8 = 2;

/// One start attempt: its generation and its cancellation state.
#[derive(Debug)]
pub(crate) struct Startup {
    generation: u64,
    state: AtomicU8,
}

/// What [`Startup::cancel`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    /// The attempt was pending and is now cancelled: the owner will tear down what it made.
    Cancelled,
    /// The owner already published its windows: the caller owns them now.
    AlreadyReady,
    /// Someone cancelled before.
    AlreadyCancelled,
}

impl Startup {
    pub(crate) fn new(generation: u64) -> Startup {
        Startup {
            generation,
            state: AtomicU8::new(PENDING),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The owner polls this before creating a window and after each window it created.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) == CANCELLED
    }

    /// The owner, with every window built: publish them. `false` means the caller cancelled
    /// first and the owner must destroy what it made and exit.
    pub(crate) fn mark_ready(&self) -> bool {
        self.state
            .compare_exchange(PENDING, READY, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// The caller, giving up (timeout or shutdown during startup).
    pub(crate) fn cancel(&self) -> CancelOutcome {
        match self
            .state
            .compare_exchange(PENDING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => CancelOutcome::Cancelled,
            Err(READY) => CancelOutcome::AlreadyReady,
            Err(_) => CancelOutcome::AlreadyCancelled,
        }
    }
}

/// Where the driver is in its life. `T` is the live controller (cloneable handle).
#[derive(Debug)]
pub(crate) enum Phase<T> {
    /// Nothing started yet. Only an explicit `ensure_installed` starts it.
    NotStarted,
    /// An owner thread is creating the windows; one caller waits for it.
    Starting(Arc<Startup>),
    /// `shutdown` ran while starting: the attempt is cancelled and the starter is tearing down.
    Cancelling(Arc<Startup>),
    Running {
        generation: u64,
        controller: T,
    },
    /// Startup failed, timed out or the driver faulted. Only an explicit `ensure_installed`
    /// starts a fresh generation; implicit calls do nothing (no crash loop).
    Failed,
    /// `shutdown` ran. Only an explicit `ensure_installed` starts a fresh generation.
    ShutDown,
}

/// What a caller must do next ([`Lifecycle::begin`]).
#[derive(Debug)]
pub(crate) enum Begin<T> {
    Run(T),
    /// Another thread is starting and this call is explicit: wait for it (bounded).
    Wait,
    /// This explicit caller starts the attempt.
    Start(Arc<Startup>),
    /// Nothing to do: an implicit call never starts a generation and never waits for one.
    Refuse,
}

/// How a start attempt ended, as reported by the starter ([`Lifecycle::finish`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    Installed,
    Failed,
    /// `shutdown` (or a newer start) overtook the attempt: the starter destroys what it made.
    Superseded,
}

/// What `shutdown` found and the caller must now tear down.
#[derive(Debug)]
pub(crate) enum ShutdownTake<T> {
    Nothing,
    /// A running controller: destroy it.
    Running(T),
    /// A start in progress was cancelled; the starter tears down.
    CancelledStartup(Arc<Startup>),
}

#[derive(Debug)]
pub(crate) struct Lifecycle<T> {
    phase: Phase<T>,
}

impl<T: Clone> Lifecycle<T> {
    pub(crate) const fn new() -> Lifecycle<T> {
        Lifecycle {
            phase: Phase::NotStarted,
        }
    }

    pub(crate) fn phase(&self) -> &Phase<T> {
        &self.phase
    }

    /// The running controller, if any.
    pub(crate) fn running(&self) -> Option<T> {
        match &self.phase {
            Phase::Running { controller, .. } => Some(controller.clone()),
            _ => None,
        }
    }

    /// `explicit` is `ensure_installed`; implicit is any other call that merely needs a driver. Only
    /// an explicit call starts a generation or waits for one, so an event sent from a UI thread
    /// never blocks on a start and never starts one.
    pub(crate) fn begin(&mut self, explicit: bool) -> Begin<T> {
        match &self.phase {
            Phase::Running { controller, .. } => Begin::Run(controller.clone()),
            Phase::Starting(_) | Phase::Cancelling(_) => {
                if explicit {
                    Begin::Wait
                } else {
                    Begin::Refuse
                }
            }
            Phase::NotStarted | Phase::Failed | Phase::ShutDown => {
                if explicit {
                    self.start()
                } else {
                    Begin::Refuse
                }
            }
        }
    }

    fn start(&mut self) -> Begin<T> {
        let startup = Arc::new(Startup::new(next_generation()));
        self.phase = Phase::Starting(Arc::clone(&startup));
        Begin::Start(startup)
    }

    /// The starter reports. `result` is the controller on success.
    pub(crate) fn finish(&mut self, startup: &Startup, result: Option<T>) -> Finish {
        let generation = startup.generation();
        let (starting, cancelling) = match &self.phase {
            Phase::Starting(current) => (current.generation() == generation, false),
            Phase::Cancelling(current) => (false, current.generation() == generation),
            _ => (false, false),
        };
        if starting {
            return match result {
                Some(controller) => {
                    self.phase = Phase::Running {
                        generation,
                        controller,
                    };
                    Finish::Installed
                }
                None => {
                    self.phase = Phase::Failed;
                    Finish::Failed
                }
            };
        }
        if cancelling {
            self.phase = Phase::ShutDown;
        }
        Finish::Superseded
    }

    /// `shutdown`: leave `ShutDown` (or `Cancelling` while a start is still unwinding).
    pub(crate) fn shutdown(&mut self) -> ShutdownTake<T> {
        match std::mem::replace(&mut self.phase, Phase::ShutDown) {
            Phase::Running { controller, .. } => ShutdownTake::Running(controller),
            Phase::Starting(startup) => {
                startup.cancel();
                self.phase = Phase::Cancelling(Arc::clone(&startup));
                ShutdownTake::CancelledStartup(startup)
            }
            Phase::Cancelling(startup) => {
                self.phase = Phase::Cancelling(startup);
                ShutdownTake::Nothing
            }
            Phase::NotStarted | Phase::Failed | Phase::ShutDown => ShutdownTake::Nothing,
        }
    }

    /// The driver of `generation` ended on its own (a panic, a painter fault, an overlay window
    /// that ended by itself): `Failed`, unless that generation is no longer the running one.
    ///
    /// An owner can die after it published its windows (`Startup::mark_ready`) but before its
    /// starter called [`Lifecycle::finish`]: the phase is still `Starting`, so this returns `false`
    /// and changes nothing. The driver closes that window itself: the owner records its death
    /// before calling this, and the starter checks the record right after `finish` installed the
    /// controller and calls this again (`driver::start`).
    pub(crate) fn mark_dead(&mut self, generation: u64) -> bool {
        match &self.phase {
            Phase::Running {
                generation: current,
                ..
            } if *current == generation => {
                self.phase = Phase::Failed;
                true
            }
            _ => false,
        }
    }
}

/// The type of a panic payload, for the diagnostic event: `str`, `string` or `other`. Never the
/// payload itself, which can hold anything a host callback formatted into it (a transcript, a
/// path, a label).
pub(crate) fn panic_type(payload: &(dyn Any + Send)) -> &'static str {
    if payload.is::<&str>() {
        "str"
    } else if payload.is::<String>() {
        "string"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Life = Lifecycle<u32>;

    fn start(life: &mut Life, explicit: bool) -> Arc<Startup> {
        match life.begin(explicit) {
            Begin::Start(startup) => startup,
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn generations_never_repeat() {
        let a = next_generation();
        let b = next_generation();
        assert!(a >= 1);
        assert!(b > a);
    }

    #[test]
    fn handshake_owner_wins_when_it_publishes_first() {
        let startup = Startup::new(1);
        assert!(!startup.is_cancelled());
        assert!(startup.mark_ready());
        // The caller timing out afterwards finds the windows already published.
        assert_eq!(startup.cancel(), CancelOutcome::AlreadyReady);
        assert!(!startup.is_cancelled());
    }

    #[test]
    fn handshake_caller_wins_when_it_gives_up_first() {
        let startup = Startup::new(1);
        assert_eq!(startup.cancel(), CancelOutcome::Cancelled);
        assert!(startup.is_cancelled());
        // The owner arriving late may not publish: it must destroy what it made.
        assert!(!startup.mark_ready());
        assert_eq!(startup.cancel(), CancelOutcome::AlreadyCancelled);
    }

    #[test]
    fn exactly_one_side_wins_the_race() {
        for _ in 0..200 {
            let startup = Arc::new(Startup::new(1));
            let owner = {
                let startup = Arc::clone(&startup);
                std::thread::spawn(move || startup.mark_ready())
            };
            let caller = startup.cancel();
            let owner_won = owner.join().unwrap();
            match caller {
                CancelOutcome::Cancelled => assert!(!owner_won),
                CancelOutcome::AlreadyReady => assert!(owner_won),
                CancelOutcome::AlreadyCancelled => panic!("nobody else cancels"),
            }
        }
    }

    #[test]
    fn an_explicit_call_starts_and_a_second_explicit_call_waits_then_runs() {
        let mut life = Life::new();
        assert!(matches!(life.begin(false), Begin::Refuse));
        let startup = start(&mut life, true);
        assert!(matches!(life.begin(false), Begin::Refuse));
        assert!(matches!(life.begin(true), Begin::Wait));
        assert_eq!(life.finish(&startup, Some(7)), Finish::Installed);
        assert!(matches!(life.begin(false), Begin::Run(7)));
        assert_eq!(life.running(), Some(7));
    }

    #[test]
    fn failed_start_is_retried_only_by_an_explicit_call_with_a_fresh_generation() {
        let mut life = Life::new();
        let first = start(&mut life, true);
        assert_eq!(life.finish(&first, None), Finish::Failed);
        assert!(matches!(life.begin(false), Begin::Refuse));
        let second = start(&mut life, true);
        assert!(second.generation() > first.generation());
        // The old attempt can never be revived.
        first.cancel();
        assert!(!second.is_cancelled());
    }

    #[test]
    fn timeout_cancel_then_late_result_is_superseded() {
        let mut life = Life::new();
        let startup = start(&mut life, true);
        // The starter times out: cancel, report failure.
        assert_eq!(startup.cancel(), CancelOutcome::Cancelled);
        assert_eq!(life.finish(&startup, None), Finish::Failed);
        // A later explicit install starts a new generation; the old owner stays cancelled.
        let next = start(&mut life, true);
        assert_ne!(next.generation(), startup.generation());
        assert!(startup.is_cancelled());
        assert!(!startup.mark_ready());
        assert!(!next.is_cancelled());
    }

    #[test]
    fn shutdown_during_startup_cancels_and_blocks_implicit_restart() {
        let mut life = Life::new();
        let startup = start(&mut life, true);
        match life.shutdown() {
            ShutdownTake::CancelledStartup(cancelled) => {
                assert_eq!(cancelled.generation(), startup.generation())
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(startup.is_cancelled());
        assert!(!startup.mark_ready());
        // While the starter unwinds: implicit calls refuse, an explicit one waits.
        assert!(matches!(life.begin(false), Begin::Refuse));
        assert!(matches!(life.begin(true), Begin::Wait));
        // The starter finishes: whatever it built is torn down, and the phase is ShutDown.
        assert_eq!(life.finish(&startup, Some(1)), Finish::Superseded);
        assert!(matches!(life.begin(false), Begin::Refuse));
        let next = start(&mut life, true);
        assert!(next.generation() > startup.generation());
    }

    #[test]
    fn shutdown_of_a_running_controller_hands_it_back_once() {
        let mut life = Life::new();
        let startup = start(&mut life, true);
        life.finish(&startup, Some(9));
        match life.shutdown() {
            ShutdownTake::Running(9) => {}
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(life.shutdown(), ShutdownTake::Nothing));
        assert!(matches!(life.begin(false), Begin::Refuse));
        assert_eq!(life.running(), None);
    }

    #[test]
    fn a_dead_driver_marks_only_its_own_generation() {
        let mut life = Life::new();
        let startup = start(&mut life, true);
        life.finish(&startup, Some(3));
        assert!(!life.mark_dead(startup.generation() + 1000));
        assert!(matches!(life.begin(false), Begin::Run(3)));
        assert!(life.mark_dead(startup.generation()));
        assert!(matches!(life.begin(false), Begin::Refuse));
        let again = start(&mut life, true);
        assert!(again.generation() > startup.generation());
    }

    #[test]
    fn a_stale_finish_cannot_install_over_a_newer_generation() {
        let mut life = Life::new();
        let first = start(&mut life, true);
        life.finish(&first, None);
        let second = start(&mut life, true);
        // The first starter reports late: it is superseded and installs nothing.
        assert_eq!(life.finish(&first, Some(1)), Finish::Superseded);
        assert!(matches!(life.begin(true), Begin::Wait));
        assert_eq!(life.finish(&second, Some(2)), Finish::Installed);
        assert_eq!(life.running(), Some(2));
    }

    #[test]
    fn panic_payloads_are_reported_by_type_never_by_content() {
        let text: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_type(text.as_ref()), "str");
        let owned: Box<dyn Any + Send> = Box::new(String::from("bang"));
        assert_eq!(panic_type(owned.as_ref()), "string");
        let other: Box<dyn Any + Send> = Box::new(5u8);
        assert_eq!(panic_type(other.as_ref()), "other");
    }
}
