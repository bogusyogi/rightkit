//! Lock-free metrics counters that the cpal callback can update without
//! allocation or synchronization beyond simple atomic ops.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static METRICS_CLOCK_ORIGIN: OnceLock<Instant> = OnceLock::new();

#[inline]
fn monotonic_micros() -> u64 {
    METRICS_CLOCK_ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros() as u64
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureErrorKind {
    DeviceBusy = 1,
    DeviceChanged = 2,
    DeviceNotAvailable = 3,
    HostUnavailable = 4,
    InvalidInput = 5,
    PermissionDenied = 6,
    RealtimeDenied = 7,
    ResourceExhausted = 8,
    StreamInvalidated = 9,
    UnsupportedConfig = 10,
    UnsupportedOperation = 11,
    Xrun = 12,
    BackendError = 13,
    Other = 14,
}

impl CaptureErrorKind {
    pub fn from_cpal(kind: cpal::ErrorKind) -> Self {
        match kind {
            cpal::ErrorKind::DeviceBusy => Self::DeviceBusy,
            cpal::ErrorKind::DeviceChanged => Self::DeviceChanged,
            cpal::ErrorKind::DeviceNotAvailable => Self::DeviceNotAvailable,
            cpal::ErrorKind::HostUnavailable => Self::HostUnavailable,
            cpal::ErrorKind::InvalidInput => Self::InvalidInput,
            cpal::ErrorKind::PermissionDenied => Self::PermissionDenied,
            cpal::ErrorKind::RealtimeDenied => Self::RealtimeDenied,
            cpal::ErrorKind::ResourceExhausted => Self::ResourceExhausted,
            cpal::ErrorKind::StreamInvalidated => Self::StreamInvalidated,
            cpal::ErrorKind::UnsupportedConfig => Self::UnsupportedConfig,
            cpal::ErrorKind::UnsupportedOperation => Self::UnsupportedOperation,
            cpal::ErrorKind::Xrun => Self::Xrun,
            cpal::ErrorKind::BackendError => Self::BackendError,
            cpal::ErrorKind::Other => Self::Other,
            _ => Self::Other,
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            Self::DeviceChanged | Self::RealtimeDenied | Self::Xrun
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeviceBusy => "device_busy",
            Self::DeviceChanged => "device_changed",
            Self::DeviceNotAvailable => "device_not_available",
            Self::HostUnavailable => "host_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::RealtimeDenied => "realtime_denied",
            Self::ResourceExhausted => "resource_exhausted",
            Self::StreamInvalidated => "stream_invalidated",
            Self::UnsupportedConfig => "unsupported_config",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::Xrun => "xrun",
            Self::BackendError => "backend_error",
            Self::Other => "other",
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::DeviceBusy,
            2 => Self::DeviceChanged,
            3 => Self::DeviceNotAvailable,
            4 => Self::HostUnavailable,
            5 => Self::InvalidInput,
            6 => Self::PermissionDenied,
            7 => Self::RealtimeDenied,
            8 => Self::ResourceExhausted,
            9 => Self::StreamInvalidated,
            10 => Self::UnsupportedConfig,
            11 => Self::UnsupportedOperation,
            12 => Self::Xrun,
            13 => Self::BackendError,
            14 => Self::Other,
            _ => return None,
        })
    }
}

#[derive(Debug, Default)]
pub struct Metrics {
    pub input_overflow_count: AtomicU64,
    pub dropped_blocks: AtomicU64,
    pub total_samples_emitted: AtomicU64,
    pub callback_invocations: AtomicU64,
    pub input_samples_received: AtomicU64,
    /// Monotonic timestamp for current stream start/resume epoch.
    pub capture_epoch_started_us: AtomicU64,
    pub first_callback_latency_us: AtomicU64,
    pub async_error_count: AtomicU64,
    pub last_async_error_kind: AtomicU8,
    pub terminal_error_kind: AtomicU8,
    pub callback_duration_sum_us: AtomicU64,
    pub callback_duration_max_us: AtomicU64,
    /// 0 = averaging all channels; N = locked onto channel N-1 after the
    /// adaptive downmix detected cancellation/dilution in the averaged mix.
    pub downmix_locked_channel: AtomicU8,
}

