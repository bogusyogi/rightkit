# rightkit-agent-host-traits

Host traits for RightKit agent runtimes.

Skills, skill loaders and slash-command catalogs belong to the host product. RightKit agent crates do not parse, discover or evaluate skills; they call the [`SkillCatalog`] the host installs once at start up.

```rust
use std::sync::Arc;
use rightkit_agent_host_traits::{install_skill_catalog, SkillCatalog};

fn start(catalog: Arc<dyn SkillCatalog>) {
    install_skill_catalog(catalog);
}
```

With no catalog installed every query answers "no skills".

## API

| Item | Purpose |
| --- | --- |
| `SkillCatalog` | Trait the host implements: `parse_manifest`, `triggered_skills`, `list_catalog`. |
| `install_skill_catalog`, `clear_skill_catalog`, `skill_catalog` | Process-wide installation point. |
| `TriggerContext`, `TriggeredSkill`, `SkillEffects`, `SkillManifestSummary`, `SkillCatalogRoot` | Plain data passed across the trait. |

Used by `rightkit-agent-engine-core`, `rightkit-agent-tools`, `rightkit-agent-orchestration` and `rightkit-agent-engine-mobile-runtime`.
