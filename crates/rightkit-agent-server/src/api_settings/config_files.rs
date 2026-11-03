fn load_workspace_config_only(
    workspace: &FsPath,
) -> Result<
    rightkit_app_settings::agent_config::HostConfig,
    rightkit_app_settings::agent_config::ConfigError,
> {
    let path = workspace_config_path(workspace);
    match fs::read_to_string(&path) {
        Ok(content) => rightkit_app_settings::agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_app_settings::agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_app_settings::agent_config::ConfigError::Io(
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
    files.push((
        "workspace",
        workspace.join(".coderight").join("config.toml"),
    ));
    files
}

fn workspace_config_path(workspace: &FsPath) -> PathBuf {
    workspace.join(".coderight").join("config.toml")
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
    rightkit_app_settings::agent_config::state_root()
}

/// Read-modify-write of the workspace config under its cross-process lock, on a blocking thread: the
/// lock wait can last five seconds and must not park an async worker. A read failure is a bad request
/// (the file is malformed), a lock or write failure a server error.
async fn update_workspace_config_blocking(
    workspace: std::path::PathBuf,
    edit: impl FnOnce(&mut rightkit_app_settings::agent_config::HostConfig) + Send + 'static,
) -> Result<(), StatusCode> {
    let outcome = tokio::task::spawn_blocking(move || {
        let read_failed = std::cell::Cell::new(false);
        let result = rightkit_app_settings::agent_config::ConfigLoader::new()
            .update_workspace_from(
                &workspace,
                || {
                    load_workspace_config_only(&workspace).map_err(|err| {
                        read_failed.set(true);
                        err
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

#[cfg(test)]
mod workspace_lock_tests {
    use super::*;

    /// Eight concurrent handler-side writers each add a toolset; none is lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_workspace_saves_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let tasks: Vec<_> = (0..8)
            .map(|n| {
                let workspace = workspace.clone();
                tokio::spawn(async move {
                    update_workspace_config_blocking(workspace, move |config| {
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        config.toolsets.insert(
                            format!("t{n}"),
                            rightkit_app_settings::agent_config::ToolsetConfig::default(),
                        );
                    })
                    .await
                })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        let saved = load_workspace_config_only(&workspace).unwrap();
        for n in 0..8 {
            assert!(
                saved.toolsets.contains_key(&format!("t{n}")),
                "t{n} was lost"
            );
        }
    }

    /// While one writer holds the lock the async runtime keeps running: the wait is on a blocking
    /// thread, so a timer on a one-thread runtime still fires long before the writer is let in.
    #[tokio::test(flavor = "current_thread")]
    async fn a_waiting_writer_does_not_park_the_async_worker() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let holder_workspace = workspace.clone();
        let holder = std::thread::spawn(move || {
            rightkit_app_settings::agent_config::ConfigLoader::new()
                .update_workspace_from(
                    &holder_workspace,
                    || {
                        held_tx.send(()).unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(700));
                        Ok(rightkit_app_settings::agent_config::HostConfig::default())
                    },
                    |_| {},
                )
                .unwrap();
        });
        held_rx.recv().unwrap();
        let waiting = tokio::spawn(update_workspace_config_blocking(
            workspace.clone(),
            |config| {
                config.toolsets.insert(
                    "late".into(),
                    rightkit_app_settings::agent_config::ToolsetConfig::default(),
                );
            },
        ));
        let started = std::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(600),
            "the timer was delayed by the lock wait: {:?}",
            started.elapsed()
        );
        assert!(
            waiting.await.unwrap().is_ok(),
            "the waiter gets the lock once the holder is done"
        );
        holder.join().unwrap();
        assert!(load_workspace_config_only(&workspace)
            .unwrap()
            .toolsets
            .contains_key("late"));
    }

    /// A failed write puts the previous value back, inside the task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_write_restores_the_previous_value() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(2));
        let result = persist_or_restore(slot.clone(), 1, || Err::<(), _>("disk full")).await;
        assert_eq!(result, Err("disk full".to_string()));
        assert_eq!(*slot.lock().unwrap(), 1);
    }

    /// A write that succeeds keeps the new value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_successful_write_keeps_the_new_value() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(2));
        assert_eq!(
            persist_or_restore(slot.clone(), 1, || Ok::<(), String>(())).await,
            Ok(())
        );
        assert_eq!(*slot.lock().unwrap(), 2);
    }

    /// A panic in the write (even while the slot is locked and poisoned) still restores the value and
    /// reports a stopped writer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_write_restores_the_previous_value() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(2));
        let inner = slot.clone();
        let result = persist_or_restore(slot.clone(), 1, move || -> Result<(), String> {
            let _held = inner.lock().unwrap();
            panic!("edit blew up");
        })
        .await;
        assert!(result.unwrap_err().contains("config writer stopped"));
        assert_eq!(*lock_recover(&slot, "test"), 1);
    }

    /// The case that was lost before: the request is cancelled (the handler future dropped) while the
    /// write is still waiting, and the write then fails. The restore still happens, in the task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_the_request_during_the_write_still_restores_when_the_write_fails() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(2));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let handler = tokio::spawn(persist_or_restore(slot.clone(), 1, move || {
            started_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            Err::<(), _>("lock wait timed out")
        }));
        started_rx.recv().unwrap();
        handler.abort(); // the client went away while the writer was still waiting
        assert!(handler.await.unwrap_err().is_cancelled());
        for _ in 0..100 {
            if *slot.lock().unwrap() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the in-memory value was not restored after the cancelled request's write failed");
    }
}
