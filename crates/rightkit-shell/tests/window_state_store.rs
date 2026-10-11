//! `WindowStateStore` used on its own (no `Shell`, no `ShellState`): an explicit directory, nothing
//! restored unless asked, minimised windows recorded, and restore that can also show, unminimise and
//! focus. The Tauri mock runtime reports no monitors and stateless windows, so the window calls are
//! observed through the `RestoreTarget` seam instead.

use std::path::PathBuf;

use rightkit_shell::window_state::{
    apply_restore, next_state, LogicalRect, MonitorSnapshot, RestoreOptions, RestoreTarget,
    WindowState, WindowStateStore,
};

fn monitor() -> MonitorSnapshot {
    MonitorSnapshot {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
        scale: 1.0,
    }
}

fn saved(minimized: bool) -> WindowState {
    WindowState {
        x: 100.0,
        y: 80.0,
        width: 1000.0,
        height: 700.0,
        maximized: false,
        minimized,
        monitor: monitor(),
        version: 1,
    }
}

#[derive(Default)]
struct Fake(Vec<&'static str>);

impl RestoreTarget for Fake {
    fn unmaximize(&mut self) {
        self.0.push("unmaximize");
    }
    fn set_size(&mut self, _: f64, _: f64) {
        self.0.push("size");
    }
    fn set_position(&mut self, _: f64, _: f64) {
        self.0.push("position");
    }
    fn maximize(&mut self) {
        self.0.push("maximize");
    }
    fn unminimize(&mut self) {
        self.0.push("unminimize");
    }
    fn show(&mut self) {
        self.0.push("show");
    }
    fn focus(&mut self) {
        self.0.push("focus");
    }
    fn minimize(&mut self) {
        self.0.push("minimize");
    }
}

const RECT: LogicalRect = LogicalRect {
    x: 10.0,
    y: 20.0,
    width: 900.0,
    height: 600.0,
};

#[test]
fn default_options_only_position_exactly_as_before() {
    let mut w = Fake::default();
    apply_restore(&mut w, RECT, &saved(false), &RestoreOptions::default());
    assert_eq!(w.0, ["unmaximize", "size", "position"]);
}

#[test]
fn restore_can_show_unminimize_and_focus_in_that_safe_order() {
    let mut w = Fake::default();
    let opts = RestoreOptions {
        unminimize: true,
        show: true,
        focus: true,
        ..Default::default()
    };
    apply_restore(&mut w, RECT, &saved(false), &opts);
    assert_eq!(
        w.0,
        [
            "unmaximize",
            "size",
            "position",
            "unminimize",
            "show",
            "focus"
        ]
    );
}

#[test]
fn a_maximised_save_is_restored_maximised_before_it_is_shown() {
    let mut w = Fake::default();
    let mut s = saved(false);
    s.maximized = true;
    let opts = RestoreOptions {
        show: true,
        ..Default::default()
    };
    apply_restore(&mut w, RECT, &s, &opts);
    assert_eq!(w.0, ["unmaximize", "size", "position", "maximize", "show"]);
}

#[test]
fn a_minimised_save_comes_back_minimised_only_on_request() {
    let opts = RestoreOptions {
        show: true,
        restore_minimized: true,
        ..Default::default()
    };
    let mut w = Fake::default();
    apply_restore(&mut w, RECT, &saved(true), &opts);
    assert_eq!(w.0, ["unmaximize", "size", "position", "show", "minimize"]);

    let mut w = Fake::default();
    let show_only = RestoreOptions {
        show: true,
        ..Default::default()
    };
    apply_restore(&mut w, RECT, &saved(true), &show_only);
    assert_eq!(w.0, ["unmaximize", "size", "position", "show"]);
}

#[test]
fn a_minimised_window_is_saved_without_losing_its_normal_geometry() {
    let prev = saved(false);
    let next = next_state(Some(&prev), monitor(), false, true, None);
    assert!(next.minimized);
    assert_eq!(
        (next.x, next.y, next.width, next.height),
        (100.0, 80.0, 1000.0, 700.0)
    );

    // Un-minimised again: live geometry wins and the flag clears.
    let live = Some((5.0, 6.0, 800.0, 500.0));
    let back = next_state(Some(&next), monitor(), false, false, live);
    assert!(!back.minimized);
    assert_eq!(
        (back.x, back.y, back.width, back.height),
        (5.0, 6.0, 800.0, 500.0)
    );

    // Minimised with nothing saved yet falls back to the default normal rectangle.
    let first = next_state(None, monitor(), false, true, None);
    assert!(first.minimized && first.width > 0.0);
}

#[test]
fn old_state_files_without_the_flag_still_load() {
    let raw = r#"{"x":1.0,"y":2.0,"width":900.0,"height":600.0,"maximized":false,
        "monitor":{"x":0.0,"y":0.0,"width":1920.0,"height":1080.0,"scale":1.0},"version":1}"#;
    let s: WindowState = serde_json::from_str(raw).unwrap();
    assert!(!s.minimized);
}

#[test]
fn the_store_works_standalone_with_an_explicit_directory_and_never_restores_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    let store = WindowStateStore::new(PathBuf::from(dir.path()));
    assert_eq!(store.dir(), dir.path());
    assert!(store.saved("main").is_none());

    store.save_state("main", saved(true)).unwrap();
    assert_eq!(store.saved("main"), Some(saved(true)));
    assert!(dir.path().join("window-state.json").is_file());

    // A second store over the same directory sees it; nothing here touched a window.
    let again = WindowStateStore::new(dir.path().to_path_buf());
    assert_eq!(again.saved("main"), Some(saved(true)));
}

#[test]
fn restore_against_a_runtime_with_no_shell_state_is_a_clean_no() {
    let app = tauri::test::mock_app();
    let _w = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = WindowStateStore::new(dir.path().to_path_buf());
    store.save_state("main", saved(false)).unwrap();
    // The mock runtime lists no monitors, so nothing can be positioned: false, not a panic or error.
    assert_eq!(store.restore(app.handle(), "main"), Ok(false));
    let show = RestoreOptions {
        show: true,
        ..Default::default()
    };
    assert_eq!(store.restore_with(app.handle(), "main", &show), Ok(false));
    assert_eq!(store.restore(app.handle(), "absent"), Ok(false));
}
