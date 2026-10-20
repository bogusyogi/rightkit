//! Service registration (PK07-5): an app declares **who it is**, **which methods it offers** and
//! **what handles each one**, and RightKit owns the rest: the endpoint, peer authentication,
//! admission, lifecycle, bounded connections and events, and shutdown.
//!
//! ```text
//! let registration = Registration::new(ServiceIdentity::new("mail-agent", "engine", 5, "1.0")?)
//!     .method(MethodDescriptor::local("mail.list", ["mailright"]), |cx, params, _emit| {
//!         Ok(json!({ "app": cx.app(), "params": params }))
//!     })?
//!     .method(MethodDescriptor::local("mail.send", ["mailright"]), handle_send)?;
//! let service = Service::builder()
//!     .register(&suite, registration, HostOptions::new())?
//!     .start()?;
//! ```
//!
//! A [`Registration`] is a value, not a framework: identity plus a [`MethodTable`] plus one closure
//! per method, checked together when built (a method needs a descriptor, an id is not duplicated
//! or reserved, a method that is reachable by remote devices needs the
//! [`TransportProfile::LocalAndRemote`] profile). It adds no transport. The only listener a
//! service has is the local one [`Service::builder`](crate::Service::builder) already hosts (a
//! current-user-only socket or pipe); nothing here opens a network address, and a service
//! registered with [`TransportProfile::Local`] refuses `ServiceBuilder::remote` at `start`. That
//! is what lets an app satisfy "hosts a service" without registering a public listener.
//!
//! The method table remains the one source of method policy: the local allowlist and, with the
//! `remote` feature, the device route table are both derived from it, exactly as for a hand-built
//! [`Handler`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde_json::Value;

use crate::contract::{validate_method_id, CallContext, Emit, Handler, MethodPolicy, MethodTable};
use crate::contract::{TableError, Transport};
use crate::error::ServiceError;
use crate::server::ServiceConfig;
use crate::suite::valid_service_name;
use crate::Allowlist;

/// Which transports a registered service may be offered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportProfile {
    /// The local transport only. No method may be reachable by a remote device, and the builder
    /// refuses a remote daemon.
    Local,
    /// The local transport, and signed remote devices for the methods whose policy names them.
    LocalAndRemote,
}

/// Who the service is: the identity published in `service.json` and used for ranking and takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceIdentity {
    service: String,
    role: String,
    rank: u8,
    version: String,
}

fn plain_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(|c| !c.is_control())
}

impl ServiceIdentity {
    /// `service` must be a valid service name ([`valid_service_name`]); `role` and `version` must
    /// be short, non-empty and free of control characters. Lower `rank` wins a takeover.
    pub fn new(
        service: &str,
        role: &str,
        rank: u8,
        version: &str,
    ) -> Result<Self, RegistrationError> {
        if !valid_service_name(service) {
            return Err(RegistrationError::BadIdentity("service name"));
        }
        if !plain_text(role) {
            return Err(RegistrationError::BadIdentity("role"));
        }
        if !plain_text(version) {
            return Err(RegistrationError::BadIdentity("version"));
        }
        Ok(Self {
            service: service.to_string(),
            role: role.to_string(),
            rank,
            version: version.to_string(),
        })
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn rank(&self) -> u8 {
        self.rank
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The host configuration for this identity. The allowlist is empty here and replaced by the
    /// one the method table implies when the service starts.
    pub fn to_config(&self) -> ServiceConfig {
        ServiceConfig::new(
            &self.service,
            &self.role,
            self.rank,
            &self.version,
            Allowlist::new(),
        )
    }
}

/// One method an app offers: its id and who may call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDescriptor {
    id: String,
    policy: MethodPolicy,
}

impl MethodDescriptor {
    pub fn new(id: impl Into<String>, policy: MethodPolicy) -> Self {
        Self {
            id: id.into(),
            policy,
        }
    }

    /// A method local apps named in `apps` may call (`"*"` for any).
    pub fn local<I, S>(id: impl Into<String>, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new(id, MethodPolicy::local(apps))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn policy(&self) -> &MethodPolicy {
        &self.policy
    }
}

/// Why a registration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistrationError {
    /// A part of the identity is not acceptable (`"service name"`, `"role"`, `"version"`).
    BadIdentity(&'static str),
    /// The method id or policy was refused by the method table.
    Table(TableError),
    /// `ping` is answered by the host and cannot be registered.
    Reserved(String),
    /// A method id was registered twice.
    Duplicate(String),
    /// A method is reachable by remote devices but the profile is [`TransportProfile::Local`].
    RemoteMethodInLocalProfile(String),
    /// A remote daemon was configured for a service registered with [`TransportProfile::Local`].
    RemoteNotDeclared,
    /// No method was registered.
    NoMethods,
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistrationError::BadIdentity(part) => {
                write!(f, "service registration: the {part} is not acceptable")
            }
            RegistrationError::Table(e) => write!(f, "service registration: {e}"),
            RegistrationError::Reserved(id) => {
                write!(f, "service registration: {id:?} is answered by the host")
            }
            RegistrationError::Duplicate(id) => {
                write!(f, "service registration: method {id:?} is registered twice")
            }
            RegistrationError::RemoteMethodInLocalProfile(id) => write!(
                f,
                "service registration: method {id:?} is reachable by remote devices but the profile is local"
            ),
            RegistrationError::RemoteNotDeclared => f.write_str(
                "service registration: a remote daemon was configured for a local-only registration",
            ),
            RegistrationError::NoMethods => {
                f.write_str("service registration: no method was registered")
            }
        }
    }
}

