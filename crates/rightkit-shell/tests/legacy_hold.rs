//! The opt-in legacy hold: a NEW build that is primary also holds and services the claim an OLD build
//! (on `tauri-plugin-single-instance`) looks for, so the old build forwards its launch and exits.
//!
//! The "old client" in each test does exactly what the plugin does at startup: Windows opens the
//! named mutex `<id>-sim` (it sees ERROR_ALREADY_EXISTS), finds the window `<id>-sic`/`<id>-siw` and
//! sends `WM_COPYDATA` (`dwData` 1542, `"{cwd}|{argv joined by |}\0"`); macOS connects to
//! `/tmp/<id with . and - as _>_si.sock` and writes `cwd`, two NULs, then the argv joined by NUL.

#![cfg(any(windows, target_os = "macos"))]

use std::sync::mpsc;
use std::time::Duration;

use rightkit_service::Suite;
use rightkit_shell::{ClaimOptions, ClaimOutcome, InstanceMessage, SingleInstance};

const ID: &str = "test-shell-hold";

/// `destroy()` is process-wide, so these tests run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn identifier() -> String {
    format!("rk.legacy-hold-{}", std::process::id())
}

fn claim_holding(
    suite: &Suite,
    identifier: &str,
    tx: mpsc::Sender<InstanceMessage>,
) -> ClaimOutcome {
    SingleInstance::claim_in_with(
        suite,
        ID,
        ClaimOptions::with_legacy(identifier).holding_legacy(),
        InstanceMessage::default(),
        move |m| {
            let _ = tx.send(m);
        },
    )
}

#[cfg(windows)]
mod old_client {
    use std::ffi::c_void;

    #[repr(C)]
    struct CopyDataStruct {
        dw_data: usize,
        cb_data: u32,
        lp_data: *const u8,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Whether the legacy mutex exists (the plugin's `CreateMutexW`, then `GetLastError`).
    pub fn mutex_exists(identifier: &str) -> bool {
        let name = wide(&format!("{identifier}-sim"));
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
            let exists = GetLastError() == 183;
            if h != 0 {
                CloseHandle(h);
            }
            exists
        }
    }

    pub fn window_exists(identifier: &str) -> bool {
        let class = wide(&format!("{identifier}-sic"));
        let title = wide(&format!("{identifier}-siw"));
        unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) != 0 }
    }

    /// The plugin's launch: returns false when it would not find a window and so start normally.
    pub fn launch(identifier: &str, cwd: &str, argv: &[&str]) -> bool {
        let class = wide(&format!("{identifier}-sic"));
        let title = wide(&format!("{identifier}-siw"));
        unsafe {
            let hwnd = FindWindowW(class.as_ptr(), title.as_ptr());
            if hwnd == 0 {
                return false;
            }
            let data = format!("{cwd}|{}\0", argv.join("|"));
            let bytes = data.as_bytes();
            let cds = CopyDataStruct {
                dw_data: 1542,
                cb_data: bytes.len() as u32,
                lp_data: bytes.as_ptr(),
            };
            SendMessageW(hwnd, 0x004A, 0, &cds as *const _ as isize);
            true
        }
    }
}

#[cfg(windows)]
#[test]
fn an_old_client_finds_the_held_claim_and_its_launch_reaches_on_message() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let identifier = identifier();
    let suite = Suite::per_run_temp("shell-legacy-hold").unwrap();
    assert!(!old_client::mutex_exists(&identifier), "precondition");
    let (tx, rx) = mpsc::channel();
    let guard = claim_holding(&suite, &identifier, tx);
    assert!(guard.is_primary());

    // What the plugin does: the mutex already exists, so it looks for the window and forwards.
    assert!(
        old_client::mutex_exists(&identifier),
        "the old build must see ERROR_ALREADY_EXISTS"
    );
    assert!(old_client::launch(
        &identifier,
        "C:\\work",
        &["C:\\old.exe", "--section", "x"]
    ));
    let message = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the launch reached on_message");
    assert_eq!(message.cwd, "C:\\work");
    assert_eq!(message.argv, ["C:\\old.exe", "--section", "x"]);
    assert!(message.action.is_none());

    // destroy() releases the legacy claim: the mutex and the window are gone when it returns.
    SingleInstance::destroy();
    assert!(
        !old_client::window_exists(&identifier),
        "the window must be gone after destroy"
    );
    assert!(
        !old_client::mutex_exists(&identifier),
        "the mutex must be gone after destroy"
    );
    drop(guard);
}

