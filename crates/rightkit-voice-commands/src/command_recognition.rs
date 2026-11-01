mod catalog;
mod matching;
mod recognize;
#[cfg(test)]
mod tests;

pub use catalog::has_longer_direct_command_prefix;
pub use matching::{app_alias, app_launch_query, edit_distance, shortcut_query};
pub use recognize::{
    command_catalog, command_token_supported_on_current_platform, macos_command, recognize_command,
    CatalogEntry, MacCommand,
};
