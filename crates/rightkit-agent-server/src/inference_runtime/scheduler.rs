use super::LoadSnapshot;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

/// Default maximum number of simultaneously active cloud inference streams.
pub const DEFAULT_ACTIVE_STREAM_LIMIT: u32 = 4;
/// Each active stream has this many event slots. It is deliberately bounded.
pub const EVENT_CHANNEL_CAPACITY: usize = 128;

#[derive(Clone)]
pub struct InferenceScheduler {
    shared: Arc<Semaphore>,
    interactive_reserved: Arc<Semaphore>,
    background_slots: Arc<Semaphore>,
    reserved_slots: usize,
    limit: usize,
    per_connection_limit: usize,
    queued: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    buffered_events: Arc<AtomicUsize>,
    per_connection: Arc<Mutex<BTreeMap<String, usize>>>,
    connection_slots: Arc<Mutex<BTreeMap<String, Arc<Semaphore>>>>,
    per_route: Arc<Mutex<BTreeMap<String, usize>>>,
}

/// Admission class for one inference request. Background classes share the
/// bounded pool; interactive work can also consume a reserved slot that
/// background fan-out can never occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceLane {
    Interactive,
    Conductor,
    Background,
}

/// Cancellation authority for one queued or active inference job.
///
/// A job uses one Tokio watch pair: queued acquisition observes its receiver,
/// while cancellation releases an already active lease immediately.
#[derive(Clone)]
pub struct SchedulerJob {
    cancellation: watch::Sender<bool>,
    active: Arc<Mutex<Option<Arc<ActiveLease>>>>,
}

pub struct SchedulerPermit {
    lease: Arc<ActiveLease>,
    job: SchedulerJob,
    cancellation: watch::Receiver<bool>,
}

pub struct SchedulerEventSender<T> {
    sender: mpsc::Sender<T>,
    buffered_events: Arc<AtomicUsize>,
}

pub struct SchedulerEventReceiver<T> {
    receiver: mpsc::Receiver<T>,
    buffered_events: Arc<AtomicUsize>,
}

struct ActiveLease {
    lane_permit: Mutex<Option<OwnedSemaphorePermit>>,
    connection_permit: Mutex<Option<OwnedSemaphorePermit>>,
    background_permit: Mutex<Option<OwnedSemaphorePermit>>,
    scheduler: InferenceScheduler,
    connection: String,
    route: Option<String>,
    released: AtomicBool,
}

struct QueuedGuard(Arc<AtomicUsize>);

impl QueuedGuard {
    fn new(queued: Arc<AtomicUsize>) -> Self {
        queued.fetch_add(1, Ordering::SeqCst);
        Self(queued)
    }
}

impl Drop for QueuedGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for InferenceScheduler {
    fn default() -> Self {
        Self::new(DEFAULT_ACTIVE_STREAM_LIMIT)
    }
}

impl InferenceScheduler {
    pub fn new(limit: u32) -> Self {
        let limit = limit.clamp(1, 32) as usize;
        let reserved_slots = usize::from(limit > 1);
        Self {
            shared: Arc::new(Semaphore::new(limit - reserved_slots)),
            interactive_reserved: Arc::new(Semaphore::new(reserved_slots)),
            background_slots: Arc::new(Semaphore::new(((limit - reserved_slots) / 3).max(1))),
            reserved_slots,
            limit,
            per_connection_limit: (limit / 2).max(1),
            queued: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            buffered_events: Arc::new(AtomicUsize::new(0)),
            per_connection: Arc::new(Mutex::new(BTreeMap::new())),
            connection_slots: Arc::new(Mutex::new(BTreeMap::new())),
            per_route: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn new_job(&self) -> SchedulerJob {
        let (cancellation, _) = watch::channel(false);
        SchedulerJob {
            cancellation,
            active: Arc::new(Mutex::new(None)),
        }
    }

    /// Acquires a FIFO global slot for a non-cancellable caller.
    pub async fn acquire(
        &self,
        connection: &str,
        route: Option<&str>,
    ) -> Result<SchedulerPermit, &'static str> {
        self.acquire_job(&self.new_job(), connection, route).await
    }

    /// Acquires a FIFO global slot for `job`, unless its watch cancellation was
    /// requested while it waited. Tokio's fair semaphore preserves arrival order.
    pub async fn acquire_job(
        &self,
        job: &SchedulerJob,
        connection: &str,
        route: Option<&str>,
    ) -> Result<SchedulerPermit, &'static str> {
        self.acquire_lane_job(job, InferenceLane::Interactive, connection, route)
            .await
    }

