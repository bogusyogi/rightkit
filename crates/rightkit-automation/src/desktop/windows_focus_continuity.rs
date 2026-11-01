#![cfg(target_os = "windows")]

//! Cheap stop/delivery continuity proof for Windows.
//!
//! This module deliberately uses only Win32 window/process identity. In
//! particular, it never calls UI Automation on recording start or stop.

use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::null_mut,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{HMODULE, HWND},
    System::{SystemInformation::GetTickCount64, Threading::GetCurrentThreadId},
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK, WINEVENTPROC},
        WindowsAndMessaging::{
            DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, IsWindow,
            KillTimer, PostThreadMessageW, SetTimer, TranslateMessage, EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_FOCUS, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT, WM_APP,
            WM_TIMER,
        },
    },
};

const HISTORY_CAPACITY: usize = 4096;
const HEARTBEAT_PERIOD_MS: u32 = 250;
const HEARTBEAT_MAX_AGE_MS: u64 = 1_000;
const CATCH_UP_TIMEOUT: Duration = Duration::from_millis(250);
const TIMER_ID: usize = 0x4852;
const CATCH_UP_MESSAGE: u32 = WM_APP + 0x4852;

static OBSERVER: OnceLock<Arc<Observer>> = OnceLock::new();
static CATCH_UP_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
enum EventKind {
    Foreground,
    ObjectFocus,
    ObjectDestroy,
}

#[derive(Clone, Copy)]
struct ObservedEvent {
    at: u64,
    kind: EventKind,
    hwnd: isize,
    pid: u32,
}

struct ObserverState {
    coverage_started: Option<u64>,
    last_change: u64,
    heartbeat: u64,
    thread_id: u32,
    ready: bool,
    lost: bool,
    caught_up: usize,
    history_floor: Option<u64>,
    events: VecDeque<ObservedEvent>,
}

struct Observer {
    state: Mutex<ObserverState>,
    changed: Condvar,
}

impl Observer {
    fn new() -> Self {
        Self {
            state: Mutex::new(ObserverState {
                coverage_started: None,
                last_change: 0,
                heartbeat: 0,
                thread_id: 0,
                ready: false,
                lost: false,
                caught_up: 0,
                history_floor: None,
                events: VecDeque::with_capacity(HISTORY_CAPACITY),
            }),
            changed: Condvar::new(),
        }
    }
}

fn observer() -> Arc<Observer> {
    OBSERVER.get_or_init(|| Arc::new(Observer::new())).clone()
}

/// Start the process-wide WinEvent observer. Calls are idempotent.
///
/// Registration runs asynchronously; capture remains unavailable until all
/// three hooks and the heartbeat timer publish ready=true.
pub fn start_focus_observer() {
    let observer = observer();

    let should_start = match observer.state.lock() {
        Ok(state) => !state.ready && !state.lost && state.thread_id == 0,
        Err(_) => false,
    };
    if !should_start {
        return;
    }

    // A second caller can arrive between the check above and thread creation.
    // Reserve startup with thread_id=u32::MAX; the observer thread replaces it
    // with its actual id before publishing ready=true.
    if let Ok(mut state) = observer.state.lock() {
        if state.ready || state.lost || state.thread_id != 0 {
            return;
        }
        state.thread_id = u32::MAX;
    } else {
        return;
    }

    let thread_observer = Arc::clone(&observer);
    if std::thread::Builder::new()
        .name("rightkit-focus-observer".to_string())
        .spawn(move || unsafe { observer_thread(thread_observer) })
        .is_err()
    {
        mark_lost(&observer);
    }
}

/// Capture `(OS-wide GetTickCount64 timestamp, process creation FILETIME)` for
/// a foreground window. This is intentionally a native, non-UIA snapshot.
pub fn capture_focus_continuity(pid: u32, window: isize) -> Option<(u64, u64)> {
    if pid == 0 || window == 0 {
        return None;
    }
    let observer = OBSERVER.get()?.clone();
    let captured_at = unsafe { GetTickCount64() };
    let hwnd = window as HWND;

    if !target_is_current(pid, hwnd) {
        return None;
    }
    let process_instance = process_creation_time(pid)?;
    // Process/window can change while GetProcessTimes runs. Recheck both
    // before publishing the timestamp, keeping capture cheap and atomic enough
    // for a stop boundary.
    if !target_is_current(pid, hwnd) || process_creation_time(pid) != Some(process_instance) {
        return None;
    }

    let state = observer.state.lock().ok()?;
    if !state.ready
        || state.lost
        || state
            .coverage_started
            .is_none_or(|started| started > captured_at)
        || !heartbeat_fresh(&state, unsafe { GetTickCount64() })
    {
        return None;
    }
    Some((captured_at, process_instance))
}

