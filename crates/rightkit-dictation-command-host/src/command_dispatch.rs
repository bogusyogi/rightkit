// Typed standalone-command dispatcher — module hub.
//
// Was 4 flat include!()'d sections (~49KB, zero tests); now real modules with
// the exact same public surface (`command_dispatch::dispatch`,
// `command_dispatch::dispatch_with_last_text`, `DispatchError`,
// `DispatchOutcome`, `DispatchResult`) plus unit tests on the pure logic.
mod entry;
mod keys;
mod mouse;
mod power;
mod screenshot;
mod transforms;

pub use entry::{
    dispatch, dispatch_with_last_text, DispatchError, DispatchOutcome, DispatchResult,
};
