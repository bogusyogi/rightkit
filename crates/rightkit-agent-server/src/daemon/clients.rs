//! One supervisor model for daemon clients, leases, ownership and birth
//! without clients (DL-006 and DL-008).
//!
//! Ported from prime-agent's `daemon-supervisor.ts` client maps and cleanup
//! paths (`handleConnection`, input-pause leases, owner-client grace, and
//! startup adoption). The important property is structural: client,
//! connection, session attachment, worker owner, pause lease, and disconnect
//! grace are not independent registries. Every transition goes through
//! [`DaemonSupervisor`], which enforces the identity relationships.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PauseLeaseId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOwner {
    pub worker_id: WorkerId,
    pub client_id: Option<ClientId>,
    pub session_id: SessionId,
}

#[derive(Debug, Clone)]
struct ClientState {
    connection_id: ConnectionId,
    attached_sessions: HashSet<SessionId>,
    pause_leases: HashSet<PauseLeaseId>,
    worker_ids: HashSet<WorkerId>,
    disconnected_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct PauseLease {
    id: PauseLeaseId,
    client_id: ClientId,
    connection_id: ConnectionId,
    session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupResidency {
    pub adopted_workers: Vec<WorkerOwner>,
    pub scheduled_work: usize,
    pub should_evaluate_residency: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SupervisorError {
    #[error("client is not registered: {0}")]
    UnknownClient(String),
    #[error("connection is already registered for another client")]
    ConnectionConflict,
    #[error("session is not attached to client")]
    SessionNotAttached,
    #[error("worker is already owned by another client")]
    WorkerConflict,
    #[error("pause lease is not owned by client")]
    PauseLeaseOwnerConflict,
    #[error("pause lease is not registered")]
    UnknownPauseLease,
}

/// The sole in-memory authority for all client/worker/lease relationships.
#[derive(Debug, Default)]
pub struct DaemonSupervisor {
    clients: HashMap<ClientId, ClientState>,
    connections: HashMap<ConnectionId, ClientId>,
    workers: HashMap<WorkerId, WorkerOwner>,
    pauses: HashMap<PauseLeaseId, PauseLease>,
    next_pause_id: u64,
    started: bool,
    ever_attached: bool,
    scheduled_work: usize,
}

impl DaemonSupervisor {
    /// Start with no clients. Adoption and residency are startup operations,
    /// not consequences of the first socket attachment (DL-008).
    pub fn start_empty(adopted_workers: Vec<WorkerOwner>, scheduled_work: usize) -> Self {
        let mut supervisor = Self {
            started: true,
            scheduled_work,
            ..Self::default()
        };
        for worker in adopted_workers {
            supervisor.workers.insert(worker.worker_id.clone(), worker);
        }
        supervisor
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn live_client_count(&self) -> usize {
        self.clients
            .values()
            .filter(|client| client.disconnected_at.is_none())
            .count()
    }

    /// Ids of every client that is registered and not in disconnect grace.
    pub fn live_client_ids(&self) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter(|(_, client)| client.disconnected_at.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn ever_attached(&self) -> bool {
        self.ever_attached
    }

    pub fn startup_residency(&self) -> StartupResidency {
        StartupResidency {
            adopted_workers: self.workers.values().cloned().collect(),
            scheduled_work: self.scheduled_work,
            should_evaluate_residency: self.scheduled_work > 0 || !self.workers.is_empty(),
        }
    }

    /// Seed durable schedule residency before any client connects. A daemon
    /// born headless must not wait for first attachment to evaluate work.
    pub fn set_scheduled_work(&mut self, scheduled_work: usize) {
        self.scheduled_work = scheduled_work;
    }

    pub fn register_client(
        &mut self,
        client_id: ClientId,
        connection_id: ConnectionId,
    ) -> Result<(), SupervisorError> {
        if let Some(existing) = self.connections.get(&connection_id) {
            if existing != &client_id {
                return Err(SupervisorError::ConnectionConflict);
            }
        }
        if let Some(old_connection_id) = self
            .clients
            .get(&client_id)
            .map(|client| client.connection_id.clone())
        {
            if old_connection_id != connection_id {
                self.connections.remove(&old_connection_id);
            }
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.connection_id = connection_id.clone();
            client.disconnected_at = None;
        } else {
            self.clients.insert(
                client_id.clone(),
                ClientState {
                    connection_id: connection_id.clone(),
                    attached_sessions: HashSet::new(),
                    pause_leases: HashSet::new(),
                    worker_ids: HashSet::new(),
                    disconnected_at: None,
                },
            );
        }
        for lease in self
            .pauses
            .values_mut()
            .filter(|lease| lease.client_id == client_id)
        {
            lease.connection_id = connection_id.clone();
        }
        self.connections.insert(connection_id, client_id);
        Ok(())
    }

    pub fn attach_session(
        &mut self,
        client_id: &ClientId,
        session_id: SessionId,
    ) -> Result<(), SupervisorError> {
        let client = self.client_mut(client_id)?;
        client.attached_sessions.insert(session_id);
        self.ever_attached = true;
        Ok(())
    }

    pub fn detach_session(
        &mut self,
        client_id: &ClientId,
        session_id: &SessionId,
    ) -> Result<(), SupervisorError> {
        let client = self.client_mut(client_id)?;
        if !client.attached_sessions.remove(session_id) {
            return Err(SupervisorError::SessionNotAttached);
        }
        Ok(())
    }

    pub fn claim_worker(
        &mut self,
        client_id: Option<ClientId>,
        session_id: SessionId,
        worker_id: WorkerId,
    ) -> Result<(), SupervisorError> {
        if let Some(existing) = self.workers.get(&worker_id) {
            if existing.client_id.as_ref() != client_id.as_ref() {
                return Err(SupervisorError::WorkerConflict);
            }
            return Ok(());
        }
        if let Some(ref id) = client_id {
            self.client_mut(id)?;
        }
        let owner = WorkerOwner {
            worker_id: worker_id.clone(),
            client_id: client_id.clone(),
            session_id,
        };
        self.workers.insert(worker_id.clone(), owner);
        if let Some(id) = client_id {
            self.client_mut(&id)?.worker_ids.insert(worker_id);
        }
        Ok(())
    }

    pub fn release_worker(
        &mut self,
        client_id: Option<&ClientId>,
        worker_id: &WorkerId,
    ) -> Result<(), SupervisorError> {
        let owner = self
            .workers
            .get(worker_id)
            .ok_or(SupervisorError::WorkerConflict)?;
        if owner.client_id.as_ref() != client_id {
            return Err(SupervisorError::WorkerConflict);
        }
        let owner = self.workers.remove(worker_id).expect("checked above");
        if let Some(client_id) = owner.client_id {
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.worker_ids.remove(worker_id);
            }
        }
        Ok(())
    }

    pub fn acquire_pause_lease(
        &mut self,
        client_id: &ClientId,
        session_id: &SessionId,
    ) -> Result<PauseLeaseId, SupervisorError> {
        let connection_id = self.client(client_id)?.connection_id.clone();
        if !self
            .client(client_id)?
            .attached_sessions
            .contains(session_id)
        {
            return Err(SupervisorError::SessionNotAttached);
        }
        self.next_pause_id += 1;
        let id = PauseLeaseId(format!("pause-{}", self.next_pause_id));
        let lease = PauseLease {
            id: id.clone(),
            client_id: client_id.clone(),
            connection_id,
            session_id: session_id.clone(),
        };
        self.pauses.insert(id.clone(), lease);
        self.client_mut(client_id)?.pause_leases.insert(id.clone());
        Ok(id)
    }

    pub fn release_pause_lease(
        &mut self,
        client_id: &ClientId,
        lease_id: &PauseLeaseId,
    ) -> Result<(), SupervisorError> {
        let lease = self
            .pauses
            .get(lease_id)
            .ok_or(SupervisorError::UnknownPauseLease)?;
        if &lease.client_id != client_id {
            return Err(SupervisorError::PauseLeaseOwnerConflict);
        }
        let lease = self.pauses.remove(lease_id).expect("checked above");
        self.client_mut(&lease.client_id)?
            .pause_leases
            .remove(&lease.id);
        Ok(())
    }

    pub fn release_pause_for_session(
        &mut self,
        client_id: &ClientId,
        session_id: &SessionId,
    ) -> Result<(), SupervisorError> {
        let lease_id = self
            .pauses
            .values()
            .find(|lease| &lease.client_id == client_id && &lease.session_id == session_id)
            .map(|lease| lease.id.clone())
            .ok_or(SupervisorError::UnknownPauseLease)?;
        self.release_pause_lease(client_id, &lease_id)
    }

    /// Disconnect keeps the client identity and leases until grace expires;
    /// reconnecting the same client therefore restores ownership instead of
    /// creating a second logical client.
    pub fn disconnect(&mut self, connection_id: &ConnectionId) -> Result<(), SupervisorError> {
        let client_id = self
            .connections
            .remove(connection_id)
            .ok_or_else(|| SupervisorError::UnknownClient(connection_id.0.clone()))?;
        self.client_mut(&client_id)?.disconnected_at = Some(Instant::now());
        Ok(())
    }

    /// Refresh connection liveness without changing client identity or any
    /// attached session/worker/lease relationships.
    pub fn heartbeat(&mut self, client_id: &ClientId) -> Result<(), SupervisorError> {
        let client = self.client_mut(client_id)?;
        client.disconnected_at = None;
        Ok(())
    }

    pub fn reap_disconnect_grace(&mut self, now: Instant, grace: Duration) -> Vec<ClientId> {
        let expired: Vec<ClientId> = self
            .clients
            .iter()
            .filter_map(|(id, state)| {
                state
                    .disconnected_at
                    .filter(|at| now.duration_since(*at) >= grace)
                    .map(|_| id.clone())
            })
            .collect();
        for client_id in &expired {
            if let Some(state) = self.clients.remove(client_id) {
                for lease_id in state.pause_leases {
                    self.pauses.remove(&lease_id);
                }
                for worker_id in state.worker_ids {
                    self.workers.remove(&worker_id);
                }
            }
        }
        expired
    }

    /// Snapshot of every currently-owned worker. Returned by value so a
    /// caller (RA-003's idle-eviction producer) can drop this lock before
    /// doing any session/storage work keyed by the returned ids.
    pub fn worker_owners(&self) -> Vec<WorkerOwner> {
        self.workers.values().cloned().collect()
    }

    /// Count of currently-live (not in disconnect grace) clients with
    /// `session_id` attached. Backs `SessionResidencySnapshot::attached_clients`
    /// with real presence state instead of a hardcoded zero.
    pub fn attached_client_count(&self, session_id: &SessionId) -> usize {
        self.clients
            .values()
            .filter(|client| {
                client.disconnected_at.is_none() && client.attached_sessions.contains(session_id)
            })
            .count()
    }

    /// True if `client_id` is registered and not currently in disconnect grace.
    pub fn client_is_live(&self, client_id: &ClientId) -> bool {
        self.clients
            .get(client_id)
            .is_some_and(|client| client.disconnected_at.is_none())
    }

    fn client(&self, client_id: &ClientId) -> Result<&ClientState, SupervisorError> {
        self.clients
            .get(client_id)
            .ok_or_else(|| SupervisorError::UnknownClient(client_id.0.clone()))
    }

    fn client_mut(&mut self, client_id: &ClientId) -> Result<&mut ClientState, SupervisorError> {
        self.clients
            .get_mut(client_id)
            .ok_or_else(|| SupervisorError::UnknownClient(client_id.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(id: &str, client_id: Option<&str>) -> WorkerOwner {
        WorkerOwner {
            worker_id: WorkerId(id.into()),
            client_id: client_id.map(|value| ClientId(value.into())),
            session_id: SessionId(format!("session-{id}")),
        }
    }

    #[test]
    fn starts_and_adopts_workers_with_zero_clients() {
        let supervisor = DaemonSupervisor::start_empty(vec![worker("w1", None)], 2);
        assert!(supervisor.is_started());
        assert_eq!(supervisor.client_count(), 0);
        let residency = supervisor.startup_residency();
        assert_eq!(residency.adopted_workers.len(), 1);
        assert_eq!(residency.scheduled_work, 2);
        assert!(residency.should_evaluate_residency);
    }

    #[test]
    fn one_model_enforces_attachment_owner_and_pause_lease() {
        let mut supervisor = DaemonSupervisor::start_empty(Vec::new(), 0);
        let client = ClientId("c1".into());
        let connection = ConnectionId("conn1".into());
        let session = SessionId("s1".into());
        supervisor
            .register_client(client.clone(), connection)
            .unwrap();
        supervisor.attach_session(&client, session.clone()).unwrap();
        supervisor
            .claim_worker(Some(client.clone()), session.clone(), WorkerId("w1".into()))
            .unwrap();
        let lease = supervisor.acquire_pause_lease(&client, &session).unwrap();
        assert_eq!(supervisor.live_client_count(), 1);
        supervisor.release_pause_lease(&client, &lease).unwrap();
        assert!(supervisor.workers.contains_key(&WorkerId("w1".into())));
    }

    #[test]
    fn reconnecting_same_client_preserves_logical_identity() {
        let mut supervisor = DaemonSupervisor::start_empty(Vec::new(), 0);
        let client = ClientId("c1".into());
        supervisor
            .register_client(client.clone(), ConnectionId("old".into()))
            .unwrap();
        supervisor
            .attach_session(&client, SessionId("s1".into()))
            .unwrap();
        supervisor.disconnect(&ConnectionId("old".into())).unwrap();
        supervisor
            .register_client(client.clone(), ConnectionId("new".into()))
            .unwrap();
        assert_eq!(supervisor.client_count(), 1);
        assert_eq!(supervisor.live_client_count(), 1);
        assert_eq!(
            supervisor.disconnect(&ConnectionId("old".into())),
            Err(SupervisorError::UnknownClient("old".into()))
        );
    }

    #[test]
    fn attached_client_count_ignores_disconnected_clients() {
        let mut supervisor = DaemonSupervisor::start_empty(Vec::new(), 0);
        let client = ClientId("c1".into());
        let session = SessionId("s1".into());
        supervisor
            .register_client(client.clone(), ConnectionId("conn".into()))
            .unwrap();
        supervisor.attach_session(&client, session.clone()).unwrap();
        assert_eq!(supervisor.attached_client_count(&session), 1);
        assert!(supervisor.client_is_live(&client));

        supervisor.disconnect(&ConnectionId("conn".into())).unwrap();
        assert_eq!(supervisor.attached_client_count(&session), 0);
        assert!(!supervisor.client_is_live(&client));
    }

    #[test]
    fn worker_owners_reflects_claims_and_releases() {
        let mut supervisor = DaemonSupervisor::start_empty(Vec::new(), 0);
        assert!(supervisor.worker_owners().is_empty());
        supervisor
            .claim_worker(None, SessionId("s1".into()), WorkerId("w1".into()))
            .unwrap();
        assert_eq!(supervisor.worker_owners().len(), 1);
        supervisor
            .release_worker(None, &WorkerId("w1".into()))
            .unwrap();
        assert!(supervisor.worker_owners().is_empty());
    }

    #[test]
    fn disconnect_grace_reaps_owned_worker_and_lease() {
        let mut supervisor = DaemonSupervisor::start_empty(Vec::new(), 0);
        let client = ClientId("c1".into());
        let session = SessionId("s1".into());
        supervisor
            .register_client(client.clone(), ConnectionId("conn".into()))
            .unwrap();
        supervisor.attach_session(&client, session.clone()).unwrap();
        supervisor
            .claim_worker(Some(client.clone()), session.clone(), WorkerId("w".into()))
            .unwrap();
        let _lease = supervisor.acquire_pause_lease(&client, &session).unwrap();
        supervisor.disconnect(&ConnectionId("conn".into())).unwrap();
        let removed = supervisor.reap_disconnect_grace(
            Instant::now() + Duration::from_secs(31),
            Duration::from_secs(30),
        );
        assert_eq!(removed, vec![client]);
        assert_eq!(supervisor.client_count(), 0);
        assert!(supervisor.workers.is_empty());
        assert!(supervisor.pauses.is_empty());
    }
}
