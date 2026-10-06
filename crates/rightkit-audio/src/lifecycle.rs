//! Windows-only capture endpoint & power lifecycle adapter (Wave W1).
//!
//! Behavioral reference: the MIT `windows-mic-listener.c` donor (OpenWhispr)
//! for the IMMNotificationClient registration / enqueue-only-callback /
//! unregister-on-drop shape. Only that discipline is ported — no session or
//! PID-refcount logic from the donor is reused, and no TypeWhisper (GPL)
//! source was read or referenced. `PowerRegisterSuspendResumeNotification`
//! follows the Microsoft-documented suspend/resume callback contract.
//!
//! LOCKED (docs/WINDOWS_IMPLEMENTATION_SET.md Wave W1;
//! docs/NATIVE_PLATFORM_ARCHITECTURE.md "Windows"):
//! - endpoint callbacks enqueue one bounded typed event; power callbacks
//!   publish latest transition through lock-free atomic slot; both share a
//!   monotonic sequence & return immediately; they never touch capture state,
//!   never block, and never teardown or rebuild anything;
//! - registration owns COM/power-notification lifetime; `Drop` unregisters
//!   every callback and releases every COM object;
//! - this module only observes endpoint/power events and publishes them; it
//!   never opens, owns, or reopens a capture stream, never picks a capture
//!   backend or inference provider, and never emits diagnostics itself —
//!   coalescing, recovery, and the `windows_capture_lifecycle` record live in
//!   the worker loop (`worker/worker_main.rs` streaming phase,
//!   `worker/capture_recovery.rs`) so they can share state with the
//!   existing capture owner.
//!
//! Non-Windows targets get a no-op stub with the same public surface so
//! callers never need a platform `cfg` at the call site.

use std::sync::{mpsc::Receiver, Arc};
use std::time::Instant;

pub type LifecycleNotify = Arc<dyn Fn() + Send + Sync + 'static>;

/// Why one lifecycle event fired. Values map 1:1 onto the diagnostics
/// contract's `cause=` field (docs/WINDOWS_IMPLEMENTATION_SET.md, Wave W1
/// route/backend diagnostics) via `as_diagnostic_str`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LifecycleCause {
    /// A capture endpoint was added or its state changed (not removal, not
    /// the default route). Ranked lowest: least likely to require a reopen.
    DeviceStateChanged,
    /// The default input endpoint changed for the console or communications
    /// role. Only device-flow==capture events reach this variant.
    DefaultChanged,
    /// A property value changed on an existing capture endpoint (format,
    /// friendly name, etc.).
    PropertyChanged,
    /// A capture endpoint disappeared. Ranked above generic state/property
    /// changes because it is the one most likely to break an active stream.
    DeviceRemoved,
    /// `PBT_APMRESUMEAUTOMATIC` / `PBT_APMRESUMESUSPEND`.
    Resume,
    /// `PBT_APMSUSPEND`. Ranked highest: when a suspend and an endpoint event
    /// land in the same burst, the generation must quiesce for sleep rather
    /// than spend the burst reopening a stream about to be torn down anyway.
    Suspend,
}

impl LifecycleCause {
    pub fn as_diagnostic_str(self) -> &'static str {
        match self {
            Self::DeviceStateChanged => "device_state",
            Self::DefaultChanged => "default_changed",
            Self::PropertyChanged => "property_changed",
            Self::DeviceRemoved => "device_removed",
            Self::Resume => "resume",
            Self::Suspend => "suspend",
        }
    }
}

/// One enqueued lifecycle occurrence. `sequence` is monotonic per
/// registration (endpoint and power sources share one counter) so the worker
/// can detect gaps if the bounded channel ever drops an event.
#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub sequence: u64,
    pub cause: LifecycleCause,
    pub observed_at: Instant,
    /// Internal-only IMM endpoint identity used to discard unrelated device
    /// notifications before capture is touched. Never emitted in telemetry.
    pub endpoint_id: Option<String>,
}