impl std::error::Error for RegistrationError {}

impl From<TableError> for RegistrationError {
    fn from(e: TableError) -> Self {
        RegistrationError::Table(e)
    }
}

type MethodFn = Box<dyn Fn(&CallContext, &Value, &Emit) -> Result<Value, ServiceError> + Send + Sync>;

/// What [`Registration::into_parts`] hands to the builder.
type Parts = (ServiceIdentity, TransportProfile, MethodTable, Arc<dyn Handler>);

/// Identity, method descriptors and one handler closure per method, checked together. See the
/// module docs.
pub struct Registration {
    identity: ServiceIdentity,
    transport: TransportProfile,
    table: MethodTable,
    handlers: BTreeMap<String, MethodFn>,
    on_handoff: Option<Box<dyn Fn() + Send + Sync>>,
    on_connection_closed: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

impl Registration {
    /// A registration on the local transport only ([`TransportProfile::Local`]).
    pub fn new(identity: ServiceIdentity) -> Self {
        Self {
            identity,
            transport: TransportProfile::Local,
            table: MethodTable::new(),
            handlers: BTreeMap::new(),
            on_handoff: None,
            on_connection_closed: None,
        }
    }

    /// Choose the transport profile. Set it before registering methods that name remote scopes.
    pub fn transport(mut self, transport: TransportProfile) -> Self {
        self.transport = transport;
        self
    }

    /// Register a method and its handler.
    pub fn method(
        self,
        descriptor: MethodDescriptor,
        handler: impl Fn(&CallContext, &Value, &Emit) -> Result<Value, ServiceError>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self, RegistrationError> {
        self.method_boxed(descriptor, Box::new(handler))
    }

    fn method_boxed(
        mut self,
        descriptor: MethodDescriptor,
        handler: MethodFn,
    ) -> Result<Self, RegistrationError> {
        let MethodDescriptor { id, policy } = descriptor;
        validate_method_id(&id)?;
        if id == "ping" {
            return Err(RegistrationError::Reserved(id));
        }
        if self.handlers.contains_key(&id) {
            return Err(RegistrationError::Duplicate(id));
        }
        if self.transport == TransportProfile::Local && policy.transport() != Transport::Local {
            return Err(RegistrationError::RemoteMethodInLocalProfile(id));
        }
        // `try_method` consumes the table; the copy keeps `self.table` valid on every path.
        self.table = self.table.clone().try_method(id.clone(), policy)?;
        self.handlers.insert(id, handler);
        Ok(self)
    }

    /// Called once when a higher-ranked host asks for the service: stop starting new steps.
    pub fn on_handoff(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_handoff = Some(Box::new(hook));
        self
    }

    /// Called with the connection id when a connection closes.
    pub fn on_connection_closed(mut self, hook: impl Fn(u64) + Send + Sync + 'static) -> Self {
        self.on_connection_closed = Some(Box::new(hook));
        self
    }

    pub fn identity(&self) -> &ServiceIdentity {
        &self.identity
    }

    pub fn profile(&self) -> TransportProfile {
        self.transport
    }

    pub fn methods(&self) -> &MethodTable {
        &self.table
    }

    /// The parts a [`ServiceBuilder`](crate::ServiceBuilder) hosts: the identity, the profile, the
    /// method table and the one handler that dispatches to the registered closures.
    pub fn into_parts(self) -> Result<Parts, RegistrationError> {
        if self.handlers.is_empty() {
            return Err(RegistrationError::NoMethods);
        }
        let handler: Arc<dyn Handler> = Arc::new(Registered {
            handlers: self.handlers,
            on_handoff: self.on_handoff,
            on_connection_closed: self.on_connection_closed,
        });
        Ok((self.identity, self.transport, self.table, handler))
    }
}

/// The handler a [`Registration`] builds: dispatch by method id.
struct Registered {
    handlers: BTreeMap<String, MethodFn>,
    on_handoff: Option<Box<dyn Fn() + Send + Sync>>,
    on_connection_closed: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

impl Handler for Registered {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match self.handlers.get(method) {
            Some(handler) => handler(cx, params, emit),
            // The allowlist already stops an unregistered method; this is the same answer if a
            // method id ever reaches the handler by another route.
            None => Err(ServiceError::new(
                "unknown_method",
                format!("{method} is not registered"),
            )),
        }
    }

    fn on_handoff(&self) {
        if let Some(hook) = &self.on_handoff {
            hook();
        }
    }

    fn connection_closed(&self, conn: u64) {
        if let Some(hook) = &self.on_connection_closed {
            hook(conn);
        }
    }
}
