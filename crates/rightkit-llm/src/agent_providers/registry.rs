//! Model name → route mapping registry.

use std::collections::HashMap;

/// How a route's provider is authenticated. MiniMax is endpoint plus key like every provider
/// without a reusable sign-in, so there is no quota-drawing mode.
///
/// `Default` is `ApiKey` so every pre-existing [`ModelRoute`] built via
/// [`ModelRoute::new`] — which does not mention auth mode — keeps its prior,
/// implicit behavior unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[default]
    ApiKey,
    OAuth,
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
    /// use [`ModelRoute::with_auth_mode`] to declare `OAuth`.
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

    /// Builder-style setter for [`AuthMode`], so a route can declare its auth mode without
    /// changing the `new()` signature or breaking any existing positional call site.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route(name: &str, priority: u8) -> ModelRoute {
        ModelRoute::new(
            name,
            "anthropic",
            "https://api.anthropic.com/v1",
            "claude-opus-4-8-20251101",
            priority,
        )
    }

    #[test]
    fn model_route_registry_lookup_by_name() {
        let mut reg = ModelRegistry::new();
        reg.register(sample_route("claude-opus-4-8", 0));

        let route = reg.get("claude-opus-4-8").expect("route not found");
        assert_eq!(route.name, "claude-opus-4-8");
        assert_eq!(route.provider, "anthropic");
        assert_eq!(route.priority, 0);
    }

    #[test]
    fn model_route_registry_returns_none_for_unknown() {
        let reg = ModelRegistry::new();
        assert!(reg.get("gpt-9000").is_none());
    }

    #[test]
    fn model_route_new_defaults_to_api_key_auth_mode() {
        // Source-compatibility proof for ENG-88: the pre-existing positional
        // `new()` constructor, unchanged, must still compile and must yield
        // the same implicit behavior every prior caller relied on.
        let route = sample_route("legacy-caller", 0);
        assert_eq!(route.auth_mode, AuthMode::ApiKey);
    }

    #[test]
    fn model_route_with_auth_mode_declares_oauth() {
        let route = sample_route("image-caller", 0).with_auth_mode(AuthMode::OAuth);
        assert_eq!(route.auth_mode, AuthMode::OAuth);
        // Original fields are preserved by the builder.
        assert_eq!(route.name, "image-caller");
    }

    #[test]
    fn model_route_registry_priority_ordering() {
        let mut reg = ModelRegistry::new();
        reg.register(sample_route("route-b", 5));
        reg.register(sample_route("route-a", 1));
        reg.register(sample_route("route-c", 10));

        let ordered = reg.routes_by_priority();
        assert_eq!(ordered[0].name, "route-a");
        assert_eq!(ordered[1].name, "route-b");
        assert_eq!(ordered[2].name, "route-c");
    }
}
