//! Host-decided lifecycle: activation policy on hide and reveal, QA-instance exemption, explicit
//! visibility, and the lifecycle-only plugin. Pure decisions; the NSApplication call itself is
//! not exercised (see the crate CHANGELOG).

use rightkit_tauri_shell::lifecycle::{
    activation_for, instance_mode, show_on_ready_decision, Activation, ActivationTracker,
    InstanceMode, Visibility,
};
use rightkit_tauri_shell::{Module, ModuleSet, Shell};
use tauri::plugin::Plugin;

#[test]
fn hide_to_background_goes_accessory_on_hide_and_regular_on_reveal() {
    assert_eq!(
        activation_for(true, Visibility::Hidden, false),
        Some(Activation::Accessory)
    );
    assert_eq!(
        activation_for(true, Visibility::Revealed, false),
        Some(Activation::Regular)
    );
}

#[test]
fn another_visible_window_keeps_the_app_regular_and_the_default_changes_nothing() {
    assert_eq!(activation_for(true, Visibility::Hidden, true), None);
    for v in [Visibility::Hidden, Visibility::Revealed] {
        for other in [false, true] {
            assert_eq!(
                activation_for(false, v, other),
                None,
                "default must not change policy"
            );
        }
    }
}

#[test]
fn the_tracker_applies_only_real_changes() {
    let t = ActivationTracker::default();
    assert_eq!(
        t.transition(Some(Activation::Regular)),
        None,
        "already regular"
    );
    assert_eq!(
        t.transition(Some(Activation::Accessory)),
        Some(Activation::Accessory)
    );
    assert_eq!(t.transition(Some(Activation::Accessory)), None);
    assert_eq!(t.transition(None), None);
    assert_eq!(
        t.transition(Some(Activation::Regular)),
        Some(Activation::Regular)
    );
}

#[test]
fn qa_exemption_skips_arbitration_only_for_a_hidden_qa_launch_that_opted_in() {
    // single instance off stays off.
    assert_eq!(instance_mode(false, true, true), InstanceMode::Disabled);
    // Default: a hidden QA launch still arbitrates, as before.
    assert_eq!(instance_mode(true, false, true), InstanceMode::Arbitrate);
    assert_eq!(instance_mode(true, false, false), InstanceMode::Arbitrate);
    // Opted in: neither forwarded to nor claiming the running instance.
    assert_eq!(instance_mode(true, true, true), InstanceMode::QaExempt);
    assert_eq!(instance_mode(true, true, false), InstanceMode::Arbitrate);
}

#[test]
fn qa_hidden_does_not_override_a_host_that_sets_visibility_explicitly() {
    // (explicit, module on, --background, RIGHTKIT_QA_HIDDEN)
    assert!(
        !show_on_ready_decision(None, true, false, true),
        "default still obeys the QA env"
    );
    assert!(show_on_ready_decision(None, true, false, false));
    assert!(
        show_on_ready_decision(Some(true), true, false, true),
        "explicit show wins over QA env"
    );
    assert!(!show_on_ready_decision(Some(false), true, false, false));
    // The login launch and the module switch still apply.
    assert!(!show_on_ready_decision(Some(true), true, true, false));
    assert!(!show_on_ready_decision(Some(true), false, false, false));
}

#[test]
fn lifecycle_only_drops_exactly_the_five_named_modules() {
    let set = ModuleSet::lifecycle_only();
    for dropped in [
        Module::JsHardening,
        Module::NavigationPolicy,
        Module::Presence,
        Module::Watchdog,
        Module::WindowState,
    ] {
        assert!(!set.contains(dropped), "{} must be off", dropped.name());
    }
    // Behaviour change for lifecycle_only only: the host owns its windows, so ShowOnReady is off
    // unless it calls show_on_ready(true). RevealOnReopen stays on; the host opts out of it.
    assert!(!set.contains(Module::ShowOnReady));
    assert!(set.contains(Module::RevealOnReopen));
    assert!(set.contains(Module::Broker));
    // Other sets are unchanged: the full set shows on ready, and state-only keeps reopen.
    assert!(ModuleSet::all().contains(Module::ShowOnReady));
    assert!(ModuleSet::all()
        .without(Module::Watchdog)
        .contains(Module::ShowOnReady));
    assert!(ModuleSet::state_only().contains(Module::RevealOnReopen));
    assert!(!ModuleSet::state_only().contains(Module::ShowOnReady));
    // The full set is untouched.
    assert_eq!(ModuleSet::all().names().len(), Module::ALL.len());
}

#[test]
fn lifecycle_plugin_injects_no_script_and_the_full_plugin_keeps_its_script() {
    let lifecycle = Shell::builder("test-lifecycle").build().lifecycle_plugin();
    assert_eq!(lifecycle.name(), "rightkit-shell");
    assert!(lifecycle.initialization_script_2().is_none());
    assert!(lifecycle.initialization_script().is_none());

    let full = Shell::builder("test-lifecycle").build().plugin();
    assert!(full
        .initialization_script_2()
        .unwrap()
        .script
        .contains("root.__rkShell"));
}

#[test]
fn lifecycle_only_defaults_show_on_ready_and_cache_repair_off_unless_the_host_turns_them_on() {
    use rightkit_tauri_shell::lifecycle::{repair_cache_enabled, show_on_ready_installed};
    // show on ready: (module in set, lifecycle_only, explicit)
    assert!(!show_on_ready_installed(false, true, None));
    assert!(!show_on_ready_installed(false, true, Some(false)));
    assert!(show_on_ready_installed(false, true, Some(true)));
    // Not lifecycle_only: the module alone decides, an explicit call does not resurrect it.
    assert!(show_on_ready_installed(true, false, None));
    assert!(!show_on_ready_installed(false, false, Some(true)));
    // cache repair: (explicit, lifecycle_only)
    assert!(!repair_cache_enabled(None, true));
    assert!(repair_cache_enabled(Some(true), true));
    assert!(!repair_cache_enabled(Some(false), true));
    assert!(repair_cache_enabled(None, false));
    assert!(!repair_cache_enabled(Some(false), false));
}

#[test]
fn the_new_visibility_builders_install_in_every_plugin_shape() {
    for plugin in [
        Shell::builder("test-vis")
            .never_show(true)
            .never_show_env("RIGHTKIT_QA_NATIVE_REAL")
            .reveal_on_reopen(false)
            .build()
            .lifecycle_plugin(),
        Shell::builder("test-vis").never_show(true).build().plugin(),
    ] {
        assert_eq!(plugin.name(), "rightkit-shell");
    }
}

#[test]
#[should_panic(expected = "never_show_env")]
fn an_empty_never_show_environment_name_is_a_startup_error() {
    let _ = Shell::builder("test-vis").never_show_env("");
}
