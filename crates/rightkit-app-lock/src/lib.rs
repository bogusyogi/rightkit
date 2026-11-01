//! Application lock: the lock state types, the fail-closed preference rules, the OS user
//! verification (Windows Hello, Touch ID) and the shell-side unlock flow.
//!
//! The crate holds no database and no UI-framework type. The host stores the "lock enabled"
//! preference wherever it keeps secrets and feeds the raw read to [`lock_preference_from_read`];
//! it passes the product name and the engine method names it uses to [`flow::Methods`].

pub mod flow;
mod preference;
mod startup;
mod state;
mod verify;

pub use preference::{enabled_from_secret, lock_preference_from_read, LockPreference};
pub use startup::{startup_runtime, startup_verification};
pub use state::{AppLockRuntime, AppLockState, AppLockStatus};
pub use verify::{native_available, verify_user, verify_user_for_window};

#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod tests;
