//! Model name → route mapping registry.

use std::collections::HashMap;

/// How a route's provider is authenticated/billed. ENG-88: distinct from a
/// plain API key or interactive OAuth, a `TokenPlan` route draws down a
/// pre-purchased quota (see [`crate::agent_providers::token_plan_auth`]) rather than being
/// billed per call.
///
/// `Default` is `ApiKey` so every pre-existing [`ModelRoute`] built via
/// [`ModelRoute::new`] — which does not mention auth mode — keeps its prior,
/// implicit behavior unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[default]
    ApiKey,
    OAuth,
    TokenPlan,
}

/// A single provider route that maps a model name to an API endpoint.
#[derive(Debug, Clone)]
pub struct ModelRoute {
    /// Human-readable route name, e.g. `"claude-opus-4-8"`.
    pub name: String,
    /// Provider slug, e.g. `"anthropic"`.
    pub provider: String,
    /// Base URL for the provider, e.g. `"https://api.anthropic.com/v1"`.
    pub base_url: String,
    /// Provider-specific model ID, e.g. `"claude-opus-4-8-20251101"`.
    pub model_id: String,
    /// Route priority — lower value = preferred.
    pub priority: u8,
    /// How this route authenticates. Additive field (ENG-88): defaults to
    /// [`AuthMode::ApiKey`] via [`ModelRoute::new`] so every existing call
    /// site compiles and behaves exactly as before; opt into a different
    /// mode with [`ModelRoute::with_auth_mode`].
    pub auth_mode: AuthMode,
}

impl ModelRoute {
    /// Create a new `ModelRoute`. Auth mode defaults to [`AuthMode::ApiKey`];
    /// use [`ModelRoute::with_auth_mode`] to declare `OAuth` or `TokenPlan`.
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        base_url: impl Into<String>,
        model_id: impl Into<String>,
        priority: u8,
    ) -> Self {
        Self {
            name: name.into(),
            provider: provider.into(),
            base_url: base_url.into(),
            model_id: model_id.into(),
            priority,
            auth_mode: AuthMode::default(),
        }
    }

    /// Builder-style setter for [`AuthMode`], added alongside [`AuthMode`]
    /// itself so ENG-88 (token-plan billing) can declare a route's auth mode
    /// without changing the `new()` signature or breaking any existing
    /// positional call site.
    pub fn with_auth_mode(mut self, auth_mode: AuthMode) -> Self {
        self.auth_mode = auth_mode;
        self
    }
}

/// A registry that maps model names to their routes.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    routes: HashMap<String, ModelRoute>,
}

impl ModelRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a route.
    pub fn register(&mut self, route: ModelRoute) {
        self.routes.insert(route.name.clone(), route);
    }

    /// Look up a route by model name.
    pub fn get(&self, name: &str) -> Option<&ModelRoute> {
        self.routes.get(name)
    }

    /// Return all routes, sorted by priority (lowest first).
    pub fn routes_by_priority(&self) -> Vec<&ModelRoute> {
        let mut routes: Vec<&ModelRoute> = self.routes.values().collect();
        routes.sort_by_key(|r| r.priority);
        routes
    }
}
