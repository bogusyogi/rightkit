//! A small modal window with a masked PIN entry, for the owner to prove it is them when OS
//! verification is not accepted for an approval. The caller's `submit` checks each PIN.
//!
//! The window does not run on its own: the host opens it from a worker thread after the owner's
//! click. Nothing here stores or logs a PIN: the text is read from the edit control, handed
//! to the caller's `submit` once, and the control and the buffers are cleared right after.

use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, LoadCursorW,
    PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow,
    TranslateMessage, UnregisterClassW, HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WNDCLASSW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

/// What the caller says about one submitted PIN.
pub enum Outcome {
    /// Accepted: the window closes.
    Done,
    /// Wrong or refused, may try again: the message shows and the field is cleared.
    Retry(String),
    /// Refused for good here (locked out, no PIN set): the message shows, entry is disabled.
    Stop(String),
}

pub type Submit = Box<dyn FnMut(String) -> Outcome>;

struct Dialog {
    submit: Submit,
    edit: isize,
    message: isize,
    ok: isize,
}

thread_local! {
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

const ID_EDIT: i32 = 102;
const ID_MESSAGE: i32 = 103;
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;
const EM_LIMITTEXT: u32 = 0x00C5;
/// The longest PIN the window takes.
const MAX_PIN_CHARS: usize = 64;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hwnd_of(key: isize) -> HWND {
    HWND(key as *mut c_void)
}

/// Shows a small modal window with a masked entry and runs its message loop on the calling
/// thread until it closes. `submit` gets each PIN the user enters (Enter or the Unlock button);
/// the field is cleared after every attempt. Returns when the window is gone.
///
/// `hint` is the explanatory line under the entry.
pub fn pin_dialog(title: &str, prompt: &str, hint: &str, submit: Submit) {
    let Some(instance) = unsafe { GetModuleHandleW(None) }.ok().map(HINSTANCE::from) else {
        return;
    };
    let class = w!("RightKitUnlockPin");
    let Ok(cursor) = (unsafe { LoadCursorW(None, IDC_ARROW) }) else {
        return;
    };
    let registered = WNDCLASSW {
        hInstance: instance,
        lpszClassName: class,
        lpfnWndProc: Some(proc),
        hCursor: cursor,
        // COLOR_BTNFACE + 1
        hbrBackground: HBRUSH(16usize as *mut c_void),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&registered) } == 0 {
        return;
    }
    run_dialog(class, instance, [title, prompt, hint], submit);
    let _ = unsafe { UnregisterClassW(class, Some(instance)) };
}

fn run_dialog(class: PCWSTR, instance: HINSTANCE, texts: [&str; 3], submit: Submit) {
    let [title, prompt, hint] = texts;
    let (width, height) = (380, 236);
    let (screen_w, screen_h) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let title = wide(title);
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
            class,
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            (screen_w - width) / 2,
            (screen_h - height) / 3,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
    };
    let Ok(window) = created else {
        return;
    };
    let child = |class: PCWSTR, text: &str, style: u32, rect: (i32, i32, i32, i32), id: i32| {
        let text = wide(text);
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                Some(window),
                Some(HMENU(id as usize as *mut c_void)),
                Some(instance),
                None,
            )
        }
        .map(|hwnd| hwnd.0 as isize)
        .unwrap_or(0)
    };
    let tab = WS_TABSTOP.0;
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let label = child(w!("STATIC"), prompt, 0, (16, 14, 340, 36), 101);
    // ES_PASSWORD | ES_AUTOHSCROLL, bordered: the masked entry.
    let edit = child(
        w!("EDIT"),
        "",
        tab | WS_BORDER.0 | 0x20 | 0x80,
        (16, 56, 340, 24),
        ID_EDIT,
    );
    let message = child(w!("STATIC"), "", 0, (16, 88, 340, 36), ID_MESSAGE);
    let hello = child(w!("STATIC"), hint, 0, (16, 128, 340, 44), 104);
    // BS_DEFPUSHBUTTON = 1, BS_PUSHBUTTON = 0.
    let ok = child(w!("BUTTON"), "Unlock", tab | 1, (176, 180, 86, 28), ID_OK);
    let cancel = child(w!("BUTTON"), "Cancel", tab, (270, 180, 86, 28), ID_CANCEL);
    for control in [label, edit, message, hello, ok, cancel] {
        if control != 0 {
            unsafe {
                SendMessageW(
                    hwnd_of(control),
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
    }
    if edit != 0 {
        unsafe {
            SendMessageW(
                hwnd_of(edit),
                EM_LIMITTEXT,
                Some(WPARAM(MAX_PIN_CHARS)),
                Some(LPARAM(0)),
            );
        }
    }
    DIALOG.with(|cell| {
        *cell.borrow_mut() = Some(Dialog {
            submit,
            edit,
            message,
            ok,
        });
    });
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
        if edit != 0 {
            let _ = SetFocus(Some(hwnd_of(edit)));
        }
    }
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        // Tab, Enter (the default button) and Escape (Cancel) are the dialog manager's.
        if unsafe { IsDialogMessageW(window, &message) }.as_bool() {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    DIALOG.with(|cell| *cell.borrow_mut() = None);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == ID_OK {
                attempt();
            } else if id == ID_CANCEL {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Reads and clears the field, runs the caller's check and shows its answer.
fn attempt() {
    let Some((edit, message, ok)) =
        DIALOG.with(|cell| cell.borrow().as_ref().map(|d| (d.edit, d.message, d.ok)))
    else {
        return;
    };
    let edit_hwnd = hwnd_of(edit);
    let length = unsafe { GetWindowTextLengthW(edit_hwnd) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let copied = unsafe { GetWindowTextW(edit_hwnd, &mut buffer) }.max(0) as usize;
    let mut pin = String::from_utf16_lossy(&buffer[..copied.min(buffer.len())]);
    buffer.iter_mut().for_each(|unit| *unit = 0);
    let _ = unsafe { SetWindowTextW(edit_hwnd, w!("")) };
    if pin.is_empty() {
        return;
    }
    // The submit closure is taken out while it runs so a message pumped inside it cannot re-enter.
    let taken = DIALOG.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .map(|d| std::mem::replace(&mut d.submit, Box::new(|_| Outcome::Retry(String::new()))))
    });
    let Some(mut submit) = taken else {
        return;
    };
    let outcome = submit(std::mem::take(&mut pin));
    DIALOG.with(|cell| {
        if let Some(dialog) = cell.borrow_mut().as_mut() {
            dialog.submit = submit;
        }
    });
    match outcome {
        Outcome::Done => {
            if let Some(window) = window_of(edit_hwnd) {
                let _ = unsafe { DestroyWindow(window) };
            }
        }
        Outcome::Retry(text) => {
            let _ = unsafe { SetWindowTextW(hwnd_of(message), PCWSTR(wide(&text).as_ptr())) };
            let _ = unsafe { SetFocus(Some(edit_hwnd)) };
        }
        Outcome::Stop(text) => {
            let _ = unsafe { SetWindowTextW(hwnd_of(message), PCWSTR(wide(&text).as_ptr())) };
            let _ = unsafe { EnableWindow(edit_hwnd, false) };
            let _ = unsafe { EnableWindow(hwnd_of(ok), false) };
        }
    }
}

fn window_of(child: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::GetParent;
    unsafe { GetParent(child) }.ok()
}