/// Validate that `pid/window` remained the same live process/window since the
/// supplied capture timestamp. Any relevant hook event at or after the
/// timestamp fails closed; equality is rejected because GetTickCount64 has
/// millisecond granularity and WinEvent callbacks can share a tick.
pub fn validate_focus_continuity(
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    native_control: Option<isize>,
) -> bool {
    if pid == 0 || window == 0 {
        return false;
    }
    let Some(observer) = OBSERVER.get().cloned() else {
        return false;
    };

    if !catch_up(&observer) {
        return false;
    }
    let now = unsafe { GetTickCount64() };
    let (coverage_started, last_change, heartbeat, lost, history_floor, events) =
        match observer.state.lock() {
            Ok(state) => (
                state.coverage_started,
                state.last_change,
                state.heartbeat,
                state.lost,
                state.history_floor,
                state.events.iter().copied().collect::<Vec<_>>(),
            ),
            Err(_) => return false,
        };
    let Some(coverage_started) = coverage_started else {
        return false;
    };
    if lost
        || !crate::desktop::continuity_window_valid(
            captured_at,
            now,
            coverage_started,
            last_change,
            heartbeat,
            HEARTBEAT_MAX_AGE_MS,
        )
        || history_floor.is_some_and(|floor| captured_at <= floor)
    {
        return false;
    }

    let hwnd = window as HWND;
    if !target_is_current(pid, hwnd) || process_creation_time(pid) != Some(process_instance) {
        return false;
    }

    events
        .iter()
        .all(|event| event.at < captured_at || !event_relevant(*event, pid, window, native_control))
}

fn heartbeat_fresh(state: &ObserverState, now: u64) -> bool {
    state.heartbeat != 0 && now.saturating_sub(state.heartbeat) <= HEARTBEAT_MAX_AGE_MS
}

fn target_is_current(pid: u32, hwnd: HWND) -> bool {
    unsafe {
        if hwnd.is_null() || IsWindow(hwnd) == 0 || GetForegroundWindow() != hwnd {
            return false;
        }
        let mut actual_pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut actual_pid) != 0 && actual_pid == pid
    }
}

fn process_creation_time(pid: u32) -> Option<u64> {
    crate::desktop::windows::process_creation_time(pid)
}

fn event_relevant(
    event: ObservedEvent,
    pid: u32,
    window: isize,
    native_control: Option<isize>,
) -> bool {
    match event.kind {
        // A foreground event is a global focus transition. It can be emitted
        // with hwnd=0, so do not rely on PID lookup for this event kind.
        EventKind::Foreground => true,
        // Child controls report their own HWND. PID binds those focus changes
        // to the captured destination process without invalidating background
        // focus changes in unrelated applications.
        EventKind::ObjectFocus => event.hwnd == window || event.pid == pid,
        // Destroy is deliberately exact-handle scoped. A background child or
        // unrelated process destruction must not invalidate a live target.
        EventKind::ObjectDestroy => {
            event.hwnd == window || native_control.is_some_and(|control| event.hwnd == control)
        }
    }
}

fn catch_up(observer: &Arc<Observer>) -> bool {
    let request = CATCH_UP_COUNTER
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1);
    let thread_id = match observer.state.lock() {
        Ok(state) if state.ready && !state.lost => state.thread_id,
        _ => return false,
    };
    if thread_id == 0 || thread_id == u32::MAX {
        return false;
    }
    let posted = unsafe { PostThreadMessageW(thread_id, CATCH_UP_MESSAGE, request, 0) };
    if posted == 0 {
        mark_lost(observer);
        return false;
    }

    let deadline = Instant::now() + CATCH_UP_TIMEOUT;
    let mut state = match observer.state.lock() {
        Ok(state) => state,
        Err(_) => return false,
    };
    while state.caught_up < request && !state.lost {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match observer.changed.wait_timeout(state, remaining) {
            Ok((next, _)) => state = next,
            Err(_) => return false,
        }
    }
    state.caught_up >= request
        && !state.lost
        && heartbeat_fresh(&state, unsafe { GetTickCount64() })
}

