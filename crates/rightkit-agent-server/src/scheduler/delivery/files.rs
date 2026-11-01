fn delivery_message_content(source_artifact: Option<&Path>, fallback_content: &str) -> String {
    source_artifact
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|content| !content.trim().is_empty())
        .unwrap_or_else(|| fallback_content.to_string())
}

fn deliver_file_artifact(
    root: &Path,
    entry: &ScheduleEntry,
    target: &DeliveryTarget,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> Result<(), String> {
    let destination = delivery_file_path(root, entry, target.target.as_deref())?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create delivery dir '{}': {e}", parent.display()))?;
    }
    if let Some(source) = source_artifact {
        std::fs::copy(source, &destination).map_err(|e| {
            format!(
                "failed to copy '{}' to '{}': {e}",
                source.display(),
                destination.display()
            )
        })?;
    } else {
        std::fs::write(&destination, fallback_content)
            .map_err(|e| format!("failed to write '{}': {e}", destination.display()))?;
    }
    Ok(())
}

fn delivery_file_path(
    root: &Path,
    entry: &ScheduleEntry,
    target: Option<&str>,
) -> Result<PathBuf, String> {
    let path = target
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join(".coderight")
                .join("schedule-deliveries")
                .join(format!(
                    "{}-{}.txt",
                    sanitize_id(&entry.id),
                    current_epoch_millis()
                ))
        });
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let normalized_root = normalize_lexical(root);
    let normalized_path = normalize_lexical(&path);
    if !normalized_path.starts_with(&normalized_root) {
        return Err("file_artifact target must stay inside the workspace root".to_string());
    }
    Ok(path)
}

fn remote_deliveries_path(root: &Path) -> PathBuf {
    root.join(".coderight").join("remote-deliveries.json")
}

pub(super) fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod targets_tests {
    use super::*;

    #[test]
    fn desktop_notification_body_without_artifact_uses_fallback_only() {
        let body = desktop_notification_body(None, "fallback text");
        assert_eq!(body, delivery_message_content(None, "fallback text"));
        assert!(!body.contains("Artifact:"));
    }

    #[test]
    fn desktop_notification_body_with_artifact_prefixes_path() {
        let path = Path::new("/tmp/out.txt");
        let body = desktop_notification_body(Some(path), "fallback text");
        assert!(body.starts_with("Artifact: /tmp/out.txt\n\n"));
        assert!(body.ends_with(&delivery_message_content(Some(path), "fallback text")));
    }
}
