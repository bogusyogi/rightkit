/// Immutable identity of the provider-visible tool schema catalog for one
/// request boundary. Generation advances only when the exact canonical schema
/// bytes change; unrelated prompt/system changes cannot advance it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCatalogSnapshot {
    pub generation: u64,
    pub digest_hex: String,
}

/// Session-scoped monotonic observer for provider-visible tool definitions.
/// The caller still owns the schema vector; this tracker adds identity only and
/// never becomes a second tool registry.
#[derive(Debug, Default, Clone)]
pub struct ToolCatalogGenerationTracker {
    generation: u64,
    last_digest: Option<[u8; 32]>,
}

impl ToolCatalogGenerationTracker {
    pub fn observe(
        &mut self,
        definitions: &[serde_json::Value],
    ) -> Result<ToolCatalogSnapshot, serde_json::Error> {
        let encoded = serde_json::to_vec(definitions)?;
        let digest: [u8; 32] = rightkit_crypto::sha256(AsRef::<[u8]>::as_ref(&encoded)).into();
        if self.last_digest != Some(digest) {
            self.generation = self.generation.saturating_add(1);
            self.last_digest = Some(digest);
        }
        Ok(ToolCatalogSnapshot {
            generation: self.generation,
            digest_hex: rightkit_crypto::hex_encode(&digest),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod catalog_generation_tests {
    use super::*;

    fn definition(name: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": name,
                "parameters": {"type": "object"}
            }
        })
    }

    #[test]
    fn identical_catalog_keeps_generation_stable() {
        let mut tracker = ToolCatalogGenerationTracker::default();
        let definitions = vec![definition("read_file")];
        let first = tracker.observe(&definitions).unwrap();
        let second = tracker.observe(&definitions).unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 1);
        assert_eq!(first.digest_hex, second.digest_hex);
    }

    #[test]
    fn schema_membership_change_advances_generation_once() {
        let mut tracker = ToolCatalogGenerationTracker::default();
        tracker.observe(&[definition("read_file")]).unwrap();
        let changed = tracker
            .observe(&[definition("read_file"), definition("search")])
            .unwrap();
        assert_eq!(changed.generation, 2);
        assert_eq!(tracker.generation(), 2);
    }

    #[test]
    fn schema_content_change_advances_generation() {
        let mut tracker = ToolCatalogGenerationTracker::default();
        tracker.observe(&[definition("read_file")]).unwrap();
        let mut changed = definition("read_file");
        changed["function"]["description"] = serde_json::Value::String("new description".into());
        assert_eq!(tracker.observe(&[changed]).unwrap().generation, 2);
    }
}