    /// Acquire a non-interactive slot. Conductor/background fan-out cannot
    /// consume the interactive reserve, which keeps operator turns responsive
    /// even when all shared provider capacity is queued.
    pub async fn acquire_lane(
        &self,
        lane: InferenceLane,
        connection: &str,
        route: Option<&str>,
    ) -> Result<SchedulerPermit, &'static str> {
        self.acquire_lane_job(&self.new_job(), lane, connection, route)
            .await
    }

    pub async fn acquire_lane_job(
        &self,
        job: &SchedulerJob,
        lane: InferenceLane,
        connection: &str,
        route: Option<&str>,
    ) -> Result<SchedulerPermit, &'static str> {
        let mut cancellation = job.cancellation.subscribe();
        if *cancellation.borrow() {
            return Err("scheduler_cancelled");
        }
        let queued = QueuedGuard::new(self.queued.clone());
        let connection_slots = {
            let mut slots = self
                .connection_slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slots
                .entry(connection.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(self.per_connection_limit)))
                .clone()
        };
        let connection_permit = tokio::select! {
            permit = connection_slots.acquire_owned() => permit.map_err(|_| "scheduler_closed")?,
            changed = cancellation.changed() => {
                match changed {
                    Ok(()) if *cancellation.borrow() => return Err("scheduler_cancelled"),
                    Ok(()) => return Err("scheduler_cancelled"),
                    Err(_) => return Err("scheduler_cancelled"),
                }
            }
        };
        let background_permit = if lane == InferenceLane::Background {
            Some(tokio::select! {
                permit = self.background_slots.clone().acquire_owned() => permit.map_err(|_| "scheduler_closed")?,
                _ = cancellation.changed() => return Err("scheduler_cancelled"),
            })
        } else {
            None
        };
        let lane_permit = self.acquire_lane_permit(lane, &mut cancellation).await?;
        drop(queued);
        if *cancellation.borrow() {
            drop(lane_permit);
            drop(connection_permit);
            return Err("scheduler_cancelled");
        }

        self.active.fetch_add(1, Ordering::SeqCst);
        increment(&self.per_connection, connection);
        if let Some(route) = route {
            increment(&self.per_route, route);
        }
        let lease = Arc::new(ActiveLease {
            lane_permit: Mutex::new(Some(lane_permit)),
            connection_permit: Mutex::new(Some(connection_permit)),
            background_permit: Mutex::new(background_permit),
            scheduler: self.clone(),
            connection: connection.into(),
            route: route.map(str::to_owned),
            released: AtomicBool::new(false),
        });
        *job.active.lock().expect("scheduler job lock poisoned") = Some(lease.clone());

        // Close the tiny race between the first cancellation check & install.
        if *cancellation.borrow() {
            lease.release();
            return Err("scheduler_cancelled");
        }
        Ok(SchedulerPermit {
            lease,
            job: job.clone(),
            cancellation,
        })
    }

    async fn acquire_lane_permit(
        &self,
        lane: InferenceLane,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<OwnedSemaphorePermit, &'static str> {
        if lane == InferenceLane::Interactive {
            if let Ok(permit) = self.shared.clone().try_acquire_owned() {
                return Ok(permit);
            }
            if self.reserved_slots > 0 {
                return tokio::select! {
                    permit = self.interactive_reserved.clone().acquire_owned() => permit.map_err(|_| "scheduler_closed"),
                    permit = self.shared.clone().acquire_owned() => permit.map_err(|_| "scheduler_closed"),
                    _ = cancellation.changed() => Err("scheduler_cancelled"),
                };
            }
        }
        tokio::select! {
            permit = self.shared.clone().acquire_owned() => permit.map_err(|_| "scheduler_closed"),
            _ = cancellation.changed() => Err("scheduler_cancelled"),
        }
    }

    /// Creates a real bounded Tokio MPSC channel for one active stream.
    pub fn event_channel<T>(&self) -> (SchedulerEventSender<T>, SchedulerEventReceiver<T>) {
        let (sender, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        (
            SchedulerEventSender {
                sender,
                buffered_events: self.buffered_events.clone(),
            },
            SchedulerEventReceiver {
                receiver,
                buffered_events: self.buffered_events.clone(),
            },
        )
    }

    pub async fn snapshot(&self) -> LoadSnapshot {
        LoadSnapshot {
            queued_agents: self.queued.load(Ordering::SeqCst),
            active_agents: self.active.load(Ordering::SeqCst),
            active_limit: self.limit,
            per_connection_active: self
                .per_connection
                .lock()
                .expect("scheduler connection lock poisoned")
                .clone(),
            per_route_active: self
                .per_route
                .lock()
                .expect("scheduler route lock poisoned")
                .clone(),
            buffered_events: self.buffered_events.load(Ordering::SeqCst),
        }
    }
}

