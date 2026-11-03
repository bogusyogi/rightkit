use crate::{Capabilities, Delivered, Error, Notification, Permission};

fn unsupported() -> Error {
    Error::Unsupported("native notifications are implemented for macOS, Windows and Linux".into())
}

pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        actions: false,
        activation: false,
        replace: false,
        close: false,
        permission: false,
        badge: false,
        delivered: false,
    }
}

pub(crate) fn permission() -> Result<Permission, Error> {
    Ok(Permission::Unsupported)
}

pub(crate) fn request_permission() -> Result<Permission, Error> {
    Ok(Permission::Unsupported)
}

pub(crate) fn activation_registered() {}

pub(crate) fn post(_: &str, _: &str, _: &Notification) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn close(_: &str, _: &str) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn set_badge(_: &str, _: u32) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn close_all(_: &str) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn delivered(_: &str) -> Result<Vec<Delivered>, Error> {
    Ok(Vec::new())
}
