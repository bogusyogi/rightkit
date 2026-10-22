//! Android conventions: the log directory and a logcat mirror for `tracing`.
//!
//! The JSONL record stays the file written by `crate::TracingFileConfig`. Nothing here looks up
//! `HOME`, `XDG_*` or any environment variable: the caller injects the app's files directory
//! (`Context.getFilesDir()`, delivered by `rightkit-android` as `AppPaths::files_dir`).
//!
//! The logcat mirror (`LogcatLayer`, feature `tracing`) does not bind `liblog` itself. It hands
//! each event to a `LogcatSink`, and on Android the sink is `rightkit_android::logcat::write`,
//! which already owns the `__android_log_write` FFI and the debug-only switch. That keeps one
//! liblog binding in the suite and keeps logcat off unless the app enabled debug logging.

use std::path::{Path, PathBuf};

/// Subdirectory of the app files directory that holds the logs. Matches
/// `rightkit_android::AppPaths::logs_dir` (`<files_dir>/logs`).
pub const ANDROID_LOGS_SUBDIR: &str = "logs";

/// `<files_dir>/logs`, computed from the injected files directory with no environment lookup and
/// no filesystem access. An empty `files_dir` yields `None` (the caller never passed one).
pub fn android_logs_dir(files_dir: impl AsRef<Path>) -> Option<PathBuf> {
    let files_dir = files_dir.as_ref();
    if files_dir.as_os_str().is_empty() {
        return None;
    }
    Some(files_dir.join(ANDROID_LOGS_SUBDIR))
}

/// Android log priority for a mirrored line. Values are `android_LogPriority` and equal
/// `rightkit_android::LogLevel::priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogcatPriority {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogcatPriority {
    /// The `ANDROID_LOG_*` number.
    pub const fn android_value(self) -> i32 {
        match self {
            LogcatPriority::Verbose => 2,
            LogcatPriority::Debug => 3,
            LogcatPriority::Info => 4,
            LogcatPriority::Warn => 5,
            LogcatPriority::Error => 6,
        }
    }
}

/// logcat tags are limited to 23 characters on older Android releases.
pub const MAX_TAG_CHARS: usize = 23;

/// Cuts a tag to [`MAX_TAG_CHARS`] characters, falling back to `RightKit` when empty.
pub fn logcat_tag(tag: &str) -> String {
    let cut: String = tag
        .chars()
        .filter(|c| *c != '\0')
        .take(MAX_TAG_CHARS)
        .collect();
    if cut.is_empty() {
        "RightKit".to_string()
    } else {
        cut
    }
}

#[cfg(feature = "tracing")]
pub use layer::{priority_for_level, LogcatLayer, LogcatSink};

#[cfg(feature = "tracing")]
mod layer {
    use super::{logcat_tag, LogcatPriority};
    use std::fmt::{self, Write as _};
    use std::sync::Arc;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    /// Receives `(priority, tag, message)`. On Android pass a closure over
    /// `rightkit_android::logcat::write`; elsewhere any collector works (tests use one).
    pub type LogcatSink = Arc<dyn Fn(LogcatPriority, &str, &str) + Send + Sync>;

    /// Maps a `tracing` level to an Android priority (`TRACE` is verbose).
    pub fn priority_for_level(level: &Level) -> LogcatPriority {
        match *level {
            Level::TRACE => LogcatPriority::Verbose,
            Level::DEBUG => LogcatPriority::Debug,
            Level::INFO => LogcatPriority::Info,
            Level::WARN => LogcatPriority::Warn,
            Level::ERROR => LogcatPriority::Error,
        }
    }

    /// Mirrors events to a [`LogcatSink`] under one tag. Text goes through `scrub_text` first.
    /// Add it next to the file layer only when debug logging is on; it adds no filter of its own.
    pub struct LogcatLayer {
        tag: String,
        sink: LogcatSink,
    }

    impl LogcatLayer {
        pub fn new(tag: &str, sink: LogcatSink) -> Self {
            Self {
                tag: logcat_tag(tag),
                sink,
            }
        }
    }

    struct Line(String);

    impl Visit for Line {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.0, "{value:?}");
            } else {
                let _ = write!(self.0, " {}={value:?}", field.name());
            }
        }
    }

    impl<S: Subscriber> Layer<S> for LogcatLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut line = Line(String::new());
            event.record(&mut line);
            let text = crate::scrub_text(line.0.trim_start());
            (self.sink)(
                priority_for_level(event.metadata().level()),
                &self.tag,
                &text,
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Mutex;
        use tracing_subscriber::layer::SubscriberExt;

        #[test]
        fn tracing_levels_map_to_android_priorities() {
            let pairs = [
                (Level::TRACE, 2),
                (Level::DEBUG, 3),
                (Level::INFO, 4),
                (Level::WARN, 5),
                (Level::ERROR, 6),
            ];
            for (level, value) in pairs {
                assert_eq!(priority_for_level(&level).android_value(), value);
            }
        }

        #[test]
        fn events_reach_the_sink_with_tag_and_priority() {
            let seen: Arc<Mutex<Vec<(i32, String, String)>>> = Arc::default();
            let store = Arc::clone(&seen);
            let sink: LogcatSink = Arc::new(move |p, t, m| {
                store
                    .lock()
                    .unwrap()
                    .push((p.android_value(), t.into(), m.into()));
            });
            let subscriber =
                tracing_subscriber::registry().with(LogcatLayer::new("HeardRight", sink));
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!("model slow");
            });
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(
                seen[0],
                (5, "HeardRight".to_string(), "model slow".to_string())
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_dir_is_files_dir_logs_without_environment_lookup() {
        let dir = android_logs_dir("/data/user/0/com.example/files").unwrap();
        assert_eq!(
            dir,
            Path::new("/data/user/0/com.example/files").join("logs")
        );
        assert!(android_logs_dir("").is_none());
    }

    #[test]
    fn logs_subdir_matches_the_android_bridge_convention() {
        assert_eq!(ANDROID_LOGS_SUBDIR, "logs");
    }

    #[test]
    fn priority_values_match_android_log_priority() {
        let values = [
            LogcatPriority::Verbose,
            LogcatPriority::Debug,
            LogcatPriority::Info,
            LogcatPriority::Warn,
            LogcatPriority::Error,
        ]
        .map(LogcatPriority::android_value);
        assert_eq!(values, [2, 3, 4, 5, 6]);
    }

    #[test]
    fn tags_are_cut_and_defaulted() {
        assert_eq!(logcat_tag(&"é".repeat(40)).chars().count(), MAX_TAG_CHARS);
        assert_eq!(logcat_tag(""), "RightKit");
        assert_eq!(logcat_tag("a\0b"), "ab");
    }
}
