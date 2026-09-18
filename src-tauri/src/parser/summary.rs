//! Lightweight per-turn index for the session list.
//!
//! A session's bulk is in its tool calls: command output, file contents, patch
//! bodies. The list view never renders any of it — a row shows the user
//! message, one agent preview line, and a few counts — so shipping whole turns
//! to the frontend just to draw that list held the entire transcript in the JS
//! heap at once.
//!
//! [`TurnSummary`] carries exactly what a row draws. Bodies are fetched one
//! turn at a time when the user opens the detail view (see
//! `commands::session::load_turn`).

use serde::{Deserialize, Serialize};

use crate::parser::session::CodexSession;
use crate::parser::turn::{AgentMsg, CodexTurn, TurnStatus};

/// Everything the list view draws for one turn, and nothing else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSummary {
    pub turn_id: String,
    pub status: TurnStatus,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub duration_ms: Option<u64>,
    /// Shown in full: a prompt is small next to the tool output it triggers.
    pub user_message: Option<String>,
    /// The final answer when there is one, else the first non-reasoning message
    /// — the same fallback the row used to compute for itself.
    pub agent_preview: Option<String>,
    /// Timestamp of the last agent message, used when `completed_at` is absent.
    pub last_agent_timestamp: Option<String>,
    pub tool_call_count: usize,
    pub reasoning_count: usize,
    /// Just the number the row prints, not the whole `TokenInfo`.
    pub total_tokens: Option<u64>,
    pub model: Option<String>,
    /// Whether opening the detail view would show anything.
    pub has_detail: bool,
}

/// The preview line for a turn: the final answer, else the first message that
/// is not reasoning.
fn agent_preview(messages: &[AgentMsg]) -> Option<String> {
    messages
        .iter()
        .find(|m| m.phase.as_deref() == Some("final_answer"))
        .or_else(|| messages.iter().find(|m| !m.is_reasoning))
        .map(|m| m.text.clone())
}

impl TurnSummary {
    pub fn of(turn: &CodexTurn) -> Self {
        Self {
            turn_id: turn.turn_id.clone(),
            status: turn.status.clone(),
            started_at: turn.started_at,
            completed_at: turn.completed_at,
            duration_ms: turn.duration_ms,
            user_message: turn.user_message.clone(),
            agent_preview: agent_preview(&turn.agent_messages),
            last_agent_timestamp: turn.agent_messages.last().map(|m| m.timestamp.clone()),
            tool_call_count: turn.tool_calls.len(),
            reasoning_count: turn
                .agent_messages
                .iter()
                .filter(|m| m.is_reasoning)
                .count(),
            total_tokens: turn.total_tokens.as_ref().map(|t| t.total_tokens),
            model: turn.model.clone(),
            has_detail: !turn.agent_messages.is_empty() || !turn.tool_calls.is_empty(),
        }
    }
}

/// A session without its turn bodies: the metadata the info bar needs, plus one
/// [`TurnSummary`] per turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    /// The full session with `turns` emptied, so every existing metadata
    /// consumer keeps working unchanged.
    pub session: CodexSession,
    pub summaries: Vec<TurnSummary>,
}

impl SessionIndex {
    /// Index a session held in the parse cache without copying its turn bodies.
    ///
    /// The turns are moved out, summarised and put back, so the only copy made
    /// is of the session metadata. Cloning the session first and emptying the
    /// copy would duplicate every tool output in the transcript — on every
    /// index read, which is once per live append on a session being watched.
    pub fn of_cached(session: &mut CodexSession) -> Self {
        let turns = std::mem::take(&mut session.turns);
        let summaries = turns.iter().map(TurnSummary::of).collect();
        let metadata = session.clone();
        session.turns = turns;
        Self {
            session: metadata,
            summaries,
        }
    }

