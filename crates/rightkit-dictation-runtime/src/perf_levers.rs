//! Engine-runtime perf levers from the 2026-10-29 performance audit
//! (`docs/audits/2026-10-29-perf/ENGINE_RUNTIME.md`, `ORT_AND_MODELS.md`).
//!
//! Every switch is read through `rightkit_dictation_core::dev_var!`, so it exists only in a `test-mode`
//! engine (QA build, tests). A release build compiles each lever to its default, which is today's
//! behaviour unless the lever is marked output-identical. Values are read once per process.
//!
//! | switch | values (default first) | finding |
//! |---|---|---|
//! | `RIGHTKIT_MAIN_ASR_IDLE_WAIT` | `auto` \| `timed` \| `untimed` | ENG-16 (output-identical `auto` is on) |
//! | `RIGHTKIT_ASR_KEEP_WARM_IDLE_MS` | unset = 500 \| 50..=5000 | ENG-01 / ORT-18 |
//! | `RIGHTKIT_ASR_KEEP_WARM_PAUSE_MODE` | `today` \| `dense` \| `off` | ENG-01 / ENG-09 |
//! | `RIGHTKIT_OPENING_PROBES` | `on` \| `pro` \| `off` | ENG-03 |
//! | `RIGHTKIT_PARALLEL_MODEL_LOAD` | unset \| `1` | ORT-04 / ENG-13 |
//! | `RIGHTKIT_STARTUP_APP_INDEX` | `always` \| `pro` | ENG-13 |
//! | `RIGHTKIT_STOP_SNAPSHOT_AFTER_QUEUE` | unset \| `1` (macOS only) | ENG-06 |
//! | `RIGHTKIT_AUTOSTOP_DRAIN_MS` | `60` (0..=500) | ENG-18 |
//! | `RIGHTKIT_L3_LIVE_RECONFIGURE` | unset \| `1` | ENG-07 |
//! | `RIGHTKIT_ENGINE_QOS` | `default` \| `responsive` | ENG-10 / ORT-10 |

use std::sync::OnceLock;
use std::time::Duration;

macro_rules! cached {
    ($ty:ty, $name:literal, $parse:expr) => {{
        static VALUE: OnceLock<$ty> = OnceLock::new();
        *VALUE.get_or_init(|| {
            let raw = rightkit_dictation_core::dev_var!($name).ok();
            $parse(raw.as_deref().map(str::trim))
        })
    }};
}

fn flag(raw: Option<&str>) -> bool {
    matches!(raw, Some("1") | Some("true") | Some("on"))
}

fn millis(raw: Option<&str>, default: u64, min: u64, max: u64) -> Duration {
    Duration::from_millis(
        raw.and_then(|value| value.parse::<u64>().ok())
            .map_or(default, |value| value.clamp(min, max)),
    )
}

/// ENG-16: how the idle main-ASR thread waits when it has nothing to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MainAsrIdleWait {
    /// Untimed only where it is output-identical: no keep-warm on this platform/build, no
    /// dictation, no file job, no lease wait and no lease a peer could ask for. Default.
    Auto,
    /// Today's 250 ms poll everywhere (A/B baseline).
    Timed,
    /// `Auto`, plus untimed on keep-warm platforms too: a dictation start then wakes the thread
    /// at once, so the first keep-warm pass runs at dictation start instead of at the next tick.
    Untimed,
}

impl MainAsrIdleWait {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("timed") | Some("off") | Some("0") => Self::Timed,
            Some("untimed") => Self::Untimed,
            _ => Self::Auto,
        }
    }

    /// Whether an idle wait may block without a timeout, given keep-warm.
    pub(crate) fn untimed_when_idle(self, keep_warm: bool) -> bool {
        match self {
            Self::Auto => !keep_warm,
            Self::Timed => false,
            Self::Untimed => true,
        }
    }

    /// Whether a dictation start must wake the idle thread (only needed when keep-warm timing
    /// would otherwise wait on a thread that sleeps untimed).
    pub(crate) fn wake_on_dictation_start(self, keep_warm: bool) -> bool {
        self == Self::Untimed && keep_warm
    }
}

pub(crate) fn main_asr_idle_wait() -> MainAsrIdleWait {
    cached!(
        MainAsrIdleWait,
        "RIGHTKIT_MAIN_ASR_IDLE_WAIT",
        MainAsrIdleWait::parse
    )
}

/// ENG-01 / ORT-18: main-ASR idle time before a keep-warm pass. `None` keeps the shipped
/// `KEEP_WARM_IDLE` (500 ms).
pub(crate) fn parse_keep_warm_idle(raw: Option<&str>) -> Option<Duration> {
    raw.and_then(|value| value.parse::<u64>().ok())
        .map(|value| Duration::from_millis(value.clamp(50, 5_000)))
}

pub(crate) fn keep_warm_idle_override() -> Option<Duration> {
    cached!(
        Option<Duration>,
        "RIGHTKIT_ASR_KEEP_WARM_IDLE_MS",
        parse_keep_warm_idle
    )
}

/// ENG-01 / ENG-09: keep-warm behaviour while the speaker pauses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeepWarmPauseMode {
    /// Two back-to-back passes when VAD silence first reaches 300 ms (shipped). Default.
    Today,
    /// Back-to-back passes while VAD silence is between 150 ms and 2.15 s; speech stops them.
    Dense,
    /// No pause burst; only the idle keep-warm runs.
    Off,
}

impl KeepWarmPauseMode {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("dense") | Some("continuous") => Self::Dense,
            Some("off") | Some("0") => Self::Off,
            _ => Self::Today,
        }
    }
}

pub(crate) fn keep_warm_pause_mode() -> KeepWarmPauseMode {
    cached!(
        KeepWarmPauseMode,
        "RIGHTKIT_ASR_KEEP_WARM_PAUSE_MODE",
        KeepWarmPauseMode::parse
    )
}

