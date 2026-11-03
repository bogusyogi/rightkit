//! Interface-change monitor.
//!
//! Surface (item -> iroh call site): `Monitor::new` and `Monitor::interface_state` and `Monitor::network_change` and `State`
//! (socket.rs `Actor`: builds the monitor at bind, watches `local_interfaces_watcher`, and forwards
//! `ActorMessage::NetworkChange` to `network_change`).
//!
//! iOS gives an app no routing socket, so changes are found by re-reading `getifaddrs` every [`POLL`] and on demand
//! (`network_change`, which the app calls from its `NWPathMonitor` handler through `Endpoint::network_change`). A gap in
//! wall-clock ticks much longer than [`POLL`] means the process was suspended; that is published as `last_unsuspend` so iroh
//! re-checks its sockets on resume.

use std::fmt;

use n0_future::{
    task::{self, AbortOnDropHandle},
    time::{self, Duration, Instant},
};
use n0_watcher::Watchable;
use tokio::sync::mpsc;
use tracing::{debug, trace};

pub use crate::interfaces::State;

/// How often the interface list is re-read.
const POLL: Duration = Duration::from_secs(3);
/// Wait after a trigger so a burst of changes is read once.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Monitor error: the actor task is gone.
#[derive(Debug)]
pub struct Error;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("network monitor channel closed")
    }
}
impl std::error::Error for Error {}

/// Watches interface state.
#[derive(Debug)]
pub struct Monitor {
    _handle: AbortOnDropHandle<()>,
    trigger: mpsc::Sender<()>,
    state: Watchable<State>,
}

impl Monitor {
    /// Starts the monitor with the current state.
    pub async fn new() -> Result<Self, Error> {
        let state = Watchable::new(State::read());
        let (trigger, rx) = mpsc::channel(8);
        let handle = task::spawn(run(state.clone(), rx));
        Ok(Monitor {
            _handle: AbortOnDropHandle::new(handle),
            trigger,
            state,
        })
    }

    /// Watch for state changes.
    pub fn interface_state(&self) -> n0_watcher::Direct<State> {
        self.state.watch()
    }

    /// A change was detected outside (the app's path monitor): re-read soon.
    pub async fn network_change(&self) -> Result<(), Error> {
        self.trigger.send(()).await.map_err(|_| Error)
    }
}

async fn run(state: Watchable<State>, mut rx: mpsc::Receiver<()>) {
    let mut tick = time::interval(POLL);
    tick.tick().await; // the first tick is immediate; the state was just read
    let mut last_tick = Instant::now();
    loop {
        let mut jumped = false;
        tokio::select! {
            _ = tick.tick() => {
                let now = Instant::now();
                jumped = now.duration_since(last_tick) > POLL * 3 / 2 + DEBOUNCE;
                last_tick = now;
            }
            msg = rx.recv() => {
                if msg.is_none() {
                    debug!("network monitor trigger closed, stopping");
                    return;
                }
                time::sleep(DEBOUNCE).await;
                while rx.try_recv().is_ok() {}
            }
        }
        let mut new = State::read();
        let old = state.get();
        if jumped {
            new.last_unsuspend = Some(Instant::now());
        } else {
            new.last_unsuspend = old.last_unsuspend;
            if new == old {
                trace!("no network change");
                continue;
            }
        }
        debug!(jumped, "network state changed");
        state.set(new).ok();
    }
}
