// Shared by `include!` from the e2e support modules.
//
// The build runner (RightKit) hands test processes a stripped environment: no `PATH` entries for
// Git/Node and no `ProgramFiles*` variables. A spawned `coderight` (or its daemon) inherits that,
// so confined children cannot resolve those tools and the AppContainer launcher cannot recognise
// `C:\Program Files\Git\cmd` as an already-readable image directory. The product must not
// hardcode host tool locations, so the test harness restores the host facts a developer shell has.

/// Directory that holds a usable `git` executable, or `None` when Git is not installed.
#[allow(dead_code)]
pub fn host_git_dir() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join(exe).is_file() {
                return Some(dir);
            }
        }
    }
    if cfg!(windows) {
        let mut roots: Vec<std::path::PathBuf> =
            ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .iter()
                .filter_map(std::env::var_os)
                .map(std::path::PathBuf::from)
                .collect();
        roots.push(r"C:\Program Files".into());
        roots.push(r"C:\Program Files (x86)".into());
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for root in roots {
            candidates.push(root.join("Git").join("cmd"));
            candidates.push(root.join("Git").join("bin"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                std::path::PathBuf::from(local)
                    .join("Programs")
                    .join("Git")
                    .join("cmd"),
            );
        }
        return candidates.into_iter().find(|dir| dir.join(exe).is_file());
    }
    ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|dir| dir.join(exe).is_file())
}

/// Program to launch for the real Git CLI: the resolved absolute path, else plain `git`.
#[allow(dead_code)]
pub fn host_git_program() -> std::path::PathBuf {
    host_git_dir()
        .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
        .unwrap_or_else(|| "git".into())
}

/// Directory that holds the host Node runtime, or `None` when Node is unavailable.
#[allow(dead_code)]
pub fn host_node_dir() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join(exe).is_file() {
                return Some(dir);
            }
        }
    }
    if cfg!(windows) {
        let mut candidates = Vec::new();
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            candidates.push(
                std::path::PathBuf::from(profile)
                    .join(".cache")
                    .join("codex-runtimes")
                    .join("codex-primary-runtime")
                    .join("dependencies")
                    .join("node")
                    .join("bin"),
            );
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                std::path::PathBuf::from(local)
                    .join("Programs")
                    .join("nodejs"),
            );
        }
        for key in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(key) {
                candidates.push(std::path::PathBuf::from(root).join("nodejs"));
            }
        }
        candidates.push(r"C:\Program Files\nodejs".into());
        return candidates.into_iter().find(|dir| dir.join(exe).is_file());
    }
    None
}

/// Give a spawned child Git/Node on `PATH`, `ProgramFiles*` roots, and Windows `PATHEXT`.
#[allow(dead_code)]
pub fn restore_host_env(command: &mut std::process::Command) {
    let mut dirs = Vec::new();
    if let Some(node_dir) = host_node_dir() {
        dirs.push(node_dir);
    }
    if let Some(git_dir) = host_git_dir() {
        if !dirs.contains(&git_dir) {
            dirs.push(git_dir);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    if !dirs.is_empty() {
        if let Ok(joined) = std::env::join_paths(dirs) {
            command.env("PATH", joined);
        }
    }
    if cfg!(windows) {
        // SHGetFolderPath verifies that expanded known-folder paths exist.
        // Our isolated USERPROFILE otherwise lacks AppData, so Chrome cannot
        // resolve its default profile for the non-default-profile CDP check
        // even though browser.rs supplies a separate --user-data-dir.
        let isolated_profile = command.get_envs().find_map(|(key, value)| {
            if key.to_string_lossy().eq_ignore_ascii_case("USERPROFILE") {
                value.map(std::path::PathBuf::from)
            } else {
                None
            }
        });
        if let Some(profile) = isolated_profile {
            for (key, relative) in [
                ("LOCALAPPDATA", "AppData/Local"),
                ("APPDATA", "AppData/Roaming"),
            ] {
                let directory = profile.join(relative);
                std::fs::create_dir_all(&directory).expect("create isolated Windows known folder");
                command.env(key, directory);
            }
        }
        for (key, default) in [
            ("ProgramFiles", r"C:\Program Files"),
            ("ProgramW6432", r"C:\Program Files"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ] {
            let present = std::env::var_os(key).is_some_and(|v| !v.is_empty());
            if !present && std::path::Path::new(default).is_dir() {
                command.env(key, default);
            }
        }
    }
    if cfg!(windows) && !std::env::var_os("PATHEXT").is_some_and(|value| !value.is_empty()) {
        command.env("PATHEXT", ".COM;.EXE;.BAT;.CMD");
    }
}
