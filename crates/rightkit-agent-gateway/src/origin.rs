//! Which door a call came in by.

/// Which door a call came in by. Set by the process that receives the call, never by the
/// caller's own request, so a request cannot claim to be local.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A caller on this computer: the app's own CLI, its engine, or a paired local agent.
    #[default]
    Local,
    /// A remote door: a model service reaching this computer through a tunnel or relay. An app
    /// usually needs its own switch on before it serves this door at all.
    Gateway,
}

impl Origin {
    /// The word an audit row records.
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Gateway => "gateway",
        }
    }
}
