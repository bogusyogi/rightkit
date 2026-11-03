//! Public contract of the pure parts: the closed relation vocabulary, token estimates, and quantised cosine.

use rightkit_memory_core::{
    estimate_tokens, is_canonical_relation, is_observation_relation, quantized_cosine,
    relation_category, QuantizedVector, RelationCategory,
};

#[test]
fn an_empty_text_has_zero_tokens_and_any_text_at_least_one() {
    assert_eq!(estimate_tokens(""), 0);
    assert!(estimate_tokens("a") >= 1);
    assert!(estimate_tokens("one two three four five six") > estimate_tokens("one"));
}

#[test]
fn identical_vectors_have_cosine_close_to_one_and_orthogonal_ones_close_to_zero() {
    let a = [1.0_f32, 0.0, 0.0, 0.0];
    let b = [0.0_f32, 1.0, 0.0, 0.0];
    assert!((quantized_cosine(&a, &a) - 1.0).abs() < 0.02);
    assert!(quantized_cosine(&a, &b).abs() < 0.02);
}

#[test]
fn a_quantised_vector_survives_its_byte_round_trip() {
    let vector = QuantizedVector::quantize(&[0.25, -0.5, 0.75, 1.0]);
    let restored = QuantizedVector::from_bytes(&vector.to_bytes()).expect("own bytes decode");
    assert_eq!(restored.values(), vector.values());
    assert_eq!(restored.scale(), vector.scale());
    assert!(vector.cosine_with(&[0.25, -0.5, 0.75, 1.0]) > 0.99);
    assert!(QuantizedVector::from_bytes(&[1, 2]).is_none());
}

#[test]
fn only_the_closed_relation_vocabulary_is_traversable_and_derived_is_never_an_observation() {
    assert!(is_canonical_relation("supersedes"));
    assert!(!is_canonical_relation("related_to"));
    assert_eq!(
        relation_category("enriches"),
        Some(RelationCategory::Enrichment)
    );
    assert_eq!(relation_category("related_to"), None);
    assert!(is_observation_relation("supports"));
    assert!(!is_observation_relation("derived_from"));
}
