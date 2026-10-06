//! The kill switch is process-global, so it lives in its own test binary.

use rightkit_automation::mock::*;
use rightkit_automation::*;

#[test]
fn kill_switch_fails_closed_everywhere_and_resets() {
    let mock = MockPlatform::fixture();
    let a = Automation::new(mock.clone(), Policy::new(["com.test.editor"]));
    let s = a.snapshot(Target::app(EDITOR_PID), Limits::default()).unwrap();
    let save = s.root.find(&|n| n.name.as_deref() == Some("Save")).unwrap().id.clone();
    let pin = a.capture_pin(Some(EDITOR_PID)).unwrap();

    kill();
    assert!(killed());
    assert!(matches!(a.snapshot(Target::app(EDITOR_PID), Limits::default()), Err(Error::Blocked(_))));
    assert!(matches!(a.act(&s.id, Some(&save), Action::Press, &ActOptions::default()), Err(Error::Blocked(_))));
    assert!(matches!(a.begin_session(EDITOR_PID, "x"), Err(Error::Blocked(_))));
    let r = a.deliver_text(&pin, "hi", &DeliveryOptions::default());
    assert!(matches!(r.outcome, DeliveryOutcome::NotDelivered(Why::Blocked(_))));
    let steps = vec![(s.id.clone(), Some(save.clone()), Action::Press); 3];
    let out = a.act_batch(&steps, &ActOptions::default());
    assert_eq!(out.len(), 1, "batch stops at the first blocked step");
    assert!(mock.with(|m| m.pressed.is_empty()));

    reset_kill();
    assert!(a.act(&s.id, Some(&save), Action::Press, &ActOptions::default()).unwrap().done());
}
