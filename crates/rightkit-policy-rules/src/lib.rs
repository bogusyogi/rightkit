#![forbid(unsafe_code)]
//! Agent policy rules: the policy model (capabilities, effects, contexts,
//! packs, decisions) and its evaluator with precedence, frozen scopes and
//! explanations; and analysis rule packs (`analysis`): lexical and structural
//! rule compilation and evaluation with evidence spans.

pub mod analysis;
pub mod model;
pub mod rules;
