//! Engine command modules for transcript-driven editing (feature `engine-commands`): `autoedit.*`,
//! `autonomy.*`, `rules.*`, `style.measure` and `mic.align`. Each takes the JSON params of its method
//! and the engine [`Env`](rightkit_engine_host::env::Env), and returns the JSON result. The host
//! engine routes method names to them.

pub mod autoedit;
pub mod autonomy;
pub mod mic;
pub mod rules;
pub mod style;