/// `Dense` pause mode runs while the current VAD silence run is in this sample range
/// (150 ms .. 150 ms + 2 s at 16 kHz).
pub(crate) const DENSE_KEEP_WARM_SILENCE_SAMPLES: std::ops::Range<usize> = 2_400..34_400;

/// ENG-03: which users get opening-command decodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpeningProbes {
    /// Every user (shipped). Default.
    On,
    /// Only Pro users; Free users' opening results can never fire a command.
    Pro,
    /// Nobody (A/B arm only: disables standalone commands).
    Off,
}

impl OpeningProbes {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("pro") => Self::Pro,
            Some("off") | Some("0") => Self::Off,
            _ => Self::On,
        }
    }

    /// Whether to submit an opening probe; `is_pro` is evaluated only in `Pro` mode.
    pub(crate) fn allows(self, is_pro: impl FnOnce() -> bool) -> bool {
        match self {
            Self::On => true,
            Self::Pro => is_pro(),
            Self::Off => false,
        }
    }
}

pub(crate) fn opening_probes() -> OpeningProbes {
    cached!(
        OpeningProbes,
        "RIGHTKIT_OPENING_PROBES",
        OpeningProbes::parse
    )
}

/// ORT-04 / ENG-13: load KWS and start the VAD load while the main ASR loads.
pub(crate) fn parallel_model_load() -> bool {
    cached!(bool, "RIGHTKIT_PARALLEL_MODEL_LOAD", flag)
}

/// ENG-13: when the Pro-only app index / shortcut refresh runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupAppIndex {
    /// At every engine start, for every user (shipped). Default.
    Always,
    /// After the shell's first config push says the user is Pro; never for Free.
    Pro,
}

impl StartupAppIndex {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("pro") => Self::Pro,
            _ => Self::Always,
        }
    }
}

pub(crate) fn startup_app_index() -> StartupAppIndex {
    cached!(
        StartupAppIndex,
        "RIGHTKIT_STARTUP_APP_INDEX",
        StartupAppIndex::parse
    )
}

/// Run the app index / shortcut prewarm at most once per engine, and only for Pro under
/// `RIGHTKIT_STARTUP_APP_INDEX=pro`. Called after each config push.
pub(crate) fn prewarm_app_index_if_pro() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if startup_app_index() != StartupAppIndex::Pro || !crate::settings::is_pro() {
        return;
    }
    if !DONE.swap(true, std::sync::atomic::Ordering::AcqRel) {
        tracing::info!(
            event = "app_index_prewarm",
            trigger = "pro_config",
            "app index prewarm"
        );
        crate::app_launch::prewarm();
    }
}

/// ENG-06 (macOS): queue the final decode before the focus snapshot and the CoreAudio pause.
#[cfg(target_os = "macos")]
pub(crate) fn stop_snapshot_after_queue() -> bool {
    cached!(bool, "RIGHTKIT_STOP_SNAPSHOT_AFTER_QUEUE", flag)
}

/// ENG-18: the event pump's PTT-hook drain on `AutoStopBegin` (shipped 60 ms).
pub(crate) fn parse_autostop_drain(raw: Option<&str>) -> Duration {
    millis(raw, 60, 0, 500)
}

pub(crate) fn autostop_drain() -> Duration {
    cached!(Duration, "RIGHTKIT_AUTOSTOP_DRAIN_MS", parse_autostop_drain)
}

/// ENG-07: serve `ReconfigureL3` live and advertise `live_l3_reconfigure`.
pub(crate) fn l3_live_reconfigure() -> bool {
    cached!(bool, "RIGHTKIT_L3_LIVE_RECONFIGURE", flag)
}

/// ENG-10 / ORT-10: thread QoS (macOS) and EcoQoS opt-out (Windows) for the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EngineQos {
    /// OS defaults (shipped). Default.
    Default,
    /// macOS: USER_INITIATED on the worker, main-ASR, KWS and event-pump threads.
    /// Windows: the process opts out of execution-speed power throttling (EcoQoS).
    Responsive,
}

impl EngineQos {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("responsive") => Self::Responsive,
            _ => Self::Default,
        }
    }
}

pub(crate) fn engine_qos() -> EngineQos {
    cached!(EngineQos, "RIGHTKIT_ENGINE_QOS", EngineQos::parse)
}

/// Apply `RIGHTKIT_ENGINE_QOS` to the calling engine thread (`thread` names it for the log).
pub(crate) fn apply_thread_qos(thread: &'static str) {
    if engine_qos() != EngineQos::Responsive {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        rightkit_automation::desktop::macos::promote_thread_qos();
        tracing::info!(event = "engine_thread_qos", thread, qos = "user_initiated");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = thread;
}

/// `apply_thread_qos` once per thread, for loops that cannot run setup before their first pass.
pub(crate) fn apply_thread_qos_once(thread: &'static str) {
    thread_local! {
        static APPLIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !APPLIED.with(|applied| applied.replace(true)) {
        apply_thread_qos(thread);
    }
}

/// Apply `RIGHTKIT_ENGINE_QOS` to the engine process (Windows: EcoQoS execution-speed opt-out).
pub(crate) fn apply_process_qos() {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE,
        };
        if engine_qos() != EngineQos::Responsive {
            return;
        }
        // ControlMask names the policy we take control of; StateMask 0 = never throttle it.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        // SAFETY: the pseudo-handle needs no close; `state` outlives the call and the size is
        // that of the struct the information class expects.
        let ok = unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        tracing::info!(
            event = "engine_process_qos",
            ecoqos_opt_out = ok != 0,
            "engine power throttling opt-out"
        );
    }
}
