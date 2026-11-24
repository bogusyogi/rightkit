//! The areas of a profile. One file per area is the plan (ME-3); this crate names them and fixes their
//! stable slugs so an export, a sync record and a file name never disagree.

use crate::entry::Sensitivity;
use serde::{Deserialize, Serialize};

/// An area of the owner's life, in the order the plan lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Identity,
    Work,
    Jobs,
    Now,
    People,
    Health,
    Money,
    HomeTravel,
    Routines,
    Writing,
}

impl Area {
    pub const ALL: [Area; 10] = [
        Area::Identity,
        Area::Work,
        Area::Jobs,
        Area::Now,
        Area::People,
        Area::Health,
        Area::Money,
        Area::HomeTravel,
        Area::Routines,
        Area::Writing,
    ];

    /// The areas the identity card reads from: who you are, what you run, what you are doing now, key
    /// people. A run gets further areas by its task's kind, which is a table the host owns (open point 6 of
    /// the Nucleus contract), so this crate does not pick them.
    pub const CARD: [Area; 4] = [Area::Identity, Area::Work, Area::Now, Area::People];

    /// The stable lower-case name used in ids, exports and file names.
    pub fn slug(self) -> &'static str {
        match self {
            Area::Identity => "identity",
            Area::Work => "work",
            Area::Jobs => "jobs",
            Area::Now => "now",
            Area::People => "people",
            Area::Health => "health",
            Area::Money => "money",
            Area::HomeTravel => "home_travel",
            Area::Routines => "routines",
            Area::Writing => "writing",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Area> {
        Area::ALL.into_iter().find(|area| area.slug() == slug)
    }

    /// The least sensitivity an entry of this area can have. Health and money entries are switched whatever
    /// label the writer gave them, so a wrong label cannot send them to a model.
    pub fn floor(self) -> Sensitivity {
        match self {
            Area::Health => Sensitivity::Health,
            Area::Money => Sensitivity::Money,
            _ => Sensitivity::Ordinary,
        }
    }
}
