//! The application seam of the native pill; defined in `rightkit_overlay::pill::actions` so the
//! macOS bridge shares it.

pub use rightkit_overlay::pill::actions::{
    actions, install_actions, log_message, replace_file_atomically, PillActions,
};
