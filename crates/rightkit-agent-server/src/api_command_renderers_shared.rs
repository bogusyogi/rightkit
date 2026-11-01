pub(crate) fn command_workspace_arg(args: &[String]) -> PathBuf {
    let joined = args.join(" ");
    if joined.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(joined.trim())
    }
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn command_err(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}
