#![forbid(unsafe_code)]
//! Agent policy rules: the policy model (capabilities, effects, contexts,
//! packs, decisions) and its evaluator with precedence, frozen scopes and
//! explanations.

pub mod model;
pub mod rules;
