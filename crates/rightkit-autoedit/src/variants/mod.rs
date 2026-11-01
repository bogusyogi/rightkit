//! CutRight variants (M2): named cut variants of one source, kept as separate
//! timeline documents under the project, and word-locked A/B comparison.
//!
//! Port of the old per-variant artifacts (`edit/cut-plan-<variant>.json`,
//! `render/rough-cuts/<variant>.mp4`, `select_variant`) onto the revision-backed
//! timeline: a variant is `variants/<name>/{timeline.json, plan.json,
//! variant.json}`. The main timeline is never touched except by
//! [`promote_variant`], which is an undoable action group.

mod compare;
mod error;
mod variant;

pub use compare::{
    build_compare_render, compare_variants, execute_compare, BuiltCompare, CompareAudio,
    CompareLayout, CompareRenderRequest, CompareReport, SeekEntry, SeekMap, Side, VariantSummary,
    WordRow, WordStatus, COMPARE_SCHEMA,
};
pub use error::VariantError;
pub use variant::{
    create_preset_variants, create_variant, delete_variant, list_variants, load_variant,
    promote_variant, save_child_variant, save_variant, source_info_for, transcript_blake3,
    undo_promotion, validate_variant_name, ParentLink, Promotion, Provenance, Variant,
    VARIANT_KIND,
};
