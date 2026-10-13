use super::caller::{Caller, Scope};
use crate::error::{ServiceError, ERROR_FORBIDDEN, ERROR_NOT_ALLOWED};
use crate::record::Allowlist;
use std::collections::{BTreeMap, BTreeSet};

/// Which transports a method is reachable on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Transport {
    /// Local callers only.
    Local,
    /// Remote devices only.
    Remote,
    /// Both.
    Both,
}

/// Policy for one method id: which local apps may call it and which scopes a device needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodPolicy {
    transport: Transport,
    /// Local apps allowed; `"*"` means any app the table knows.
    apps: BTreeSet<String>,
    /// A device needs at least one of these scopes. Empty means no device: a policy that opens
    /// the method to devices without naming a scope is refused by [`MethodTable::try_method`],
    /// and if one is built anyway no device passes it.
    scopes: BTreeSet<Scope>,
}

impl MethodPolicy {
    /// Local-only, for the given apps (`"*"` for any).
    pub fn local<I, S>(apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            transport: Transport::Local,
            apps: apps.into_iter().map(Into::into).collect(),
            scopes: BTreeSet::new(),
        }
    }

    /// Remote-only, for devices holding at least one of `scopes`.
    pub fn remote<I, S>(scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        Self {
            transport: Transport::Remote,
            apps: BTreeSet::new(),
            scopes: scopes.into_iter().map(Into::into).collect(),
        }
    }

    /// Add local apps; the method becomes reachable on both transports if it was remote-only.
    pub fn and_local<I, S>(mut self, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.apps.extend(apps.into_iter().map(Into::into));
        self.transport = match self.transport {
            Transport::Remote => Transport::Both,
            t => t,
        };
        self
    }

    /// Add device scopes; the method becomes reachable on both transports if it was local-only.
    pub fn and_remote<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        self.scopes.extend(scopes.into_iter().map(Into::into));
        self.transport = match self.transport {
            Transport::Local => Transport::Both,
            t => t,
        };
        self
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    fn permits_scopes(&self, held: &BTreeSet<Scope>) -> bool {
        // An empty scope set admits nobody: "no scope named" must never read as "any device".
        self.transport != Transport::Local && self.scopes.iter().any(|s| held.contains(s))
    }

    /// Whether a device may reach the method at all (it names at least one scope).
    fn device_scopes_named(&self) -> bool {
        !self.scopes.is_empty()
    }
}

/// Why a method id or its policy was refused at table construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    /// The id is empty.
    EmptyMethod,
    /// The id holds a glob character (`*`, `?`, `[`, `]`, `{`, `}`), whitespace, a control
    /// character, or a path/URL delimiter (`/`, `%`, `#`). `*` is the allowlist wildcard for
    /// apps, never a method.
    BadMethod(String),
    /// A policy reachable by devices names no scope (`and_remote([])`, `remote([])`).
    NoDeviceScopes(String),
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableError::EmptyMethod => f.write_str("method id is empty"),
            TableError::BadMethod(m) => write!(f, "method id {m:?} is not a plain method name"),
            TableError::NoDeviceScopes(m) => write!(
                f,
                "method {m:?} is reachable by devices but names no scope (an empty scope set admits no device)"
            ),
        }
    }
}

impl std::error::Error for TableError {}

/// A method id must be a plain name: non-empty, no glob characters, whitespace, control
/// characters, `/`, `%` or `#`.
pub fn validate_method_id(id: &str) -> Result<(), TableError> {
    if id.is_empty() {
        return Err(TableError::EmptyMethod);
    }
    let bad = |c: char| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '*' | '?' | '[' | ']' | '{' | '}' | '/' | '%' | '#' | '\\'
            )
    };
    if id.chars().any(bad) {
        return Err(TableError::BadMethod(id.to_string()));
    }
    Ok(())
}

/// Local-transport authorization against an [`Allowlist`] (the form the host config carries and
/// `service.json` publishes); same answers as [`MethodTable::authorize`] for a local app.
pub(crate) fn authorize_allowlist(
    list: &Allowlist,
    caller: &Caller,
    method: &str,
) -> Result<(), ServiceError> {
    let ok = match caller {
        Caller::LocalApp { app, .. } => list.permits(app, method),
        Caller::Device { .. } => method == "ping",
    };
    if ok {
        Ok(())
    } else {
        Err(ServiceError::new(
            ERROR_NOT_ALLOWED,
            format!("{} may not call {method}", caller.label()),
        ))
    }
}

/// Every method of a service with its policy: the one source the local allowlist and a remote
/// route table are both derived from. `ping` is never listed and always allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MethodTable {
    methods: BTreeMap<String, MethodPolicy>,
}

