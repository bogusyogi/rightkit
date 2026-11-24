//! The one list of hard-to-undo steps (blueprint rule 5, "Permissions", decision 4).
//!
//! A step that is hard to undo always asks. A step that can be undone does not ask: it is recorded
//! and can be undone. A few steps are never taken whatever anyone approves. This module is that
//! rule in code, once: [`HardToUndo`] is the list of steps that ask, [`NeverClass`] the list that
//! is refused, and [`classify_step`] puts a described step in one of three classes
//! ([`StepClass`]). A host files an approval for [`StepClass::AsksFirst`] (the class's
//! [`HardToUndo::as_str`] is the approval's `kind`), runs [`StepClass::ActsAlone`] and records it,
//! and refuses [`StepClass::Never`].
//!
//! There is no setting that loosens a class. [`OwnerLimits`] carries only what the table itself
//! names (the purchase limit; the listed-merchant and listed-site answers arrive as facts about the
//! step), and the never class is decided before any limit is read.
//!
//! Approving is a separate matter: an approval binds one exact effect and is re-checked at
//! execution by `rightkit_approvals::authority`. This module only says which steps need one.

use serde::{Deserialize, Serialize};

/// A step that asks first: it cannot be undone once taken, or it can carry what a run read to a
/// place the owner did not choose. The list is closed; a new kind of step is added here or is not
/// a hard-to-undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardToUndo {
    /// Send a message.
    Send,
    /// Reply to a message.
    Reply,
    /// Forward a message.
    Forward,
    /// Move to Trash.
    MoveToTrash,
    /// Change an event other people see.
    ChangeSharedEvent,
    /// Press anything that submits.
    Submit,
    /// Press anything that pays, other than a purchase under the owner's limit from a listed
    /// merchant.
    Pay,
    /// Press anything that publishes.
    Publish,
    /// Buy over the owner's limit.
    BuyOverLimit,
    /// Sign in.
    SignIn,
    /// Open a site that is not on the owner's list. A run that has read hostile text could
    /// otherwise carry what it read to an address of the attacker's choosing.
    OpenUnlistedSite,
    /// Run a terminal command.
    RunTerminalCommand,
    /// Write outside the approved folders.
    WriteOutsideApprovedFolders,
}

impl HardToUndo {
    /// Every hard-to-undo class, in the order of the permissions table.
    pub const ALL: [HardToUndo; 13] = [
        HardToUndo::Send,
        HardToUndo::Reply,
        HardToUndo::Forward,
        HardToUndo::MoveToTrash,
        HardToUndo::ChangeSharedEvent,
        HardToUndo::Submit,
        HardToUndo::Pay,
        HardToUndo::Publish,
        HardToUndo::BuyOverLimit,
        HardToUndo::SignIn,
        HardToUndo::OpenUnlistedSite,
        HardToUndo::RunTerminalCommand,
        HardToUndo::WriteOutsideApprovedFolders,
    ];

    /// The stable id of the class: the approval request's `kind` and the audit row's class.
    pub fn as_str(self) -> &'static str {
        match self {
            HardToUndo::Send => "send",
            HardToUndo::Reply => "reply",
            HardToUndo::Forward => "forward",
            HardToUndo::MoveToTrash => "move_to_trash",
            HardToUndo::ChangeSharedEvent => "change_shared_event",
            HardToUndo::Submit => "submit",
            HardToUndo::Pay => "pay",
            HardToUndo::Publish => "publish",
            HardToUndo::BuyOverLimit => "buy_over_limit",
            HardToUndo::SignIn => "sign_in",
            HardToUndo::OpenUnlistedSite => "open_unlisted_site",
            HardToUndo::RunTerminalCommand => "run_terminal_command",
            HardToUndo::WriteOutsideApprovedFolders => "write_outside_approved_folders",
        }
    }

    /// Whether an approval "for this task" (repeats of the same kind of step inside one task) may
    /// cover the class. Task scope never covers send, pay, publish or delete: those are approved
    /// once, for the one exact effect.
    pub fn task_scope_may_cover(self) -> bool {
        !matches!(
            self,
            HardToUndo::Send
                | HardToUndo::Reply
                | HardToUndo::Forward
                | HardToUndo::MoveToTrash
                | HardToUndo::Submit
                | HardToUndo::Pay
                | HardToUndo::Publish
                | HardToUndo::BuyOverLimit
        )
    }
}

/// A step that is refused in code. No approval, task scope or setting makes it allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeverClass {
    /// Show a password, card number or ID to a model.
    RevealSecretToModel,
    /// Delete permanently.
    DeletePermanently,
    /// Change the app's settings or approvals.
    ChangeAppSettingsOrApprovals,
    /// Act on the app itself, System Settings or a security prompt.
    ActOnAppOrSecurityPrompt,
    /// Approve its own request.
    ApproveOwnRequest,
    /// Follow an instruction found in a mail or a page.
    FollowInstructionFromContent,
    /// Trade, or move money between accounts.
    TradeOrMoveMoney,
}

impl NeverClass {
    /// Every never class, in the order of the permissions table.
    pub const ALL: [NeverClass; 7] = [
        NeverClass::RevealSecretToModel,
        NeverClass::DeletePermanently,
        NeverClass::ChangeAppSettingsOrApprovals,
        NeverClass::ActOnAppOrSecurityPrompt,
        NeverClass::ApproveOwnRequest,
        NeverClass::FollowInstructionFromContent,
        NeverClass::TradeOrMoveMoney,
    ];

