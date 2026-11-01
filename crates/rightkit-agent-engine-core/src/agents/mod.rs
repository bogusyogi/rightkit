//! Built-in agent definitions — each agent has a baked-in system prompt and
//! a role string. The binary wires the right prompt into a `SessionEngine`
//! at session-creation time based on the requested role.

pub mod website_designer;

pub use website_designer::{is_website_task, WEBSITE_DESIGNER_SYSTEM_PROMPT};
