//! System-wide shortcuts via Carbon `RegisterEventHotKey`.
//!
//! Needs no Accessibility grant and never sees keystrokes that are not the registered chord, which
//! is why it is the right primitive for ordinary shortcuts (event taps are reserved for fn and
//! modifier-only gestures). Registration failure is reported precisely: an already-registered chord
//! is [`RegisterError::Conflict`], not a generic failure.
//!
//! Delivery runs through the application event target, i.e. the **main thread's run loop**
//! (Tauri / `NSApplication` provide it; a test binary must pump it, see
//! [`super::synth::pump_run_loop`]).

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use super::ffi::*;
use crate::chord::{Chord, Modifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HotkeyId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The chord is already registered (by this process or another app).
    Conflict {
        chord: String,
        os_status: i32,
    },
    /// The chord cannot be expressed as a Carbon hotkey (for example it needs fn).
    Unsupported(String),
    Os {
        chord: String,
        os_status: i32,
    },
}

impl RegisterError {
    /// `register <chord> failed: <reason>`: the shape [`crate::HotkeyConflict`] understands.
    pub fn message(&self) -> String {
        match self {
            RegisterError::Conflict { chord, os_status } => {
                format!(
                    "register {chord} failed: shortcut already registered (OSStatus {os_status})"
                )
            }
            RegisterError::Unsupported(chord) => {
                format!("register {chord} failed: chord not expressible as a system hotkey")
            }
            RegisterError::Os { chord, os_status } => {
                format!("register {chord} failed: OSStatus {os_status}")
            }
        }
    }
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for RegisterError {}

type Callback = Box<dyn Fn(HotkeyId, HotkeyState)>;

struct Inner {
    signature: u32,
    callback: Callback,
}

static NEXT_SIGNATURE: AtomicU32 = AtomicU32::new(0x524B_0001); // 'RK' + counter

pub struct GlobalHotkeys {
    inner: *mut Inner,
    handler: *mut c_void,
    refs: HashMap<HotkeyId, *mut c_void>,
    next_id: u32,
}

unsafe extern "C" fn handle(_next: *mut c_void, event: *mut c_void, user_data: *mut c_void) -> i32 {
    const EVENT_NOT_HANDLED: i32 = -9874;
    let inner = &*(user_data as *const Inner);
    let mut id = EventHotKeyID::default();
    let status = GetEventParameter(
        event,
        K_EVENT_PARAM_DIRECT_OBJECT,
        TYPE_EVENT_HOTKEY_ID,
        std::ptr::null_mut(),
        std::mem::size_of::<EventHotKeyID>() as u32,
        std::ptr::null_mut(),
        &mut id as *mut EventHotKeyID as *mut c_void,
    );
    if status != 0 || id.signature != inner.signature {
        return EVENT_NOT_HANDLED;
    }
    let state = match GetEventKind(event) {
        K_EVENT_HOTKEY_PRESSED => HotkeyState::Pressed,
        K_EVENT_HOTKEY_RELEASED => HotkeyState::Released,
        _ => return EVENT_NOT_HANDLED,
    };
    (inner.callback)(HotkeyId(id.id), state);
    0
}

impl GlobalHotkeys {
    /// Install the dispatcher. `callback` runs on the main run-loop thread for each press/release.
    pub fn new(callback: impl Fn(HotkeyId, HotkeyState) + 'static) -> Result<Self, i32> {
        let signature = NEXT_SIGNATURE.fetch_add(1, Ordering::SeqCst);
        let inner = Box::into_raw(Box::new(Inner {
            signature,
            callback: Box::new(callback),
        }));
        let types = [
            EventTypeSpec {
                event_class: K_EVENT_CLASS_KEYBOARD,
                event_kind: K_EVENT_HOTKEY_PRESSED,
            },
            EventTypeSpec {
                event_class: K_EVENT_CLASS_KEYBOARD,
                event_kind: K_EVENT_HOTKEY_RELEASED,
            },
        ];
        let mut handler: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetEventDispatcherTarget(),
                handle,
                types.len() as u32,
                types.as_ptr(),
                inner as *mut c_void,
                &mut handler,
            )
        };
        if status != 0 {
            unsafe { drop(Box::from_raw(inner)) };
            return Err(status);
        }
        Ok(Self {
            inner,
            handler,
            refs: HashMap::new(),
            next_id: 1,
        })
    }

    pub fn register(&mut self, chord: &Chord) -> Result<HotkeyId, RegisterError> {
        let name = chord.canonical();
        let mut mods = 0u32;
        for m in &chord.modifiers {
            mods |= match m {
                Modifier::Ctrl => CARBON_CONTROL_KEY,
                Modifier::Alt => CARBON_OPTION_KEY,
                Modifier::Shift => CARBON_SHIFT_KEY,
                Modifier::Meta | Modifier::Super | Modifier::Win => CARBON_CMD_KEY,
            };
        }
        let id = HotkeyId(self.next_id);
        let hk = EventHotKeyID {
            signature: unsafe { (*self.inner).signature },
            id: id.0,
        };
        let mut out: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                chord.key.mac_keycode() as u32,
                mods,
                hk,
                GetEventDispatcherTarget(),
                0,
                &mut out,
            )
        };
        match status {
            0 => {
                self.next_id += 1;
                self.refs.insert(id, out);
                Ok(id)
            }
            EVENT_HOTKEY_EXISTS_ERR => Err(RegisterError::Conflict {
                chord: name,
                os_status: status,
            }),
            EVENT_HOTKEY_INVALID_ERR => Err(RegisterError::Unsupported(name)),
            other => Err(RegisterError::Os {
                chord: name,
                os_status: other,
            }),
        }
    }

    pub fn unregister(&mut self, id: HotkeyId) -> bool {
        match self.refs.remove(&id) {
            Some(r) => unsafe { UnregisterEventHotKey(r) == 0 },
            None => false,
        }
    }

    pub fn registered(&self) -> usize {
        self.refs.len()
    }
}

impl Drop for GlobalHotkeys {
    fn drop(&mut self) {
        unsafe {
            for (_, r) in self.refs.drain() {
                UnregisterEventHotKey(r);
            }
            RemoveEventHandler(self.handler);
            drop(Box::from_raw(self.inner));
        }
    }
}