impl MethodTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a method, refusing a bad id or a device-reachable policy with no scope
    /// ([`TableError`]).
    pub fn try_method(
        mut self,
        id: impl Into<String>,
        policy: MethodPolicy,
    ) -> Result<Self, TableError> {
        let id = id.into();
        validate_method_id(&id)?;
        if policy.transport != Transport::Local && !policy.device_scopes_named() {
            return Err(TableError::NoDeviceScopes(id));
        }
        self.methods.insert(id, policy);
        Ok(self)
    }

    /// [`try_method`](Self::try_method) for a table built in code: a bad id or an unscoped
    /// device policy is a programming error and panics at construction, never later at a call.
    #[track_caller]
    pub fn method(self, id: impl Into<String>, policy: MethodPolicy) -> Self {
        match self.try_method(id, policy) {
            Ok(t) => t,
            Err(e) => panic!("MethodTable::method: {e}"),
        }
    }

    pub fn get(&self, id: &str) -> Option<&MethodPolicy> {
        self.methods.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    /// The local allowlist this table implies: each local-reachable method, per app. A `"*"`
    /// app entry stays `"*"` and, as in [`Allowlist`], applies only to apps with no entry of
    /// their own.
    pub fn allowlist(&self) -> Allowlist {
        let mut a = Allowlist::new();
        for (id, p) in &self.methods {
            if p.transport == Transport::Remote {
                continue;
            }
            for app in &p.apps {
                a = a.allow(app, [id.clone()]);
            }
        }
        a
    }

    /// Whether a device may call `method` through `POST /v1/call/{method}`: only a method
    /// reachable on both transports, by a device holding one of its scopes. Anything else,
    /// including an unknown method, is `forbidden`, so a device cannot probe which ids exist.
    pub fn authorize_device_call(&self, caller: &Caller, method: &str) -> Result<(), ServiceError> {
        let both = self
            .methods
            .get(method)
            .is_some_and(|p| p.transport == Transport::Both);
        if method != "ping" && !both {
            return Err(ServiceError::new(
                ERROR_FORBIDDEN,
                format!("{} may not call {method}", caller.label()),
            ));
        }
        self.authorize(caller, method)
    }

    /// Whether `caller` may call `method`. `ping` is always allowed. A local app is refused with
    /// `request_not_allowed`; a device lacking scope with `forbidden`.
    pub fn authorize(&self, caller: &Caller, method: &str) -> Result<(), ServiceError> {
        if method == "ping" {
            return Ok(());
        }
        let policy = self.methods.get(method);
        match caller {
            Caller::LocalApp { .. } => authorize_allowlist(&self.allowlist(), caller, method),
            Caller::Device { id, scopes } => {
                if policy.is_some_and(|p| p.permits_scopes(scopes)) {
                    Ok(())
                } else {
                    Err(ServiceError::new(
                        ERROR_FORBIDDEN,
                        format!("{id} may not call {method}"),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerIdentity;

    fn table() -> MethodTable {
        MethodTable::new()
            .method("echo", MethodPolicy::local(["*"]))
            .method("both", MethodPolicy::local(["a"]).and_remote(["s1"]))
            .method("up", MethodPolicy::remote(["s2"]))
    }

    #[test]
    fn allowlist_is_derived_and_skips_remote_only() {
        let a = table().allowlist();
        assert!(a.permits("any", "echo"));
        assert!(a.permits("a", "both"));
        assert!(!a.permits("b", "both"));
        assert!(!a.permits("a", "up"));
        assert!(a.permits("b", "ping"));
    }

    #[test]
    fn an_empty_scope_set_admits_no_device() {
        let p = MethodPolicy::remote(Vec::<String>::new());
        let held: BTreeSet<Scope> = ["s1".to_string(), "any".to_string()].into();
        assert!(!p.permits_scopes(&held));
        assert!(!p.permits_scopes(&BTreeSet::new()));
        let q = MethodPolicy::local(["a"]).and_remote(Vec::<String>::new());
        assert!(!q.permits_scopes(&held));
    }

    #[test]
    fn device_call_needs_both_transports() {
        let t = table();
        let dev = Caller::device("dev_1", ["s1", "s2"]);
        assert!(t.authorize_device_call(&dev, "both").is_ok());
        for m in ["up", "echo", "missing"] {
            assert_eq!(
                t.authorize_device_call(&dev, m).unwrap_err().code,
                ERROR_FORBIDDEN,
                "{m}"
            );
        }
        let no_scope = Caller::device("dev_2", ["other"]);
        assert_eq!(
            t.authorize_device_call(&no_scope, "both").unwrap_err().code,
            ERROR_FORBIDDEN
        );
    }

    #[test]
    fn authorize_by_caller() {
        let t = table();
        let app = |n: &str| Caller::local_app(n, PeerIdentity::default());
        assert!(t.authorize(&app("a"), "both").is_ok());
        assert_eq!(
            t.authorize(&app("b"), "both").unwrap_err().code,
            ERROR_NOT_ALLOWED
        );
        assert_eq!(
            t.authorize(&app("a"), "up").unwrap_err().code,
            ERROR_NOT_ALLOWED
        );
        let dev = Caller::device("dev_1", ["s1"]);
        assert!(t.authorize(&dev, "both").is_ok());
        assert_eq!(t.authorize(&dev, "up").unwrap_err().code, ERROR_FORBIDDEN);
        assert_eq!(t.authorize(&dev, "echo").unwrap_err().code, ERROR_FORBIDDEN);
        assert_eq!(
            t.authorize(&dev, "missing").unwrap_err().code,
            ERROR_FORBIDDEN
        );
        assert!(t.authorize(&dev, "ping").is_ok());
    }
}
