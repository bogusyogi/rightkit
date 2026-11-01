//! A doctor framework for RightKit engines and command-line tools.
//!
//! An app keeps only what is specific to it (which checks to run, its identity and constants)
//! and implements [`command::DoctorHost`]; this crate supplies everything around it:
//!
//! - [`verdict`]: pass / warn / block checks, the rolled-up verdict, text rendering and the
//!   exit-code rule (0 ready, 1 warnings, 2 blocked), plus `--verbose` evidence
//!   ([`verdict::Dimension`]).
//! - [`row`]: the engine check row (`id`, `required`, `status`, `detail`) and the readiness
//!   roll-up engines publish.
//! - [`report`]: the engine doctor report envelope (identity, contract version, capabilities,
//!   readiness, paths, platform, dry-run flag).
//! - [`command`]: the `doctor` command for `rightkit-engine-host` registries.
//! - [`probe`]: neutral probes (tools on the search path and their versions, writable folders,
//!   bounded child processes, free ports, the system clipboard).
//! - [`clock`]: UTC timestamps and lifecycle lines for long-running doctors.
//! - [`host_dirs`]: agent-host configuration folders and small file helpers.
//!
//! ```
//! use rightkit_doctor::row::{readiness, Row};
//!
//! let rows = vec![Row::ok("data_dir", true, "writable"), Row::unavailable("gpu", false, "no gpu")];
//! assert_eq!(readiness(&rows)["status"], "ready");
//! ```

pub mod clock;
pub mod command;
pub mod host_dirs;
pub mod probe;
pub mod report;
pub mod row;
pub mod verdict;
