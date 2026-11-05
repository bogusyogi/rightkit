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
        let digest: [u8; 32] = rightkit_crypto::sha256(AsRef::<[u8]>::as_ref(&encoded));
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
