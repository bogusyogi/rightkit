//! Console-window suppression for every spawned process (standing product
//! rule: **zero visible Windows console windows for background automation**).
//!
//! The suppression itself is `rightkit_child_process::NoWindow`; commands are
//! built with `rightkit_child_process::command_std` (blocking) and
//! `rightkit_child_process::tokio_support::command` (async), already hidden.
//! This module only re-exports the trait and the flag for the call sites that
//! name them through `crate::no_window`.
//!
//! The one sanctioned exception to the no-window rule is an
//! operator-requested *visible* subagent, which opts in explicitly via
//! `visible_subagent_window` and uses `CREATE_NEW_CONSOLE`.

pub use rightkit_child_process::{NoWindow, CREATE_NO_WINDOW};