unsafe fn observer_thread(observer: Arc<Observer>) {
    let callback: WINEVENTPROC = Some(win_event_proc);
    let null_module: HMODULE = null_mut::<c_void>();
    let foreground = SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        null_module,
        callback,
        0,
        0,
        WINEVENT_OUTOFCONTEXT,
    );
    let focus = SetWinEventHook(
        EVENT_OBJECT_FOCUS,
        EVENT_OBJECT_FOCUS,
        null_module,
        callback,
        0,
        0,
        WINEVENT_OUTOFCONTEXT,
    );
    let destroy = SetWinEventHook(
        EVENT_OBJECT_DESTROY,
        EVENT_OBJECT_DESTROY,
        null_module,
        callback,
        0,
        0,
        WINEVENT_OUTOFCONTEXT,
    );
    if foreground.is_null() || focus.is_null() || destroy.is_null() {
        unhook_all(foreground, focus, destroy);
        mark_lost(&observer);
        return;
    }

    let timer = SetTimer(null_mut(), TIMER_ID, HEARTBEAT_PERIOD_MS, None);
    if timer == 0 {
        unhook_all(foreground, focus, destroy);
        mark_lost(&observer);
        return;
    }

    let started = GetTickCount64();
    if let Ok(mut state) = observer.state.lock() {
        state.thread_id = GetCurrentThreadId();
        state.coverage_started = Some(started);
        state.heartbeat = started;
        state.ready = true;
        observer.changed.notify_all();
    } else {
        KillTimer(null_mut(), timer);
        unhook_all(foreground, focus, destroy);
        mark_lost(&observer);
        return;
    }

    let mut msg: MSG = std::mem::zeroed();
    loop {
        let result = GetMessageW(&mut msg, null_mut(), 0, 0);
        if result <= 0 {
            break;
        }
        if msg.message == WM_TIMER && msg.wParam == timer {
            note_heartbeat(&observer, GetTickCount64());
            continue;
        }
        if msg.message == CATCH_UP_MESSAGE {
            if let Ok(mut state) = observer.state.lock() {
                state.heartbeat = GetTickCount64();
                state.caught_up = msg.wParam;
                observer.changed.notify_all();
            }
            continue;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
        note_heartbeat(&observer, GetTickCount64());
    }

    KillTimer(null_mut(), timer);
    unhook_all(foreground, focus, destroy);
    mark_lost(&observer);
}

unsafe fn unhook_all(foreground: HWINEVENTHOOK, focus: HWINEVENTHOOK, destroy: HWINEVENTHOOK) {
    if !foreground.is_null() {
        let _ = UnhookWinEvent(foreground);
    }
    if !focus.is_null() {
        let _ = UnhookWinEvent(focus);
    }
    if !destroy.is_null() {
        let _ = UnhookWinEvent(destroy);
    }
}

fn note_heartbeat(observer: &Arc<Observer>, at: u64) {
    if let Ok(mut state) = observer.state.lock() {
        state.heartbeat = at;
        observer.changed.notify_all();
    }
}

fn mark_lost(observer: &Arc<Observer>) {
    if let Ok(mut state) = observer.state.lock() {
        state.ready = false;
        state.lost = true;
        state.thread_id = 0;
        observer.changed.notify_all();
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    let Some(observer) = OBSERVER.get() else {
        return;
    };
    let kind = match event {
        EVENT_SYSTEM_FOREGROUND => EventKind::Foreground,
        EVENT_OBJECT_FOCUS => EventKind::ObjectFocus,
        EVENT_OBJECT_DESTROY => EventKind::ObjectDestroy,
        _ => return,
    };
    let mut pid = 0u32;
    if !hwnd.is_null() {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    let observed = ObservedEvent {
        at: GetTickCount64(),
        kind,
        hwnd: hwnd as isize,
        pid,
    };
    let Ok(mut state) = observer.state.lock() else {
        return;
    };
    if !state.ready || state.lost {
        return;
    }
    state.heartbeat = observed.at;
    if matches!(observed.kind, EventKind::Foreground) {
        state.last_change = observed.at;
    }
    if state.events.len() == HISTORY_CAPACITY {
        if let Some(oldest) = state.events.pop_front() {
            state.history_floor = Some(oldest.at);
        }
    }
    state.events.push_back(observed);
    observer.changed.notify_all();
}