impl SchedulerJob {
    pub fn cancel(&self) {
        let _ = self.cancellation.send(true);
        if let Some(lease) = self
            .active
            .lock()
            .expect("scheduler job lock poisoned")
            .take()
        {
            lease.release();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancellation.borrow()
    }
}

impl SchedulerPermit {
    pub fn is_cancelled(&self) -> bool {
        *self.cancellation.borrow()
    }

    pub async fn cancelled(&mut self) {
        if !*self.cancellation.borrow() {
            let _ = self.cancellation.changed().await;
        }
    }
}

impl Drop for SchedulerPermit {
    fn drop(&mut self) {
        self.lease.release();
        let mut active = self.job.active.lock().expect("scheduler job lock poisoned");
        if active
            .as_ref()
            .is_some_and(|lease| Arc::ptr_eq(lease, &self.lease))
        {
            *active = None;
        }
    }
}

impl ActiveLease {
    fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let lane_permit = self
            .lane_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if lane_permit.is_none() {
            return;
        }
        let connection_permit = self
            .connection_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let background_permit = self
            .background_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.scheduler.active.fetch_sub(1, Ordering::SeqCst);
        decrement(&self.scheduler.per_connection, &self.connection);
        if let Some(route) = &self.route {
            decrement(&self.scheduler.per_route, route);
        }
        drop(connection_permit);
        drop(background_permit);
        drop(lane_permit);
    }
}

impl<T> Clone for SchedulerEventSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            buffered_events: self.buffered_events.clone(),
        }
    }
}

