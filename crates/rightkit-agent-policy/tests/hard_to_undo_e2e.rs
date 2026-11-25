//! End to end through the public classifier: every hard-to-undo step asks, a purchase under the
//! owner's limit from a listed merchant acts alone, the never class is refused whatever the limits
//! say, and the closed list has no gaps.

use rightkit_agent_policy::{
    classify_step, refuse_if_never, HardToUndo, NeverClass, OwnerLimits, PurchaseFacts, StepClass,
    StepFacts,
};

fn limits() -> OwnerLimits {
    OwnerLimits {
        purchase_limit_minor: Some(5_000),
        currency: "USD".into(),
    }
}

fn purchase(amount: Option<u64>, currency: &str, listed: bool) -> StepFacts {
    StepFacts::Pay {
        purchase: Some(PurchaseFacts {
            amount_minor: amount,
            currency: currency.into(),
            merchant_listed: listed,
        }),
    }
}

#[test]
fn every_hard_to_undo_step_asks_first() {
    let cases = [
        (StepFacts::Send, HardToUndo::Send),
        (StepFacts::Reply, HardToUndo::Reply),
        (StepFacts::Forward, HardToUndo::Forward),
        (StepFacts::MoveToTrash, HardToUndo::MoveToTrash),
        (
            StepFacts::ChangeEvent {
                seen_by_others: true,
            },
            HardToUndo::ChangeSharedEvent,
        ),
        (StepFacts::Submit, HardToUndo::Submit),
        (StepFacts::Publish, HardToUndo::Publish),
        (StepFacts::Pay { purchase: None }, HardToUndo::Pay),
        (purchase(Some(5_001), "USD", true), HardToUndo::BuyOverLimit),
        (StepFacts::SignIn, HardToUndo::SignIn),
        (
            StepFacts::OpenSite { on_list: false },
            HardToUndo::OpenUnlistedSite,
        ),
        (
            StepFacts::RunTerminalCommand,
            HardToUndo::RunTerminalCommand,
        ),
        (
            StepFacts::WriteFile {
                inside_approved_folder: false,
            },
            HardToUndo::WriteOutsideApprovedFolders,
        ),
    ];
    let mut seen = Vec::new();
    for (step, class) in cases {
        assert_eq!(classify_step(&step, &limits()), StepClass::AsksFirst(class));
        seen.push(class);
    }
    for class in HardToUndo::ALL {
        assert!(seen.contains(&class), "{} has no case", class.as_str());
    }
}

#[test]
fn reversible_steps_act_alone_and_a_small_listed_purchase_goes_through() {
    let alone = [
        StepFacts::ChangeEvent {
            seen_by_others: false,
        },
        StepFacts::OpenSite { on_list: true },
        StepFacts::WriteFile {
            inside_approved_folder: true,
        },
        purchase(Some(5_000), "usd", true),
        purchase(Some(1), "USD", true),
    ];
    for step in alone {
        assert_eq!(
            classify_step(&step, &limits()),
            StepClass::ActsAlone,
            "{step:?}"
        );
    }
}

#[test]
fn a_purchase_asks_when_any_fact_is_missing_or_off() {
    let asks_pay = [
        purchase(Some(100), "USD", false),
        purchase(None, "USD", true),
        purchase(Some(100), "EUR", true),
    ];
    for step in asks_pay {
        assert_eq!(
            classify_step(&step, &limits()),
            StepClass::AsksFirst(HardToUndo::Pay),
            "{step:?}"
        );
    }
    let no_limit = OwnerLimits::default();
    assert_eq!(
        classify_step(&purchase(Some(1), "USD", true), &no_limit),
        StepClass::AsksFirst(HardToUndo::Pay)
    );
}

#[test]
fn the_never_class_is_refused_whatever_the_limits() {
    let generous = OwnerLimits {
        purchase_limit_minor: Some(u64::MAX),
        currency: "USD".into(),
    };
    for class in NeverClass::ALL {
        let step = StepFacts::Never(class);
        assert_eq!(classify_step(&step, &generous), StepClass::Never(class));
        assert_eq!(
            classify_step(&step, &OwnerLimits::default()),
            StepClass::Never(class)
        );
        assert_eq!(refuse_if_never(&step), Err(class));
    }
    assert_eq!(refuse_if_never(&StepFacts::Send), Ok(()));
}

#[test]
fn task_scope_never_covers_send_pay_publish_or_delete() {
    for class in [
        HardToUndo::Send,
        HardToUndo::Reply,
        HardToUndo::Forward,
        HardToUndo::MoveToTrash,
        HardToUndo::Submit,
        HardToUndo::Pay,
        HardToUndo::Publish,
        HardToUndo::BuyOverLimit,
    ] {
        assert!(!class.task_scope_may_cover(), "{}", class.as_str());
    }
    assert!(HardToUndo::RunTerminalCommand.task_scope_may_cover());
}
