#![cfg(feature = "vector")]

use rightkit_embed::{cosine_similarity, VectorStore};

#[test]
fn sqlite_vectors_round_trip_and_rank_by_cosine() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vectors.sqlite");
    {
        let mut store = VectorStore::open(&path, 3).unwrap();
        store
            .put_batch([
                ("near", "gemma-v1", &[1.0, 0.0, 0.0][..]),
                ("middle", "gemma-v1", &[0.7, 0.7, 0.0][..]),
                ("other-model", "bge-v1", &[1.0, 0.0, 0.0][..]),
            ])
            .unwrap();
        let nearest = store.nearest("gemma-v1", &[1.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(
            nearest
                .iter()
                .map(|row| row.key.as_str())
                .collect::<Vec<_>>(),
            ["near", "middle"]
        );
        assert!(nearest[0].score > nearest[1].score);
        assert_eq!(
            store.get("near").unwrap().unwrap().vector,
            vec![1.0, 0.0, 0.0]
        );
        assert_eq!(store.len().unwrap(), 3);
    }
    let store = VectorStore::open(&path, 3).unwrap();
    assert_eq!(
        store.nearest("gemma-v1", &[1.0, 0.0, 0.0], 1).unwrap()[0].key,
        "near"
    );
    assert_eq!(store.clear_model("bge-v1").unwrap(), 1);
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
}

#[test]
fn vectors_reject_wrong_dimensions_and_non_finite_values() {
    let store = VectorStore::in_memory(2).unwrap();
    assert!(store.put("bad", "model", &[1.0]).is_err());
    assert!(store.put("bad", "model", &[1.0, f32::NAN]).is_err());
}
