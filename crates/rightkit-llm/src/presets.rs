//! Known provider endpoints. All but Anthropic speak the OpenAI chat protocol.

use std::sync::Arc;

use rightkit_http::Client;

use crate::anthropic::{AnthropicProvider, ANTHROPIC_BASE_URL};
use crate::credentials::CredentialSource;
use crate::openai::{MaxTokensField, OpenAiCompatProvider};
use crate::provider::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    Anthropic,
    OpenAiCompat,
}

#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub wire: Wire,
    pub base_url: &'static str,
    pub requires_credential: bool,
    pub max_tokens_field: MaxTokensField,
}

const fn p(
    id: &'static str,
    wire: Wire,
    base_url: &'static str,
    requires_credential: bool,
) -> Preset {
    Preset {
        id,
        wire,
        base_url,
        requires_credential,
        max_tokens_field: MaxTokensField::MaxTokens,
    }
}

pub const PRESETS: &[Preset] = &[
    p("anthropic", Wire::Anthropic, ANTHROPIC_BASE_URL, true),
    Preset {
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        ..p(
            "openai",
            Wire::OpenAiCompat,
            "https://api.openai.com/v1",
            true,
        )
    },
    p(
        "openrouter",
        Wire::OpenAiCompat,
        "https://openrouter.ai/api/v1",
        true,
    ),
    p(
        "groq",
        Wire::OpenAiCompat,
        "https://api.groq.com/openai/v1",
        true,
    ),
    p(
        "deepseek",
        Wire::OpenAiCompat,
        "https://api.deepseek.com/v1",
        true,
    ),
    p(
        "together",
        Wire::OpenAiCompat,
        "https://api.together.xyz/v1",
        true,
    ),
    p(
        "mistral",
        Wire::OpenAiCompat,
        "https://api.mistral.ai/v1",
        true,
    ),
    p("xai", Wire::OpenAiCompat, "https://api.x.ai/v1", true),
    p(
        "gemini",
        Wire::OpenAiCompat,
        "https://generativelanguage.googleapis.com/v1beta/openai",
        true,
    ),
    p(
        "ollama",
        Wire::OpenAiCompat,
        "http://127.0.0.1:11434/v1",
        false,
    ),
    p(
        "lmstudio",
        Wire::OpenAiCompat,
        "http://127.0.0.1:1234/v1",
        false,
    ),
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Build a provider for a preset; `base_url` overrides the endpoint (self-hosted
/// gateways, tests).
pub fn build(
    id: &str,
    base_url: Option<&str>,
    client: Client,
    credentials: Arc<dyn CredentialSource>,
) -> Option<Arc<dyn Provider>> {
    let p = preset(id)?;
    let url = base_url.unwrap_or(p.base_url);
    Some(match p.wire {
        Wire::Anthropic => Arc::new(AnthropicProvider::new(url, client, credentials).with_id(p.id)),
        Wire::OpenAiCompat => {
            let mut prov = OpenAiCompatProvider::new(p.id, url, client, credentials)
                .with_max_tokens_field(p.max_tokens_field);
            if !p.requires_credential {
                prov = prov.without_credential();
            }
            Arc::new(prov)
        }
    })
}
