//! Public viewport core, exercised without Tauri windows or production app state.
#![cfg(feature = "control")]

use rightkit_qa::embedded::{set_viewport, PlacementGate, ViewportCfg, ViewportWindow};
use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;

struct Window {
    state: Mutex<(usize, usize)>, // applies, reads
    exact: bool,
}

impl ViewportWindow for Window {
    fn apply(&self, _: u32, _: u32) -> Result<(), String> {
        self.state.lock().unwrap().0 += 1;
        Ok(())
    }
    fn requires_exact_size(&self) -> Result<bool, String> {
        Ok(self.exact)
    }
    fn read(&self, _: Duration) -> Result<(f64, f64), String> {
        let mut state = self.state.lock().unwrap();
        state.1 += 1;
        // First reading echoes the target; a late restore then shrinks the window.
        if state.1 == 1 || (self.exact && state.0 >= 2) {
            Ok((1440.0, 1000.0))
        } else {
            Ok((1440.0, 789.0))
        }
    }
}

fn cfg() -> ViewportCfg {
    ViewportCfg {
        settle: Duration::from_secs(2),
        interval: Duration::from_millis(10),
        placement_wait: Duration::ZERO,
    }
}

#[test]
fn hidden_window_reapplies_after_late_restore_and_confirms_target_twice() {
    let window = Window {
        state: Mutex::new((0, 0)),
        exact: true,
    };
    let out = set_viewport(
        &window,
        &PlacementGate::default(),
        "main",
        1440,
        1000,
        &cfg(),
    )
    .unwrap();
    assert_eq!(out.size, (1440, 1000));
    assert!(out.settled && !out.clamped());
    assert_eq!(*window.state.lock().unwrap(), (2, 4));
    assert_eq!(out.to_json()["ok"], json!(true));
}

#[test]
fn constrained_window_returns_confirmed_actual_size_instead_of_transient_target() {
    let window = Window {
        state: Mutex::new((0, 0)),
        exact: false,
    };
    let out = set_viewport(
        &window,
        &PlacementGate::default(),
        "main",
        1440,
        1000,
        &cfg(),
    )
    .unwrap();
    assert_eq!(out.size, (1440, 789));
    assert!(out.settled && out.clamped());
    assert_eq!(*window.state.lock().unwrap(), (1, 3));
}

#[test]
fn pending_shell_placement_is_never_success_even_when_dimensions_match() {
    let window = Window {
        state: Mutex::new((0, 0)),
        exact: false,
    };
    let gate = PlacementGate::default();
    gate.begin("main");
    let out = set_viewport(&window, &gate, "main", 1440, 1000, &cfg()).unwrap();
    let reply = out.to_json();
    assert_eq!(reply["ok"], json!(false));
    assert_eq!(reply["placementTimedOut"], json!(true));
    assert!(reply["error"]
        .as_str()
        .unwrap()
        .contains("placement timed out"));
}

#[test]
fn hidden_window_that_cannot_reach_target_returns_failure_with_actual_size() {
    struct Limited;
    impl ViewportWindow for Limited {
        fn apply(&self, _: u32, _: u32) -> Result<(), String> {
            Ok(())
        }
        fn read(&self, _: Duration) -> Result<(f64, f64), String> {
            Ok((1440.0, 789.0))
        }
        fn requires_exact_size(&self) -> Result<bool, String> {
            Ok(true)
        }
    }
    let cfg = ViewportCfg {
        settle: Duration::from_millis(40),
        ..cfg()
    };
    let out = set_viewport(
        &Limited,
        &PlacementGate::default(),
        "main",
        1440,
        1000,
        &cfg,
    )
    .unwrap();
    assert_eq!(out.size, (1440, 789));
    assert!(!out.settled);
    assert_eq!(out.to_json()["ok"], json!(false));
}
