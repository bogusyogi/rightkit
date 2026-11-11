fn load_workspace_config_only(
    workspace: &FsPath,
) -> Result<
    rightkit_agent_config::HostConfig,
    rightkit_agent_config::ConfigError,
> {
    let path = workspace_config_path(workspace);
    match fs::read_to_string(&path) {
        Ok(content) => rightkit_agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_agent_config::ConfigError::Io(
            err.to_string(),
        )),
    }
}

fn config_status(q: &WorkspaceQuery) -> ConfigStatus {
    let files = config_files(&workspace_path(q))
        .into_iter()
        .map(|(scope, path)| config_file_status(scope, path))
        .collect::<Vec<_>>();
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for file in &files {
        hasher.update(file.scope.as_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(&[file.exists as u8]);
        hasher.update(&file.modified_ms.unwrap_or_default().to_le_bytes());
        hasher.update(&file.bytes.unwrap_or_default().to_le_bytes());
        if let Some(sha) = &file.sha256 {
            hasher.update(sha.as_bytes());
        }
    }
    ConfigStatus {
        workspace_root: workspace_root_string(q),
        fingerprint: format!("sha256:{}", hasher.finalize()),
        loaded_at: now_iso(),
        files,
    }
}

fn config_file_status(scope: &str, path: PathBuf) -> ConfigFileStatus {
    let path_string = path.display().to_string();
    match fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis());
            let sha256 = fs::read(&path).ok().map(|bytes| {
                format!(
                    "sha256:{}",
                    rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&bytes))
                )
            });
            ConfigFileStatus {
                scope: scope.to_string(),
                path: path_string,
                exists: true,
                modified_ms,
                bytes: Some(meta.len()),
                sha256,
            }
        }
        _ => ConfigFileStatus {
            scope: scope.to_string(),
            path: path_string,
            exists: false,
            modified_ms: None,
            bytes: None,
            sha256: None,
        },
    }
}

fn config_files(workspace: &FsPath) -> Vec<(&'static str, PathBuf)> {
    let mut files = Vec::new();
    if let Some(state) = config_state_dir() {
        files.push(("user", state.join("config.toml")));
    }
    files.push(("workspace", workspace_config_path(workspace)));
    files
}

/// The file `ConfigLoader::update_workspace_from` and `save_workspace` write and
/// `load_files_for_workspace` reads (`<workspace>/.rightkit/config.toml`).
fn workspace_config_path(workspace: &FsPath) -> PathBuf {
    workspace.join(".rightkit").join("config.toml")
}

fn workspace_path(q: &WorkspaceQuery) -> PathBuf {
    (!q.workspace_root.trim().is_empty())
        .then(|| PathBuf::from(q.workspace_root.trim()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn workspace_root_string(q: &WorkspaceQuery) -> String {
    workspace_path(q).display().to_string()
}

fn config_state_dir() -> Option<PathBuf> {
    rightkit_agent_config::state_root()
}

/// Read-modify-write of the workspace config under its cross-process lock, on a blocking thread: the
/// lock wait can last five seconds and must not park an async worker. A read failure is a bad request
/// (the file is malformed), a lock or write failure a server error.
async fn update_workspace_config_blocking(
    workspace: std::path::PathBuf,
    edit: impl FnOnce(&mut rightkit_agent_config::HostConfig) + Send + 'static,
) -> Result<(), StatusCode> {
    let outcome = tokio::task::spawn_blocking(move || {
        let read_failed = std::cell::Cell::new(false);
        let result = rightkit_agent_config::ConfigLoader::new()
            .update_workspace_from(
                &workspace,
                || {
                    load_workspace_config_only(&workspace).inspect_err(|_| {
                        read_failed.set(true);
                    })
                },
                edit,
            );
        (result.map(|_| ()), read_failed.get())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match outcome {
        (Ok(()), _) => Ok(()),
        (Err(_), true) => Err(StatusCode::BAD_REQUEST),
        (Err(_), false) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Puts a value back into a shared slot when dropped, unless disarmed. It lives inside the blocking task,
/// so it runs on success-less exits of every kind: an error return and a panic (unwind).
struct RestoreOnDrop<T> {
    slot: std::sync::Arc<std::sync::Mutex<T>>,
    previous: Option<T>,
}

impl<T> RestoreOnDrop<T> {
    fn disarm(&mut self) {
        self.previous = None;
    }
}

impl<T> Drop for RestoreOnDrop<T> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            *lock_recover(&self.slot, "state.settings") = previous;
        }
    }
}

/// Runs `persist` on a blocking thread and, when it fails or panics, puts `previous` back into `slot`
/// IN THAT SAME TASK. A blocking task is not cancelled when the request that spawned it is dropped
/// (client disconnect, timeout), so the restore cannot be skipped the way a restore written after the
/// `.await` in the handler can. The error text names a stopped writer when the task panicked.
async fn persist_or_restore<T, E>(
    slot: std::sync::Arc<std::sync::Mutex<T>>,
    previous: T,
    persist: impl FnOnce() -> Result<(), E> + Send + 'static,
) -> Result<(), String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut restore = RestoreOnDrop {
            slot,
            previous: Some(previous),
        };
        let result = persist().map_err(|e| e.to_string());
        if result.is_ok() {
            restore.disarm();
        }
        result
    })
    .await
    .unwrap_or_else(|e| Err(format!("config writer stopped: {e}")))
}
