fn require_schedule_admin_for_mutation(ctx: &ToolContext) -> Result<(), String> {
    if ctx.mcp_channel == "schedule"
        && ctx.toolset.as_deref().map(str::trim) != Some(SCHEDULE_ADMIN_TOOLSET)
    {
        return Err(
            "schedule_cron mutation from scheduled sessions requires the schedule-admin toolset"
                .to_string(),
        );
    }
    Ok(())
}

fn require_schedule_write_authorization(ctx: &ToolContext) -> Result<(), String> {
    if rightkit_agent_policy::durable_write_is_authorized(
        &ctx.content_provenance,
        ctx.durable_write_approved,
    ) {
        return Ok(());
    }
    Err(format!(
        "schedule durable write influenced by {} content from '{}' requires explicit approval",
        ctx.content_provenance.origin.label(),
        ctx.content_provenance.source
    ))
}

fn optional_string_arg(args: &Value, key: &str) -> Option<String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn path_stays_inside(path: &Path, root: &Path) -> bool {
    normalize_lexical(path).starts_with(normalize_lexical(root))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn string_array_arg(args: &Value, key: &str) -> Vec<String> {
    args[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the delivery-target list from tool args, gating `allowPrivateNetwork`
/// behind an explicit permission decision. `allowPrivateNetwork` opts a webhook
/// delivery out of the net_guard SSRF check (`gateway.rs::validated_delivery_url`)
/// — an agent/tool-supplied flag alone must not silently grant that, so it is
/// evaluated as a Destructive-class capability call: Bypass mode allows it,
/// every other mode escalates to Ask (the same "explicit approval" contract
/// `write_file`/`shell` use for their own escalations).
fn delivery_arg(args: &Value, ctx: &ToolContext) -> Result<Vec<DeliveryTarget>, String> {
    let items = match args["delivery"].as_array() {
        Some(items) => items,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(kind) = item["kind"].as_str().map(str::trim) else {
            continue;
        };
        if kind.is_empty() {
            continue;
        }
        let allow_private_network = item["allowPrivateNetwork"].as_bool().unwrap_or(false);
        if allow_private_network {
            require_private_network_approval(ctx)?;
        }
        out.push(DeliveryTarget {
            kind: kind.to_string(),
            target: optional_string_arg(item, "target"),
            allow_private_network,
        });
    }
    Ok(out)
}

fn require_private_network_approval(ctx: &ToolContext) -> Result<(), String> {
    let verdict = rightkit_agent_policy::PermissionEngine::new().evaluate(
        rightkit_agent_policy::CapabilityClass::Network,
        ctx.permission_mode,
        ctx.workspace_trust,
        None,
        true, // destructive=true: force the Allow->Ask escalation outside Bypass
    );
    match verdict.decision {
        rightkit_agent_policy::PermissionDecision::Deny => Err(format!(
            "delivery allowPrivateNetwork denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        rightkit_agent_policy::PermissionDecision::Ask => Err(format!(
            "delivery allowPrivateNetwork requires explicit approval: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        rightkit_agent_policy::PermissionDecision::Allow => Ok(()),
    }
}
