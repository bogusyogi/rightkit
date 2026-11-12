/// Build the planner's system prompt, advertising the available agent roles and
/// reusable team templates so the model can only assign work to agents that
/// actually exist while still respecting user-defined collaboration patterns.
pub fn planner_system_prompt(
    registry: &AgentRegistry,
    teams: &[rightkit_agent_coding_tools::TeamDefinition],
) -> String {
    let mut roles = String::new();
    for a in registry.all() {
        roles.push_str(&format!("- {}: {}\n", a.role, a.tool_description));
    }
    let mut team_templates = String::new();
    for team in teams {
        let members = team.members.join(", ");
        let description = team.description.as_deref().unwrap_or("no description");
        let mut presets = Vec::new();
        if let Some(max_workers) = team.max_workers {
            presets.push(format!("maxWorkers: {max_workers}"));
        }
        if let Some(budget_usd) = team.budget_usd {
            presets.push(format!("budgetUsd: {budget_usd}"));
        }
        let preset_suffix = if presets.is_empty() {
            String::new()
        } else {
            format!("; presets: {}", presets.join(", "))
        };
        team_templates.push_str(&format!(
            "- {}: {}; members: {}{}\n",
            team.name, description, members, preset_suffix
        ));
    }
    let team_block = if team_templates.is_empty() {
        "No user-defined team templates are currently registered.\n".to_string()
    } else {
        format!(
            "Team templates (team templates are not roles; they are reusable planner hints):\n\
             {team_templates}\
             If the goal names a team or clearly matches a team description, prefer tasks assigned \
             to that team's member roles and respect any listed presets as execution constraints. \
             Do not assign work to the team name itself; assign work only to concrete agent roles \
             from the roster above.\n"
        )
    };
    format!(
        "You are the Conductor's planner. Decompose the user's goal into the smallest set of \
         concrete tasks that genuinely achieves it. Assign each task to exactly one agent role \
         from the roster below, and express real ordering with dependencies (a task lists the ids \
         of tasks that must finish first). Surface genuine parallelism but do not invent it. \
         For code changes and refactors, include an explicit first task to inspect the affected \
         current files before any edit task; if behavior must be preserved, capture the baseline \
         behavior before editing. \
         Always include a final verification task assigned to the 'signoff' role that depends on \
         all implementation tasks.\n\n\
         Available agent roles:\n{roles}\n\
         {team_block}\n\
         Respond with ONLY a JSON object, no prose, in exactly this shape:\n\
         {{\"tasks\":[{{\"id\":\"t1\",\"title\":\"...\",\"role\":\"coder\",\"deps\":[],\"done_when\":\"...\"}}]}}"
    )
}

/// Parse the planner's JSON response into task nodes. Tolerant of markdown code
/// fences and surrounding prose: it extracts the outermost JSON object.
pub fn parse_plan(raw: &str, registry: &AgentRegistry) -> Result<Vec<TaskNode>, String> {
    let json = extract_json_object(raw).ok_or("planner did not return a JSON object")?;
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("planner JSON parse error: {e}"))?;
    let arr = value
        .get("tasks")
        .and_then(|t| t.as_array())
        .ok_or("planner JSON missing 'tasks' array")?;
    if arr.is_empty() {
        return Err("planner produced no tasks".into());
    }

    let now = now_iso();
    let mut tasks = Vec::with_capacity(arr.len());
    let mut seen = std::collections::HashSet::new();
    for (i, t) in arr.iter().enumerate() {
        let id = t
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("t{}", i + 1));
        if !seen.insert(id.clone()) {
            return Err(format!("planner produced duplicate task id: {id}"));
        }
        let title = t
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let role = t
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let deps = t
            .get("deps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let done_when = t
            .get("done_when")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Unknown roles are kept; the router will fail them cleanly rather than
        // silently dropping work. A warning is the best we can do here.
        if registry.get(&role).is_none() {
            eprintln!("[planner] task {id} assigned unknown role '{role}'");
        }

        tasks.push(TaskNode {
            id,
            title,
            deps,
            done_when,
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: String::new(),
            estimated_duration: String::new(),
            escalation_path: registry
                .get(&role)
                .map(|a| a.escalation_path.join(","))
                .unwrap_or_default(),
            assigned_role: role,
            open_questions: vec![],
            created_at: now.clone(),
            updated_at: now.clone(),
        });
    }
    Ok(tasks)
}

/// Extract the outermost `{...}` JSON object from a possibly-fenced response.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

/// Ask the conductor's model to decompose `goal` into a task graph.
pub async fn plan_goal(
    llm: &dyn LlmClient,
    goal: &str,
    memory: Option<&str>,
    registry: &AgentRegistry,
    teams: &[rightkit_agent_coding_tools::TeamDefinition],
) -> Result<Vec<TaskNode>, String> {
    let user = match memory {
        Some(m) => format!("Goal: {goal}\n\nRelevant experience from past runs:\n{m}"),
        None => format!("Goal: {goal}"),
    };
    let messages = vec![
        TranscriptMessage::System {
            id: format!("sys-{}", now_millis()),
            created_at: now_iso(),
            content: planner_system_prompt(registry, teams),
            pinned: true,
        },
        TranscriptMessage::User {
            id: format!("user-{}", now_millis()),
            created_at: now_iso(),
            content: user,
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        },
    ];
    let resp = llm.complete(&messages).await?;
    parse_plan(&resp.text, registry)
}
