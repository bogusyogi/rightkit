// Google Tasks REST client. Builds on the shared `google` client for auth,
// retry, and backoff — this module only knows Tasks API endpoints and how to
// assemble a flat Vec<TaskItem> across all task lists.
//
// v1 surface (full RW):
//   list_task_lists  — GET  users/@me/lists
//   list_tasks       — GET  lists/{l}/tasks (all lists, enriched with list_id)
//   create_task      — POST lists/@default/tasks  (legacy; kept for compat)
//   create_task_in   — POST lists/{l}/tasks        (explicit list)
//   set_task_status  — PATCH lists/{l}/tasks/{t}  (complete / uncomplete)
//   update_task      — PATCH lists/{l}/tasks/{t}  (title/notes/due)
//   delete_task      — DELETE lists/{l}/tasks/{t}

use crate::google::{Google, GoogleError};
use serde::Deserialize;

const LISTS_ENDPOINT: &str = "https://tasks.googleapis.com/tasks/v1/users/@me/lists";
const TASKS_ENDPOINT_PREFIX: &str = "https://tasks.googleapis.com/tasks/v1/lists";

// ── types ──────────────────────────────────────────────────────────────────

/// Metadata for a task list returned to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskListInfo {
    pub id: String,
    pub title: String,
}

/// A single task item, enriched with both parent-list id and title.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// RFC3339 date string (date-only; Tasks ignores time). Empty when unset.
    pub due: String,
    /// "needsAction" | "completed"
    pub status: String,
    /// The id of the task list this task belongs to.
    pub list_id: String,
    /// Human-readable list title (convenience for display).
    pub list_title: String,
}

// ── internal deserialization types ────────────────────────────────────────

#[derive(Deserialize)]
struct TaskListRef {
    id: String,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct TaskListsResp {
    #[serde(default)]
    items: Vec<TaskListRef>,
}

#[derive(Deserialize)]
struct RawTask {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    due: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct TasksResp {
    #[serde(default)]
    items: Vec<RawTask>,
}

// ── helpers ───────────────────────────────────────────────────────────────

fn tasks_url(list_id: &str) -> String {
    format!("{}/{}/tasks", TASKS_ENDPOINT_PREFIX, list_id)
}

fn task_url(list_id: &str, task_id: &str) -> String {
    format!("{}/{}/tasks/{}", TASKS_ENDPOINT_PREFIX, list_id, task_id)
}

// ── read functions ────────────────────────────────────────────────────────

/// Fetch all task lists for one account (id + title only).
pub async fn list_task_lists(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<TaskListInfo>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let resp: TaskListsResp = g
        .get_json(LISTS_ENDPOINT, &[])
        .await
        .map_err(String::from)?;

    Ok(resp
        .items
        .into_iter()
        .map(|l| TaskListInfo {
            id: l.id,
            title: l.title,
        })
        .collect())
}

/// Fetch all incomplete tasks across every task list for one account.
/// Returns at most 100 TaskItems total (truncated after concatenation).
/// Each item carries both `list_id` and `list_title`.
pub async fn list_tasks(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    show_completed: bool,
) -> Result<Vec<TaskItem>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    // 1. Fetch all task lists.
    let lists_resp: TaskListsResp = g
        .get_json(LISTS_ENDPOINT, &[])
        .await
        .map_err(String::from)?;

    // 2. For each list fetch its tasks; stop once we have 100 total. Completed
    //    tasks are "hidden" in the Tasks API, so showHidden must accompany
    //    showCompleted to actually surface them.
    let mut out: Vec<TaskItem> = Vec::new();
    for list in lists_resp.items {
        if out.len() >= 100 {
            break;
        }
        let url = tasks_url(&list.id);
        let query = [
            ("showCompleted", show_completed.to_string()),
            ("showHidden", show_completed.to_string()),
            ("maxResults", "100".to_string()),
        ];
        let tasks_resp: TasksResp = match g.get_json(&url, &query).await {
            Ok(r) => r,
            Err(GoogleError::Http { status: 404, .. }) => continue,
            Err(e) => return Err(String::from(e)),
        };
        for t in tasks_resp.items {
            out.push(TaskItem {
                id: t.id,
                title: t.title,
                notes: t.notes,
                due: t.due,
                status: t.status,
                list_id: list.id.clone(),
                list_title: list.title.clone(),
            });
            if out.len() >= 100 {
                break;
            }
        }
    }

