use crate::api::DaemonState;
use rightkit_bot_messaging::RemoteChannelAdapter;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::warn;

use super::execution::sanitize_id;
use super::model::{DeliveryOutcome, DeliveryTarget, ScheduleEntry};
use super::time::current_epoch_millis;

include!("delivery/targets.rs");
include!("delivery/remote.rs");
include!("delivery/telegram.rs");
include!("delivery/files.rs");
