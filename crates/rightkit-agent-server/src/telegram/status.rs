#[derive(Clone, Debug, Default)]
pub struct TelegramChatCursorStatus {
    pub chat_label: String,
    pub session_id: String,
    pub last_pushed_seq: i64,
}

#[derive(Clone, Debug, Default)]
pub struct TelegramRuntimeStatus {
    pub started_at: Option<String>,
    pub last_poll_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub next_retry_ms: Option<u64>,
    pub update_offset: i64,
    pub active_chat_count: usize,
    pub chat_cursors: Vec<TelegramChatCursorStatus>,
}

pub(crate) type TelegramRuntimeStatusStore = Arc<Mutex<TelegramRuntimeStatus>>;

pub(crate) fn new_telegram_runtime_status_store() -> TelegramRuntimeStatusStore {
    Arc::new(Mutex::new(TelegramRuntimeStatus::default()))
}

fn status_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

fn update_status(status: &TelegramRuntimeStatusStore, f: impl FnOnce(&mut TelegramRuntimeStatus)) {
    if let Ok(mut guard) = status.lock() {
        f(&mut guard);
    }
}

fn update_chat_cursors(
    status: &TelegramRuntimeStatusStore,
    chat_event_seq: &HashMap<i64, i64>,
    chat_event_sessions: &HashMap<i64, String>,
) {
    let mut rows = chat_event_seq
        .iter()
        .map(|(chat_id, seq)| (*chat_id, *seq))
        .collect::<Vec<_>>();
    rows.sort_by_key(|(chat_id, _)| *chat_id);
    let chat_cursors = rows
        .into_iter()
        .enumerate()
        .map(|(index, (chat_id, seq))| TelegramChatCursorStatus {
            chat_label: format!("chat-{}", index + 1),
            session_id: chat_event_sessions
                .get(&chat_id)
                .cloned()
                .unwrap_or_else(|| "default".to_string()),
            last_pushed_seq: seq,
        })
        .collect::<Vec<_>>();
    update_status(status, |runtime| {
        runtime.active_chat_count = chat_cursors.len();
        runtime.chat_cursors = chat_cursors;
    });
}

#[derive(Clone, Debug)]
struct TelegramPollBackoff {
    base_delay: Duration,
    max_delay: Duration,
    failures: u32,
}

impl TelegramPollBackoff {
    fn new(base_delay: Duration) -> Self {
        Self {
            base_delay,
            max_delay: Duration::from_secs(60),
            failures: 0,
        }
    }

    fn record_failure(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let multiplier = 1u32
            .checked_shl(self.failures.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.base_delay
            .saturating_mul(multiplier)
            .min(self.max_delay)
    }

    fn reset(&mut self) {
        self.failures = 0;
    }

    fn failures(&self) -> u32 {
        self.failures
    }
}