impl<T> SchedulerEventSender<T> {
    pub async fn send(&self, event: T) -> Result<(), mpsc::error::SendError<T>> {
        self.sender.send(event).await?;
        self.buffered_events.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn try_send(&self, event: T) -> Result<(), mpsc::error::TrySendError<T>> {
        self.sender.try_send(event)?;
        self.buffered_events.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl<T> SchedulerEventReceiver<T> {
    pub async fn recv(&mut self) -> Option<T> {
        let event = self.receiver.recv().await;
        if event.is_some() {
            decrement_atomic(&self.buffered_events, 1);
        }
        event
    }

    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        let event = self.receiver.try_recv()?;
        decrement_atomic(&self.buffered_events, 1);
        Ok(event)
    }
}

impl<T> Drop for SchedulerEventReceiver<T> {
    fn drop(&mut self) {
        decrement_atomic(&self.buffered_events, self.receiver.len());
    }
}

fn increment(map: &Mutex<BTreeMap<String, usize>>, key: &str) {
    *map.lock()
        .expect("scheduler counter lock poisoned")
        .entry(key.into())
        .or_default() += 1;
}

fn decrement(map: &Mutex<BTreeMap<String, usize>>, key: &str) {
    let mut map = map.lock().expect("scheduler counter lock poisoned");
    if let Some(value) = map.get_mut(key) {
        *value -= 1;
        if *value == 0 {
            map.remove(key);
        }
    }
}

fn decrement_atomic(counter: &AtomicUsize, amount: usize) {
    let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
        Some(current.saturating_sub(amount))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    async fn wait_for_queued(scheduler: &InferenceScheduler, expected: usize) {
        while scheduler.snapshot().await.queued_agents < expected {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn defaults_to_four_active_streams() {
        let snapshot = InferenceScheduler::default().snapshot().await;
        assert_eq!(snapshot.active_limit, 4);
    }

    #[tokio::test]
    async fn interactive_lane_keeps_one_slot_reserved_from_background_fanout() {
        let scheduler = InferenceScheduler::new(4);
        let mut background = Vec::new();
        for connection in ["bg-a", "bg-b", "bg-c"] {
            background.push(
                scheduler
                    .acquire_lane(InferenceLane::Conductor, connection, None)
                    .await
                    .unwrap(),
            );
        }
        let blocked = tokio::spawn({
            let scheduler = scheduler.clone();
            async move {
                scheduler
                    .acquire_lane(InferenceLane::Conductor, "bg-d", None)
                    .await
            }
        });
        wait_for_queued(&scheduler, 1).await;
        let interactive = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            scheduler.acquire_lane(InferenceLane::Interactive, "operator", None),
        )
        .await
        .expect("interactive reserve must remain immediately available")
        .unwrap();
        drop(interactive);
        drop(background);
        drop(blocked.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn per_connection_limit_prevents_one_provider_from_taking_every_slot() {
        let scheduler = InferenceScheduler::new(4);
        let first = scheduler
            .acquire_lane(InferenceLane::Conductor, "provider-a", None)
            .await
            .unwrap();
        let second = scheduler
            .acquire_lane(InferenceLane::Conductor, "provider-a", None)
            .await
            .unwrap();
        let third = tokio::spawn({
            let scheduler = scheduler.clone();
            async move {
                scheduler
                    .acquire_lane(InferenceLane::Conductor, "provider-a", None)
                    .await
            }
        });
        wait_for_queued(&scheduler, 1).await;
        let other = scheduler
            .acquire_lane(InferenceLane::Conductor, "provider-b", None)
            .await
            .unwrap();
        drop(first);
        drop(third.await.unwrap().unwrap());
        drop(second);
        drop(other);
    }

    #[tokio::test]
    async fn weighted_shared_pool_caps_background_without_blocking_conductor() {
        let scheduler = InferenceScheduler::new(4);
        let background = scheduler
            .acquire_lane(InferenceLane::Background, "background-a", None)
            .await
            .unwrap();
        let second_background = tokio::spawn({
            let scheduler = scheduler.clone();
            async move {
                scheduler
                    .acquire_lane(InferenceLane::Background, "background-b", None)
                    .await
            }
        });
        wait_for_queued(&scheduler, 1).await;
        let conductor = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            scheduler.acquire_lane(InferenceLane::Conductor, "conductor", None),
        )
        .await
        .expect("conductor weight must retain shared admission")
        .unwrap();
        drop(background);
        drop(second_background.await.unwrap().unwrap());
        drop(conductor);
    }

    #[tokio::test]
    async fn overflow_acquires_in_fifo_order() {
        let scheduler = InferenceScheduler::new(1);
        let first = scheduler.acquire("direct-a", None).await.unwrap();
        let (order_sender, mut order_receiver) = mpsc::channel(3);
        let mut tasks = Vec::new();
        for number in 1..=3 {
            let task_scheduler = scheduler.clone();
            let sender = order_sender.clone();
            tasks.push(tokio::spawn(async move {
                let permit = task_scheduler.acquire("direct-a", None).await.unwrap();
                sender.send(number).await.unwrap();
                drop(permit);
            }));
            wait_for_queued(&scheduler, number).await;
        }
        drop(order_sender);
        drop(first);
        assert_eq!(order_receiver.recv().await, Some(1));
        assert_eq!(order_receiver.recv().await, Some(2));
        assert_eq!(order_receiver.recv().await, Some(3));
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancelling_a_queued_job_never_acquires_a_request_slot() {
        let scheduler = InferenceScheduler::new(1);
        let first = scheduler.acquire("direct-a", None).await.unwrap();
        let job = scheduler.new_job();
        let waiter = tokio::spawn({
            let scheduler = scheduler.clone();
            let job = job.clone();
            async move { scheduler.acquire_job(&job, "direct-b", None).await }
        });
        wait_for_queued(&scheduler, 1).await;
        job.cancel();
        assert!(matches!(waiter.await.unwrap(), Err("scheduler_cancelled")));
        let snapshot = scheduler.snapshot().await;
        assert_eq!(snapshot.queued_agents, 0);
        assert_eq!(snapshot.active_agents, 1);
        assert!(!snapshot.per_connection_active.contains_key("direct-b"));
        drop(first);
    }

    #[tokio::test]
    async fn cancelling_an_active_job_releases_its_permit_and_counters() {
        let scheduler = InferenceScheduler::new(1);
        let job = scheduler.new_job();
        let mut permit = scheduler
            .acquire_job(&job, "direct-a", Some("route-a"))
            .await
            .unwrap();
        job.cancel();
        permit.cancelled().await;
        assert!(permit.is_cancelled());
        let snapshot = scheduler.snapshot().await;
        assert_eq!(snapshot.active_agents, 0);
        assert!(snapshot.per_connection_active.is_empty());
        assert!(snapshot.per_route_active.is_empty());
        let next = scheduler.acquire("direct-b", None).await.unwrap();
        drop(next);
    }

    #[tokio::test]
    async fn event_channel_is_bounded_to_128_and_snapshot_counts_buffered_events() {
        let scheduler = InferenceScheduler::default();
        let (sender, mut receiver) = scheduler.event_channel();
        for event in 0..EVENT_CHANNEL_CAPACITY {
            sender.try_send(event).unwrap();
        }
        assert!(matches!(
            sender.try_send(128),
            Err(mpsc::error::TrySendError::Full(128))
        ));
        assert_eq!(
            scheduler.snapshot().await.buffered_events,
            EVENT_CHANNEL_CAPACITY
        );
        assert_eq!(receiver.recv().await, Some(0));
        assert_eq!(
            scheduler.snapshot().await.buffered_events,
            EVENT_CHANNEL_CAPACITY - 1
        );
    }
}
