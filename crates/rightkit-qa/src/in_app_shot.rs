//! QA-only argument readers & current-process Windows window screenshots.
//!
//! Enable `in-app-shot` only from an app's opt-in QA feature. Host inputs own window
//! labels, title aliases, output defaults & delay. Capture never accepts another process
//! or a raw native window handle; only windows belonging to this process are searched.

mod capture;
pub mod command_args;
mod options;

pub use capture::capture;
pub use options::{
    default_path, options_from_env, parse_args, parse_delay, parse_window, ShotHost, ShotOptions,
    ShotWindowSpec,
};