#[cfg(windows)]
#[test]
fn a_live_old_holder_receives_this_launch_and_no_second_primary_is_made() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }
    let identifier = format!("{}-old", self::identifier());
    let name: Vec<u16> = format!("{identifier}-sim")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // A live old build whose window is missing cannot take the launch: fail closed, not a second primary.
    let old = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    assert_ne!(old, 0);
    let suite = Suite::per_run_temp("shell-legacy-hold-old").unwrap();
    let (tx, _rx) = mpsc::channel();
    let outcome = claim_holding(&suite, &identifier, tx);
    assert!(
        matches!(outcome, ClaimOutcome::Failed(_)),
        "a live old holder that cannot take the launch must not become a second primary"
    );
    unsafe { CloseHandle(old) };
    // And the failed attempt released the endpoint it had hosted: the next claim can win.
    let (tx, _rx) = mpsc::channel();
    assert!(claim_holding(&suite, &identifier, tx).is_primary());
    SingleInstance::destroy();
}

#[cfg(target_os = "macos")]
#[test]
fn an_old_client_finds_the_held_socket_and_its_launch_reaches_on_message() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let identifier = identifier();
    let socket =
        rightkit_shell::LegacySingleInstanceNames::for_identifier(&identifier).macos_socket;
    let _ = std::fs::remove_file(&socket);
    let suite = Suite::per_run_temp("shell-legacy-hold").unwrap();
    let (tx, rx) = mpsc::channel();
    let guard = claim_holding(&suite, &identifier, tx);
    assert!(guard.is_primary());
    assert!(
        socket.exists(),
        "the legacy socket must exist while primary"
    );

    // What the plugin does: connect, write cwd, two NULs, the argv joined by NUL, close.
    let mut stream = UnixStream::connect(&socket).expect("the old build finds a live holder");
    stream.write_all(b"/work\0\0/old\0--section\0x").unwrap();
    drop(stream);
    let message = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the launch reached on_message");
    assert_eq!(message.cwd, "/work");
    assert_eq!(message.argv, ["/old", "--section", "x"]);

    // Malformed and oversized payloads never reach the callback.
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream.write_all(b"no separator").unwrap();
    drop(stream);
    let mut stream = UnixStream::connect(&socket).unwrap();
    let _ = stream.write_all(&vec![b'a'; 200_000]);
    drop(stream);
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());

    SingleInstance::destroy();
    assert!(!socket.exists(), "the socket must be gone after destroy");
    assert!(UnixStream::connect(&socket).is_err());
    drop(guard);
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_old_socket_holder_receives_this_launch() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    let identifier = format!("{}-old", identifier());
    let socket =
        rightkit_shell::LegacySingleInstanceNames::for_identifier(&identifier).macos_socket;
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let (old_tx, old_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut bytes = Vec::new();
            let _ = s.read_to_end(&mut bytes);
            let _ = old_tx.send(bytes);
        }
    });
    let suite = Suite::per_run_temp("shell-legacy-hold-old").unwrap();
    let (tx, _rx) = mpsc::channel();
    let outcome = claim_holding(&suite, &identifier, tx);
    assert!(
        matches!(outcome, ClaimOutcome::Forwarded),
        "the live old holder takes the launch"
    );
    assert!(old_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .windows(2)
        .any(|w| w == [0, 0]));
    let _ = std::fs::remove_file(&socket);
    // The primary hosted for the attempt was released: the next claim wins.
    let (tx, _rx) = mpsc::channel();
    assert!(claim_holding(&suite, &identifier, tx).is_primary());
    SingleInstance::destroy();
}
