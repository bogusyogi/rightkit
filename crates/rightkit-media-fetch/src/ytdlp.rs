/// Build the yt-dlp argv contract from one executable plus optional leading
/// args, so an app's override variables mean the same thing everywhere it builds the command.
pub fn ytdlp_cmd_from_env_parts(
    command_env: Option<String>,
    args_env: Option<String>,
) -> Vec<String> {
    let mut cmd = command_env
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| vec![s])
        .unwrap_or_else(|| vec!["yt-dlp".into()]);

    if let Some(extra) = args_env
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        cmd.extend(split_shell_words(&extra));
    }
    cmd
}

fn split_shell_words(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}
