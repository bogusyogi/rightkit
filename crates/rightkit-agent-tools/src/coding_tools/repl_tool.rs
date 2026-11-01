// `repl` — evaluate snippets in a persistent language REPL session.
//
// Session construction and the `Tool` impl live in `session_tool`; the
// spawn/eval/reset command paths in `commands`; workspace-artifact
// diffing in `artifacts`; protocol frames, runtime probing, and helper
// scripts in `helpers`. Ordinary submodules — no shared include! scope.

mod artifacts;
mod commands;
mod helpers;
mod session_tool;
#[cfg(test)]
mod tests;

pub use session_tool::ReplTool;