    out.truncate(100);
    Ok(out)
}

// ── write functions ───────────────────────────────────────────────────────

/// Create a new task in the default task list (legacy compat — kept for existing callers).
pub async fn create_task(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    title: &str,
    notes: &str,
) -> Result<(), String> {
    create_task_in(
        client_id,
        client_secret,
        refresh_token,
        "@default",
        title,
        notes,
        "",
    )
    .await
}

/// Create a new task in an explicit task list.
/// `due` must be RFC3339 (date-only, e.g. "2026-06-01T00:00:00.000Z") or "".
/// The Tasks API ignores the time component — only the date matters.
pub async fn create_task_in(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
    title: &str,
    notes: &str,
    due: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let url = tasks_url(list_id);

    let body = if due.is_empty() {
        serde_json::json!({ "title": title, "notes": notes })
    } else {
        serde_json::json!({ "title": title, "notes": notes, "due": due })
    };

    // POST returns the created task resource; we discard it.
    g.post_no_content(&url, &body).await.map_err(String::from)
}

/// Mark a task completed or restore it to needsAction.
pub async fn set_task_status(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
    task_id: &str,
    completed: bool,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let url = task_url(list_id, task_id);
    let status = if completed {
        "completed"
    } else {
        "needsAction"
    };
    let body = serde_json::json!({ "status": status });

    // PATCH returns the updated task; discard.
    let _: serde_json::Value = g.patch_json(&url, &body).await.map_err(String::from)?;

    Ok(())
}

/// Update a task's title, notes, and/or due date.
/// Pass "" for `due` to leave the due date unchanged (the field is simply omitted).
/// To clear an existing due date, pass an explicit null — but the Tasks API
/// only supports clearing via a full PUT; for now, omit means "leave as-is".
/// Not yet wired to a Tauri command (Tasks edit UI pending); covered by
/// `db/tests.rs`.
#[allow(dead_code)]
pub async fn update_task(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
    task_id: &str,
    title: &str,
    notes: &str,
    due: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let url = task_url(list_id, task_id);

    let body = if due.is_empty() {
        serde_json::json!({ "title": title, "notes": notes })
    } else {
        serde_json::json!({ "title": title, "notes": notes, "due": due })
    };

    let _: serde_json::Value = g.patch_json(&url, &body).await.map_err(String::from)?;

    Ok(())
}

/// Permanently delete a task from a list.
pub async fn delete_task(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
    task_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let url = task_url(list_id, task_id);
    g.delete_no_content(&url).await.map_err(String::from)
}

// ── task-list management ────────────────────────────────────────────────────

/// Create a new task list. Returns its id + title.
pub async fn create_task_list(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    title: &str,
) -> Result<TaskListInfo, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let body = serde_json::json!({ "title": title });
    let l: TaskListRef = g
        .post_json(LISTS_ENDPOINT, &body)
        .await
        .map_err(String::from)?;
    Ok(TaskListInfo {
        id: l.id,
        title: l.title,
    })
}

/// Rename a task list.
pub async fn rename_task_list(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
    title: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{}/{}", LISTS_ENDPOINT, list_id);
    let body = serde_json::json!({ "title": title });
    let _: serde_json::Value = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(())
}

/// Delete a task list (and its tasks). Irreversible on Google's side.
pub async fn delete_task_list(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{}/{}", LISTS_ENDPOINT, list_id);
    g.delete_no_content(&url).await.map_err(String::from)
}

/// Clear all completed tasks from a list (Tasks API `lists/{l}/clear`).
pub async fn clear_completed(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    list_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{}/{}/clear", TASKS_ENDPOINT_PREFIX, list_id);
    g.post_no_content(&url, &serde_json::json!({}))
        .await
        .map_err(String::from)
}