impl Metrics {
    pub fn new() -> Self {
        let metrics = Self::default();
        metrics
            .capture_epoch_started_us
            .store(monotonic_micros(), Ordering::Relaxed);
        metrics
    }

    /// Reset first-callback measurement immediately before stream resume.
    /// This keeps warm-stream telemetry about resume latency instead of the
    /// original stream-open latency.
    pub fn reset_capture_epoch(&self) {
        self.capture_epoch_started_us
            .store(monotonic_micros(), Ordering::Release);
        self.first_callback_latency_us.store(0, Ordering::Release);
    }

    #[inline(always)]
    pub fn record_callback(&self, duration_us: u64, input_samples: u64) {
        self.callback_invocations.fetch_add(1, Ordering::Relaxed);
        self.input_samples_received
            .fetch_add(input_samples, Ordering::Relaxed);
        let latency_us = monotonic_micros()
            .saturating_sub(self.capture_epoch_started_us.load(Ordering::Acquire));
        let _ = self.first_callback_latency_us.compare_exchange(
            0,
            latency_us.saturating_add(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.callback_duration_sum_us
            .fetch_add(duration_us, Ordering::Relaxed);
        // monotonic max via CAS loop
        let mut cur = self.callback_duration_max_us.load(Ordering::Relaxed);
        while duration_us > cur {
            match self.callback_duration_max_us.compare_exchange_weak(
                cur,
                duration_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    #[inline(always)]
    pub fn record_async_error(&self, kind: CaptureErrorKind) {
        self.async_error_count.fetch_add(1, Ordering::Relaxed);
        self.last_async_error_kind
            .store(kind as u8, Ordering::Relaxed);
        if kind.is_terminal() {
            let _ = self.terminal_error_kind.compare_exchange(
                0,
                kind as u8,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    #[inline(always)]
    pub fn record_overflow(&self, samples_lost: u64) {
        self.input_overflow_count.fetch_add(1, Ordering::Relaxed);
        if samples_lost > 0 {
            self.dropped_blocks.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn add_emitted(&self, n: u64) {
        self.total_samples_emitted.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_downmix_lock(&self, channel: usize) {
        self.downmix_locked_channel
            .store((channel as u8).saturating_add(1), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let invocations = self.callback_invocations.load(Ordering::Relaxed);
        let sum_us = self.callback_duration_sum_us.load(Ordering::Relaxed);
        let mean_us = if invocations > 0 {
            sum_us as f64 / invocations as f64
        } else {
            0.0
        };
        MetricsSnapshot {
            input_overflow_count: self.input_overflow_count.load(Ordering::Relaxed),
            dropped_blocks: self.dropped_blocks.load(Ordering::Relaxed),
            total_samples_emitted: self.total_samples_emitted.load(Ordering::Relaxed),
            callback_invocations: invocations,
            input_samples_received: self.input_samples_received.load(Ordering::Relaxed),
            first_callback_latency_us: self
                .first_callback_latency_us
                .load(Ordering::Relaxed)
                .checked_sub(1),
            async_error_count: self.async_error_count.load(Ordering::Relaxed),
            last_async_error_kind: CaptureErrorKind::from_u8(
                self.last_async_error_kind.load(Ordering::Relaxed),
            ),
            terminal_error_kind: CaptureErrorKind::from_u8(
                self.terminal_error_kind.load(Ordering::Relaxed),
            ),
            mean_callback_duration_us: mean_us,
            max_callback_duration_us: self.callback_duration_max_us.load(Ordering::Relaxed),
            downmix_locked_channel: match self.downmix_locked_channel.load(Ordering::Relaxed) {
                0 => None,
                n => Some((n - 1) as usize),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub input_overflow_count: u64,
    pub dropped_blocks: u64,
    pub total_samples_emitted: u64,
    pub callback_invocations: u64,
    pub input_samples_received: u64,
    pub first_callback_latency_us: Option<u64>,
    pub async_error_count: u64,
    pub last_async_error_kind: Option<CaptureErrorKind>,
    pub terminal_error_kind: Option<CaptureErrorKind>,
    pub mean_callback_duration_us: f64,
    pub max_callback_duration_us: u64,
    /// `Some(ch)` when the adaptive downmix abandoned channel averaging and
    /// locked onto one channel (cancellation/dilution detected).
    pub downmix_locked_channel: Option<usize>,
}
