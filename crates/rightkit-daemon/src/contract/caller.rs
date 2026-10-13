use crate::peer::PeerIdentity;
use std::collections::BTreeSet;

/// A capability a device holds (from enrolment). A plain label compared for equality.
pub type Scope = String;

/// Who is making a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A local app on this machine. `app` is the label declared in `hello` (a claim, an allowlist
    /// key); `peer` is what the OS verified about the other end.
    LocalApp { app: String, peer: PeerIdentity },
    /// A remote device identified by its id, with the scopes it was enrolled with.
    Device { id: String, scopes: BTreeSet<Scope> },
}

impl Caller {
    pub fn local_app(app: impl Into<String>, peer: PeerIdentity) -> Self {
        Caller::LocalApp {
            app: app.into(),
            peer,
        }
    }

    pub fn device<I, S>(id: impl Into<String>, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        Caller::Device {
            id: id.into(),
            scopes: scopes.into_iter().map(Into::into).collect(),
        }
    }

    /// The OS-verified peer, for a local caller only.
    pub fn peer(&self) -> Option<&PeerIdentity> {
        match self {
            Caller::LocalApp { peer, .. } => Some(peer),
            Caller::Device { .. } => None,
        }
    }

    /// The app label for a local caller, the device id for a device.
    pub fn label(&self) -> &str {
        match self {
            Caller::LocalApp { app, .. } => app,
            Caller::Device { id, .. } => id,
        }
    }
}