/// Owns the registered Windows lifecycle sources. Dropping it unregisters
/// every callback and releases every COM object; it never blocks on
/// in-flight capture work because callbacks only ever push onto the bounded
/// receiver/lock-free power slot handed back by [`try_register`].
pub struct WindowsAudioLifecycle {
    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    inner: windows_impl::Registration,
    #[cfg(target_os = "windows")]
    latest_power_transition: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl WindowsAudioLifecycle {
    /// Returns latest unconsumed Suspend/Resume transition. Power delivery is
    /// independent from bounded endpoint queue, so notification storms cannot
    /// drop Resume & strand capture in suspended state.
    pub fn take_pending_power_event(&self) -> Option<LifecycleEvent> {
        #[cfg(target_os = "windows")]
        {
            use std::sync::atomic::Ordering;

            let packed = self.latest_power_transition.swap(0, Ordering::AcqRel);
            if packed == 0 {
                return None;
            }
            let cause = match packed & 0b11 {
                1 => LifecycleCause::Suspend,
                2 => LifecycleCause::Resume,
                _ => return None,
            };
            Some(LifecycleEvent {
                sequence: packed >> 2,
                cause,
                observed_at: Instant::now(),
                endpoint_id: None,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }
}

/// Register endpoint (`IMMNotificationClient`) & power
/// (`PowerRegisterSuspendResumeNotification`) sources on the calling thread.
/// Returns `None` when registration is unavailable (non-Windows target, or a
/// Windows registration failure) — the worker keeps running on the existing
/// late-detection path (route-signature check at recording start, stream
/// error during capture) exactly as it did before this adapter existed.
pub fn try_register() -> Option<(WindowsAudioLifecycle, Receiver<LifecycleEvent>)> {
    try_register_with_notify(None)
}

pub fn try_register_with_notify(
    notify: Option<LifecycleNotify>,
) -> Option<(WindowsAudioLifecycle, Receiver<LifecycleEvent>)> {
    #[cfg(target_os = "windows")]
    {
        windows_impl::register(notify)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = notify;
        None
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{LifecycleCause, LifecycleEvent};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
    use std::sync::Arc;
    use std::time::Instant;

    use windows::core::implement;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eMultimedia, EDataFlow, ERole, IMMDeviceEnumerator,
        IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Power::{
        PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
    };
    // In `windows` 0.62 the power BROADCAST constants and the notify-callback
    // flag live under UI::WindowsAndMessaging (they are WM_POWERBROADCAST
    // payloads), not System::Power, which keeps only the register/unregister
    // entry points and their parameter struct.
    use windows::Win32::UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    };
    // `PROPERTYKEY` also moved out of `windows::core` in 0.62.
    use windows::core::{Result as WinResult, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, PROPERTYKEY, WIN32_ERROR};

    /// Bounded so an endpoint notification storm (e.g. a flaky USB hub cycling
    /// add/remove) cannot grow memory unbounded. The worker coalesces every
    /// queued event into one recovery generation per drain, so dropping
    /// excess raw events here loses no recovery fidelity — the worker still
    /// re-resolves the route from scratch on the next tick. Windows-only:
    /// only `register` below ever constructs the channel.
    const LIFECYCLE_QUEUE_CAPACITY: usize = 64;

    /// Enqueue-only endpoint callback. Every method here does exactly one
    /// thing: classify the event and push it onto the bounded channel. No
    /// method reads device properties, opens a device, or touches capture
    /// state — that discipline is what keeps the OS notification thread from
    /// ever blocking on worker/capture work.
    #[implement(IMMNotificationClient)]
    struct EndpointNotifications {
        sequence: Arc<AtomicU64>,
        tx: SyncSender<LifecycleEvent>,
        notify: Option<super::LifecycleNotify>,
    }

    impl EndpointNotifications {
        fn enqueue(&self, cause: LifecycleCause, endpoint_id: Option<String>) {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            // `try_send`: a full queue means the worker has not drained a
            // recent burst yet. Dropping is safe — the next drain still
            // re-resolves the route from scratch, so no recovery fidelity is
            // lost, only a duplicate trigger for the same generation.
            let _ = self.tx.try_send(LifecycleEvent {
                sequence,
                cause,
                observed_at: Instant::now(),
                endpoint_id,
            });
            if let Some(notify) = self.notify.as_ref() {
                notify();
            }
        }
    }

    fn endpoint_id(value: &PCWSTR) -> Option<String> {
        if value.is_null() {
            None
        } else {
            // SAFETY: Windows owns this callback string for this invocation.
            unsafe { value.to_string().ok() }
        }
    }

    impl IMMNotificationClient_Impl for EndpointNotifications_Impl {
        fn OnDeviceStateChanged(
            &self,
            device_id: &PCWSTR,
            // `windows` 0.62 types this as DEVICE_STATE rather than a raw u32.
            _new_state: DEVICE_STATE,
        ) -> WinResult<()> {
            self.enqueue(LifecycleCause::DeviceStateChanged, endpoint_id(device_id));
            Ok(())
        }

        fn OnDeviceAdded(&self, device_id: &PCWSTR) -> WinResult<()> {
            self.enqueue(LifecycleCause::DeviceStateChanged, endpoint_id(device_id));
            Ok(())
        }

        fn OnDeviceRemoved(&self, device_id: &PCWSTR) -> WinResult<()> {
            self.enqueue(LifecycleCause::DeviceRemoved, endpoint_id(device_id));
            Ok(())
        }

        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            role: ERole,
            default_device_id: &PCWSTR,
        ) -> WinResult<()> {
            // Only capture-flow, console/communications-role changes drive
            // HeardRight's system-default input selection; a render-flow or
            // multimedia-only-role change is not acted on.
            if flow == eCapture && (role == eConsole || role == eMultimedia) {
                self.enqueue(
                    LifecycleCause::DefaultChanged,
                    endpoint_id(default_device_id),
                );
            }
            Ok(())
        }

        fn OnPropertyValueChanged(&self, device_id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
            self.enqueue(LifecycleCause::PropertyChanged, endpoint_id(device_id));
            Ok(())
        }
    }

    /// Context handed to the raw power callback. Leaked deliberately by
    /// `register` and freed exactly once, in `Registration::drop`, after the
    /// subscription is torn down — the callback must never be invoked again
    /// once this memory is freed.
    struct PowerCallbackState {
        sequence: Arc<AtomicU64>,
        latest_power_transition: Arc<AtomicU64>,
        notify: Option<super::LifecycleNotify>,
    }

    /// Atomically record the power event and signal the worker; no device
    /// recovery or model work runs in this callback. Matches the official
    /// `PowerRegisterSuspendResumeNotification`/`DEVICE_NOTIFY_CALLBACK`
    /// contract: return `0` (`ERROR_SUCCESS`) promptly so the OS is never
    /// held up by this background notification.
    // 0.62's PDEVICE_NOTIFY_CALLBACK_ROUTINE takes *const, not *mut.
    unsafe extern "system" fn power_notify_callback(
        context: *const core::ffi::c_void,
        event_type: u32,
        _setting: *const core::ffi::c_void,
    ) -> u32 {
        // SAFETY: `context` is the `PowerCallbackState` pointer `register`
        // leaked below; `Registration::drop` unregisters this callback
        // before freeing that memory, so it is valid for every call this
        // function can receive.
        let state = unsafe { &*(context as *const PowerCallbackState) };
        let cause = match event_type {
            PBT_APMSUSPEND => Some(LifecycleCause::Suspend),
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some(LifecycleCause::Resume),
            _ => None,
        };
        if let Some(cause) = cause {
            let sequence = state.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            let cause_code = match cause {
                LifecycleCause::Suspend => 1,
                LifecycleCause::Resume => 2,
                _ => unreachable!(),
            };
            state
                .latest_power_transition
                .fetch_max((sequence << 2) | cause_code, Ordering::Release);
            if let Some(notify) = state.notify.as_ref() {
                notify();
            }
        }
        0
    }

    /// Owns every handle that must be torn down together: the endpoint
    /// registration, the power subscription, the leaked callback context,
    /// and (only if this registration performed it) the COM apartment on
    /// this thread.
    pub(super) struct Registration {
        com_initialized_here: bool,
        enumerator: IMMDeviceEnumerator,
        client: IMMNotificationClient,
        power_notify: HPOWERNOTIFY,
        power_state: *mut PowerCallbackState,
    }

    // SAFETY: every field is touched only from the thread that called
    // `register` (Drop runs on that same worker thread when the worker
    // shuts down); `power_state` is freed exactly once, in `drop`, after
    // both subscriptions are unregistered.
    unsafe impl Send for Registration {}

    impl Drop for Registration {
        fn drop(&mut self) {
            unsafe {
                let _ = self
                    .enumerator
                    .UnregisterEndpointNotificationCallback(&self.client);
                let _ = PowerUnregisterSuspendResumeNotification(self.power_notify);
                drop(Box::from_raw(self.power_state));
                if self.com_initialized_here {
                    CoUninitialize();
                }
            }
        }
    }

    pub(super) fn register(
        notify: Option<super::LifecycleNotify>,
    ) -> Option<(super::WindowsAudioLifecycle, Receiver<LifecycleEvent>)> {
        let (tx, rx) = sync_channel(LIFECYCLE_QUEUE_CAPACITY);
        let sequence = Arc::new(AtomicU64::new(0));
        let latest_power_transition = Arc::new(AtomicU64::new(0));

        // COINIT_MULTITHREADED matches cpal's WASAPI capture-thread model.
        // If this thread already has apartment-model COM initialized by
        // something else, RPC_E_CHANGED_MODE means COM is already usable
        // here, not that registration must fail.
        let com_initialized_here = unsafe {
            // 0.62 returns a bare HRESULT here instead of a Result.
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_ok() {
                true
            } else if hr == RPC_E_CHANGED_MODE {
                false
            } else {
                tracing::warn!(
                    error = %windows::core::Error::from(hr),
                    "windows audio lifecycle: CoInitializeEx failed"
                );
                return None;
            }
        };

        let enumerator: IMMDeviceEnumerator =
            match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
                Ok(enumerator) => enumerator,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "windows audio lifecycle: MMDeviceEnumerator create failed"
                    );
                    if com_initialized_here {
                        unsafe { CoUninitialize() };
                    }
                    return None;
                }
            };

        let client: IMMNotificationClient = EndpointNotifications {
            sequence: Arc::clone(&sequence),
            tx: tx.clone(),
            notify: notify.clone(),
        }
        .into();

        if let Err(error) = unsafe { enumerator.RegisterEndpointNotificationCallback(&client) } {
            tracing::warn!(
                %error,
                "windows audio lifecycle: endpoint registration failed"
            );
            if com_initialized_here {
                unsafe { CoUninitialize() };
            }
            return None;
        }

        let power_state = Box::into_raw(Box::new(PowerCallbackState {
            sequence,
            latest_power_transition: Arc::clone(&latest_power_transition),
            notify: notify.clone(),
        }));

        let mut params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_notify_callback),
            Context: power_state as *mut _,
        };
        let mut power_notify: *mut core::ffi::c_void = core::ptr::null_mut();
        // 0.62: `recipient` is a HANDLE and the out-param is `*mut *mut c_void`;
        // the return is a WIN32_ERROR rather than a bare integer.
        let power_result = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                HANDLE(&mut params as *mut _ as *mut _),
                &mut power_notify,
            )
        };
        if power_result != WIN32_ERROR(0) {
            tracing::warn!(
                code = power_result.0,
                "windows audio lifecycle: power registration failed"
            );
            let _ = unsafe { enumerator.UnregisterEndpointNotificationCallback(&client) };
            // SAFETY: never handed to the OS, so nothing else can reach it.
            drop(unsafe { Box::from_raw(power_state) });
            if com_initialized_here {
                unsafe { CoUninitialize() };
            }
            return None;
        }

        let registration = Registration {
            com_initialized_here,
            enumerator,
            client,
            power_notify: HPOWERNOTIFY(power_notify as isize),
            power_state,
        };
        Some((
            super::WindowsAudioLifecycle {
                inner: registration,
                latest_power_transition,
            },
            rx,
        ))
    }
}
