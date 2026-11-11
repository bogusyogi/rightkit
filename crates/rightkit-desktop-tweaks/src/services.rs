//! What the shortcut layer needs from the application: logging. The application installs one
//! [`Services`]; until it does, nothing is logged. Which shortcuts are on is given to
//! [`crate::keys::apply`].

use std::sync::OnceLock;

/// Services the embedding application provides.
pub trait Services: Send + Sync + 'static {
    /// Records an informational event.
    fn log_info(&self, _event: &str, _fields: &[(&str, &str)]) {}
}

static SERVICES: OnceLock<Box<dyn Services>> = OnceLock::new();

/// Installs the services. Returns `false` when some were already installed.
pub fn install(services: Box<dyn Services>) -> bool {
    SERVICES.set(services).is_ok()
}

pub(crate) fn log_info(event: &str, fields: &[(&str, &str)]) {
    if let Some(s) = SERVICES.get() {
        s.log_info(event, fields);
    }
}
