//! Shell half: verification stays here and the engine is only told outcomes.
//! A recording transport stands in for the engine; no OS prompt, no database.
use std::cell::RefCell;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::flow::*;
use crate::AppLockState;

const LOCK_GENERATION: &str = "ui.lock_generation";
const SET_LOCK: &str = "ui.set_lock";
const SET_LOCK_ENABLED: &str = "ui.set_lock_enabled";
const METHODS: Methods<'static> = Methods {
    product: "TestApp",
    set_lock: SET_LOCK,
    lock_generation: LOCK_GENERATION,
    set_lock_enabled: SET_LOCK_ENABLED,
};

#[derive(Default)]
struct Recording(Mutex<Vec<(String, Value)>>);

impl LockTransport for Recording {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.0.lock().unwrap().push((method.to_string(), params));
        if method == LOCK_GENERATION {
            return Ok(json!(7));
        }
        Ok(json!({"enabled": true, "state": "unlocked", "available": true, "reason": null}))
    }
}

fn shell<'a>(
    enabled: bool,
    verified: Result<bool, String>,
    prompts: &'a RefCell<Vec<String>>,
) -> Shell<'a> {
    Shell {
        is_enabled: Box::new(move || enabled),
        available: Box::new(|| Ok(())),
        verify: Box::new(move |reason| {
            prompts.borrow_mut().push(reason.to_string());
            verified.clone()
        }),
    }
}

#[test]
fn unlock_verifies_once_after_reading_the_generation() {
    let engine = Recording::default();
    let prompts = RefCell::new(Vec::new());
    let s = unlock(&engine, &METHODS, &shell(true, Ok(true), &prompts)).unwrap();
    assert_eq!(s.state, AppLockState::Unlocked);
    assert_eq!(*prompts.borrow(), vec!["Unlock TestApp".to_string()]);
    let calls = engine.0.lock().unwrap();
    assert_eq!(calls[0].0, LOCK_GENERATION);
    assert_eq!(calls[1].0, SET_LOCK);
    assert_eq!(calls[1].1, json!({"state": "unlocked", "generation": 7}));
}

#[test]
fn canceled_unlock_reports_canceled_with_the_same_generation() {
    let engine = Recording::default();
    let prompts = RefCell::new(Vec::new());
    unlock(&engine, &METHODS, &shell(true, Ok(false), &prompts)).unwrap();
    let calls = engine.0.lock().unwrap();
    assert_eq!(calls[1].1, json!({"state": "canceled", "generation": 7}));
}

#[test]
fn a_verify_error_never_reaches_set_lock() {
    let engine = Recording::default();
    let prompts = RefCell::new(Vec::new());
    let err = unlock(
        &engine,
        &METHODS,
        &shell(true, Err("no hardware".into()), &prompts),
    )
    .unwrap_err();
    assert_eq!(err, "no hardware");
    assert!(engine.0.lock().unwrap().iter().all(|(m, _)| m != SET_LOCK));
}

#[test]
fn unlock_with_the_lock_off_does_not_prompt() {
    let engine = Recording::default();
    let prompts = RefCell::new(Vec::new());
    unlock(&engine, &METHODS, &shell(false, Ok(false), &prompts)).unwrap();
    assert!(prompts.borrow().is_empty());
    assert_eq!(engine.0.lock().unwrap()[1].1["state"], "unlocked");
}

#[test]
fn lock_needs_no_verification() {
    let engine = Recording::default();
    lock(&engine, &METHODS).unwrap();
    assert_eq!(
        *engine.0.lock().unwrap(),
        vec![(SET_LOCK.to_string(), json!({"state": "locked"}))]
    );
}

#[test]
fn enabling_checks_availability_and_verifies_before_the_engine_call() {
    let engine = Recording::default();
    let prompts = RefCell::new(Vec::new());
    set_enabled(&engine, &METHODS, &shell(false, Ok(true), &prompts), true).unwrap();
    assert_eq!(
        *prompts.borrow(),
        vec!["Enable TestApp app lock".to_string()]
    );
    assert_eq!(
        *engine.0.lock().unwrap(),
        vec![(SET_LOCK_ENABLED.to_string(), json!({"enabled": true}))]
    );

    let engine = Recording::default();
    let mut unavailable = shell(false, Ok(true), &prompts);
    unavailable.available = Box::new(|| Err("Windows Hello is unavailable.".into()));
    assert!(set_enabled(&engine, &METHODS, &unavailable, true).is_err());
    assert!(engine.0.lock().unwrap().is_empty());
}

#[test]
fn a_canceled_enable_or_disable_never_reaches_the_engine() {
    let prompts = RefCell::new(Vec::new());
    for enabled in [true, false] {
        let engine = Recording::default();
        let err = set_enabled(
            &engine,
            &METHODS,
            &shell(true, Ok(false), &prompts),
            enabled,
        )
        .unwrap_err();
        assert_eq!(err, "Verification was canceled.");
        assert!(engine.0.lock().unwrap().is_empty());
    }
}