    /// Split an owned session into its metadata and its turn index, dropping
    /// every turn body.
    pub fn of(mut session: CodexSession) -> Self {
        Self::of_cached(&mut session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::toolcall::ToolCall;
    use crate::parser::turn::TokenInfo;

    fn agent(text: &str, phase: Option<&str>, reasoning: bool, ts: &str) -> AgentMsg {
        AgentMsg {
            text: text.to_string(),
            phase: phase.map(str::to_string),
            timestamp: ts.to_string(),
            is_reasoning: reasoning,
            order: 0,
        }
    }

    fn turn_with(messages: Vec<AgentMsg>, tools: Vec<ToolCall>) -> CodexTurn {
        let mut t = CodexTurn::new("t1".to_string());
        t.agent_messages = messages;
        t.tool_calls = tools;
        t
    }

    #[test]
    fn preview_prefers_the_final_answer() {
        let t = turn_with(
            vec![
                agent("thinking", None, true, "2026-01-01T00:00:00Z"),
                agent("chatter", None, false, "2026-01-01T00:00:01Z"),
                agent(
                    "the answer",
                    Some("final_answer"),
                    false,
                    "2026-01-01T00:00:02Z",
                ),
            ],
            vec![],
        );
        assert_eq!(
            TurnSummary::of(&t).agent_preview.as_deref(),
            Some("the answer")
        );
    }

    #[test]
    fn preview_falls_back_to_the_first_non_reasoning_message() {
        let t = turn_with(
            vec![
                agent("thinking", None, true, "2026-01-01T00:00:00Z"),
                agent("chatter", None, false, "2026-01-01T00:00:01Z"),
            ],
            vec![],
        );
        assert_eq!(
            TurnSummary::of(&t).agent_preview.as_deref(),
            Some("chatter")
        );
    }

    #[test]
    fn preview_is_absent_when_every_message_is_reasoning() {
        let t = turn_with(
            vec![agent("thinking", None, true, "2026-01-01T00:00:00Z")],
            vec![],
        );
        assert!(TurnSummary::of(&t).agent_preview.is_none());
    }

    #[test]
    fn counts_reasoning_messages_and_tool_calls() {
        let t = turn_with(
            vec![
                agent("a", None, true, "2026-01-01T00:00:00Z"),
                agent("b", None, true, "2026-01-01T00:00:01Z"),
                agent("c", None, false, "2026-01-01T00:00:02Z"),
            ],
            vec![ToolCall::default(), ToolCall::default()],
        );
        let s = TurnSummary::of(&t);
        assert_eq!(s.reasoning_count, 2);
        assert_eq!(s.tool_call_count, 2);
        assert!(s.has_detail);
    }

    #[test]
    fn an_empty_turn_has_no_detail() {
        let s = TurnSummary::of(&turn_with(vec![], vec![]));
        assert!(!s.has_detail);
        assert_eq!(s.tool_call_count, 0);
        assert_eq!(s.reasoning_count, 0);
        assert!(s.last_agent_timestamp.is_none());
    }

    #[test]
    fn a_turn_with_only_tool_calls_still_has_detail() {
        let s = TurnSummary::of(&turn_with(vec![], vec![ToolCall::default()]));
        assert!(s.has_detail);
    }

    #[test]
    fn last_agent_timestamp_is_the_last_message_not_the_first() {
        let t = turn_with(
            vec![
                agent("a", None, false, "2026-01-01T00:00:00Z"),
                agent("b", None, false, "2026-01-01T00:00:09Z"),
            ],
            vec![],
        );
        assert_eq!(
            TurnSummary::of(&t).last_agent_timestamp.as_deref(),
            Some("2026-01-01T00:00:09Z"),
        );
    }

    #[test]
    fn total_tokens_is_flattened_to_the_number_the_row_prints() {
        let mut t = turn_with(vec![], vec![]);
        t.total_tokens = Some(TokenInfo {
            total_tokens: 4321,
            ..Default::default()
        });
        assert_eq!(TurnSummary::of(&t).total_tokens, Some(4321));
    }

    #[test]
    fn the_index_drops_every_turn_body_but_keeps_the_metadata() {
        let session = CodexSession {
            id: "sess-1".into(),
            turns: vec![
                turn_with(
                    vec![agent("a", None, false, "t")],
                    vec![ToolCall::default()],
                ),
                turn_with(vec![], vec![]),
            ],
            ..Default::default()
        };

        let index = SessionIndex::of(session);

        assert_eq!(index.session.id, "sess-1");
        assert!(index.session.turns.is_empty(), "bodies are dropped");
        assert_eq!(index.summaries.len(), 2);
        assert_eq!(index.summaries[0].tool_call_count, 1);
    }

    #[test]
    fn the_index_of_a_session_with_no_turns_is_empty() {
        let index = SessionIndex::of(CodexSession::default());
        assert!(index.summaries.is_empty());
    }

    #[test]
    fn the_index_is_much_smaller_than_the_session_it_came_from() {
        let tool = ToolCall {
            output: Some("x".repeat(200_000)),
            ..Default::default()
        };
        let session = CodexSession {
            turns: vec![turn_with(vec![], vec![tool])],
            ..Default::default()
        };

        let full = serde_json::to_string(&session).unwrap().len();
        let index = serde_json::to_string(&SessionIndex::of(session))
            .unwrap()
            .len();

        assert!(index * 100 < full, "index {index} vs full {full}");
    }
}
