use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnAgentOutput {
    pub agent_id: String,
    pub nickname: String,
}

pub fn parse_spawn_agent_output(output: &str) -> Option<SpawnAgentOutput> {
    let parsed: Value = serde_json::from_str(output).ok()?;
    let agent_id = parsed.get("agent_id")?.as_str()?.to_string();
    if agent_id.is_empty() {
        return None;
    }

    let nickname = parsed
        .get("nickname")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(SpawnAgentOutput { agent_id, nickname })
}

/// The task path a multi-agent v2 `spawn_agent` call returns, e.g. `/root/batch_1` from
/// `{"task_name":"/root/batch_1"}`.
///
/// Codex v0.153.x spawns report only where the new agent sits in the task tree — no
/// `agent_id`, no nickname — so this is the only sign the spawn went through. The spawned
/// session's own `session_meta.parent_thread_id` is what links it back to its parent.
pub fn parse_spawn_task_name(output: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(output).ok()?;
    let task_name = parsed.get("task_name")?.as_str()?;
    if task_name.is_empty() {
        return None;
    }
    Some(task_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spawn_agent_output() {
        let parsed = parse_spawn_agent_output(
            r#"{"agent_id":"019dcd48-57d3-7a42-9952-bb488d179d0f","nickname":"Parfit"}"#,
        )
        .unwrap();

        assert_eq!(parsed.agent_id, "019dcd48-57d3-7a42-9952-bb488d179d0f");
        assert_eq!(parsed.nickname, "Parfit");
    }

    #[test]
    fn parses_multi_agent_v2_task_name() {
        // Codex v0.153.x spawn_agent output — a task path and nothing else.
        assert_eq!(
            parse_spawn_task_name(r#"{"task_name":"/root/batch_1"}"#).as_deref(),
            Some("/root/batch_1")
        );
    }

    #[test]
    fn ignores_spawn_output_without_task_name() {
        assert!(
            parse_spawn_task_name(r#"{"agent_id":"019dcd48-57d3-7a42-9952-bb488d179d0f"}"#)
                .is_none()
        );
        assert!(parse_spawn_task_name(r#"{"task_name":""}"#).is_none());
        assert!(parse_spawn_task_name("spawn failed: no capacity").is_none());
    }

    #[test]
    fn ignores_non_json_spawn_agent_output() {
        assert!(
            parse_spawn_agent_output("Full-history forked agents inherit parent config").is_none()
        );
    }
}