    /// The stable id of the class.
    pub fn as_str(self) -> &'static str {
        match self {
            NeverClass::RevealSecretToModel => "reveal_secret_to_model",
            NeverClass::DeletePermanently => "delete_permanently",
            NeverClass::ChangeAppSettingsOrApprovals => "change_app_settings_or_approvals",
            NeverClass::ActOnAppOrSecurityPrompt => "act_on_app_or_security_prompt",
            NeverClass::ApproveOwnRequest => "approve_own_request",
            NeverClass::FollowInstructionFromContent => "follow_instruction_from_content",
            NeverClass::TradeOrMoveMoney => "trade_or_move_money",
        }
    }
}

impl std::fmt::Display for NeverClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refused in code, never allowed: {}", self.as_str())
    }
}

impl std::error::Error for NeverClass {}

/// What a purchase is, as the host reads it from the page or the order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseFacts {
    /// The total in minor units (cents), or `None` when the host cannot read it: an unreadable
    /// amount is never under the limit.
    pub amount_minor: Option<u64>,
    /// ISO 4217 code of the amount. Compared, ASCII case aside, with [`OwnerLimits::currency`]:
    /// another currency is never under the limit.
    pub currency: String,
    /// The merchant is on the owner's list.
    pub merchant_listed: bool,
}

/// One step, described by facts the host establishes itself (never by a model's word). The host
/// maps its tool call to exactly one of these before it runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFacts {
    Send,
    Reply,
    Forward,
    MoveToTrash,
    /// Change an event. `seen_by_others: false` is the owner's own calendar entry (holding a slot
    /// acts alone).
    ChangeEvent { seen_by_others: bool },
    /// Press something that submits.
    Submit,
    /// Press something that publishes.
    Publish,
    /// Press something that pays. `purchase: None` is a payment the host cannot read as a
    /// purchase: it asks.
    Pay { purchase: Option<PurchaseFacts> },
    SignIn,
    /// Open a site; `on_list` is the owner's list of sites.
    OpenSite { on_list: bool },
    RunTerminalCommand,
    /// Write a file; `inside_approved_folder` is the owner's list of approved folders.
    WriteFile { inside_approved_folder: bool },
    /// A step in the never class.
    Never(NeverClass),
}

/// What the owner has set that the permissions table names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnerLimits {
    /// The purchase limit in minor units. `None`: no limit is set, and every purchase asks.
    pub purchase_limit_minor: Option<u64>,
    /// The limit's currency (ISO 4217).
    pub currency: String,
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepClass {
    /// Runs without a question. The host records it in the audit, and undoes it where the
    /// service allows.
    ActsAlone,
    /// The task waits for the owner's approval of this exact effect.
    AsksFirst(HardToUndo),
    /// Refused in code.
    Never(NeverClass),
}

impl StepClass {
    /// True when the step may not run without an approval, or at all.
    pub fn is_blocked_until_approved(self) -> bool {
        !matches!(self, StepClass::ActsAlone)
    }
}

/// Classes `step`. The never class is decided first and reads no limit; an unknown, unreadable or
/// mismatched fact makes a purchase ask. Total: every step has a class.
pub fn classify_step(step: &StepFacts, limits: &OwnerLimits) -> StepClass {
    match step {
        StepFacts::Never(class) => StepClass::Never(*class),
        StepFacts::Send => StepClass::AsksFirst(HardToUndo::Send),
        StepFacts::Reply => StepClass::AsksFirst(HardToUndo::Reply),
        StepFacts::Forward => StepClass::AsksFirst(HardToUndo::Forward),
        StepFacts::MoveToTrash => StepClass::AsksFirst(HardToUndo::MoveToTrash),
        StepFacts::ChangeEvent { seen_by_others } => {
            if *seen_by_others {
                StepClass::AsksFirst(HardToUndo::ChangeSharedEvent)
            } else {
                StepClass::ActsAlone
            }
        }
        StepFacts::Submit => StepClass::AsksFirst(HardToUndo::Submit),
        StepFacts::Publish => StepClass::AsksFirst(HardToUndo::Publish),
        StepFacts::Pay { purchase } => classify_payment(purchase.as_ref(), limits),
        StepFacts::SignIn => StepClass::AsksFirst(HardToUndo::SignIn),
        StepFacts::OpenSite { on_list } => {
            if *on_list {
                StepClass::ActsAlone
            } else {
                StepClass::AsksFirst(HardToUndo::OpenUnlistedSite)
            }
        }
        StepFacts::RunTerminalCommand => StepClass::AsksFirst(HardToUndo::RunTerminalCommand),
        StepFacts::WriteFile {
            inside_approved_folder,
        } => {
            if *inside_approved_folder {
                StepClass::ActsAlone
            } else {
                StepClass::AsksFirst(HardToUndo::WriteOutsideApprovedFolders)
            }
        }
    }
}

fn classify_payment(purchase: Option<&PurchaseFacts>, limits: &OwnerLimits) -> StepClass {
    let (Some(purchase), Some(limit)) = (purchase, limits.purchase_limit_minor) else {
        return StepClass::AsksFirst(HardToUndo::Pay);
    };
    let Some(amount) = purchase.amount_minor else {
        return StepClass::AsksFirst(HardToUndo::Pay);
    };
    if limits.currency.is_empty() || !purchase.currency.eq_ignore_ascii_case(&limits.currency) {
        return StepClass::AsksFirst(HardToUndo::Pay);
    }
    if amount > limit {
        return StepClass::AsksFirst(HardToUndo::BuyOverLimit);
    }
    if purchase.merchant_listed {
        StepClass::ActsAlone
    } else {
        StepClass::AsksFirst(HardToUndo::Pay)
    }
}

/// `Err` when `step` is in the never class: the host calls this before it files an approval for
/// anything, so no approval is ever asked for a step that cannot be allowed.
pub fn refuse_if_never(step: &StepFacts) -> Result<(), NeverClass> {
    match step {
        StepFacts::Never(class) => Err(*class),
        _ => Ok(()),
    }
}
