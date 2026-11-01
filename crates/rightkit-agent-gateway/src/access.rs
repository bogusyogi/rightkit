//! Connection access the gate runs on. The host implements it over its gated
//! reader/writer helpers, so a locked app yields the locked error; tests implement it over an
//! in-memory connection.

use rightkit_db::rusqlite::Connection;

/// The text a locked host must return as its error from [`ConnAccess::read`] and
/// [`ConnAccess::write`]; the gate maps exactly this text to the `app_locked` refusal.
pub const LOCKED_MESSAGE: &str = "The app is locked.";

/// How the gate reaches the host's database.
pub trait ConnAccess {
    /// True when the app lock forbids database access right now.
    fn is_locked(&self) -> bool;
    /// Run `f` on a read connection.
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String>;
    /// Run `f` on the writer connection.
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String>;
}
