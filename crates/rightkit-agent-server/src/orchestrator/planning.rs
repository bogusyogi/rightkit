/// Build the planner's system prompt, advertising the available agent roles and
/// reusable team presets so the model can only assign work to agents that
/// actually exist while still respecting user-defined collaboration patterns.
pub fn planner_system_prompt(
    registry: &AgentRegistry,
    teams: &[rightkit_agent_coding_tools::TeamDefinition],
    budget_note: Option<&str>,
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
        "No user-defined team presets are currently registered.\n".to_string()
    } else {
        format!(
            "Team presets (team presets are not roles; they are reusable planner hints):\n\
             {team_templates}\
             If the goal names a team or clearly matches a team description, prefer tasks assigned \
             to that team's member roles and respect any listed presets as execution constraints. \
             Do not assign work to the team name itself; assign work only to concrete agent roles \
             from the roster above.\n"
        )
    };
    let budget_block = budget_note
        .map(|note| format!("{note}\n\n"))
        .unwrap_or_default();
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
         {budget_block}\
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

        // A plan that names a role outside the roster fails here, before any
        // worker runs (orchestration rule 39), instead of leaving a task that
        // can never be routed.
        if registry.get(&role).is_none() {
            return Err(format!("planner assigned unknown role {role}"));
        }

        // The planner's `done_when` text is the task's completion predicate
        // (orchestration rule 8, D2). Known forms (`tests pass`, `compiles`,
        // `contains:<text>`, `has_citations`, ...) are evaluated against the
        // worker's output; any other text falls back to the worker's success,
        // which is the evaluator's documented free-text behaviour. A task that
        // states no `done_when` has no predicate and stays unverified.
        let done_when_predicates = if done_when.trim().is_empty() {
            vec![]
        } else {
            vec![rightkit_agent_engine_protocol::DoneWhenPredicate {
                predicate: done_when.trim().to_string(),
                weight: 1.0,
            }]
        };

        tasks.push(TaskNode {
            id,
            title,
            deps,
            done_when,
            done_when_predicates,
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
    budget_note: Option<&str>,
) -> Result<Vec<TaskNode>, String> {
    let user = match memory {
        Some(m) => format!("Goal: {goal}\n\nRelevant experience from past runs:\n{m}"),
        None => format!("Goal: {goal}"),
    };
    let messages = vec![
        TranscriptMessage::System {
            id: format!("sys-{}", now_millis()),
            created_at: now_iso(),
            content: planner_system_prompt(registry, teams, budget_note),
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
