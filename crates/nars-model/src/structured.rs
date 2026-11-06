//! Robust extraction of Agent IR from model text.
//!
//! Providers differ: some return pure JSON, some wrap it in Markdown fences,
//! some add a short preamble. This module extracts the JSON object, repairs a
//! small set of common shape mistakes, and then delegates to serde.

use nars_agent_ir::AgentIr;
use serde_json::{Map, Value};

use crate::provider::ModelError;

/// Parses Agent IR from arbitrary model text.
///
/// # Errors
///
/// Returns [`ModelError::invalid_output`] when no usable JSON object exists.
pub fn parse_agent_ir(raw: &str) -> Result<AgentIr, ModelError> {
    let json = extract_json_object(raw)
        .ok_or_else(|| ModelError::invalid_output("no JSON object found in response"))?;
    let mut value: Value = serde_json::from_str(&json)
        .map_err(|error| ModelError::invalid_output(format!("malformed JSON: {error}")))?;

    unwrap_known_containers(&mut value);
    normalize_kind(&mut value);
    normalize_fields(&mut value);

    serde_json::from_value::<AgentIr>(value)
        .map_err(|error| ModelError::invalid_output(error.to_string()))
}

/// Extracts the outermost JSON object from text.
#[must_use]
pub fn extract_json_object(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let unfenced = strip_fences(trimmed).unwrap_or_else(|| trimmed.to_owned());
    let start = unfenced.find('{')?;
    let end = unfenced.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(unfenced[start..=end].to_owned())
}

fn strip_fences(text: &str) -> Option<String> {
    let fence_start = text.find("```")?;
    let after = &text[fence_start + 3..];
    let after = after.split_once('\n').map_or(after, |(header, rest)| {
        if header.trim().is_empty()
            || header.trim().eq_ignore_ascii_case("json")
            || header.trim().starts_with("json")
        {
            rest
        } else {
            after
        }
    });
    let fence_end = after.find("```").unwrap_or(after.len());
    Some(after[..fence_end].to_owned())
}

fn unwrap_known_containers(value: &mut Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    if object.contains_key("kind") {
        return;
    }
    for key in ["agent_ir", "response", "result", "output"] {
        if let Some(Value::Object(inner)) = object.get(key)
            && (inner.contains_key("kind") || inner.contains_key("candidate_actions"))
        {
            *value = Value::Object(inner.clone());
            return;
        }
    }
}

fn normalize_kind(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let inferred = object
        .get("kind")
        .and_then(Value::as_str)
        .map(normalize_kind_label)
        .or_else(|| infer_kind(object));
    if let Some(kind) = inferred {
        object.insert("kind".to_owned(), Value::String(kind));
    }
}

fn normalize_kind_label(label: &str) -> String {
    let normalized = label.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        "proposal" | "plan" | "task" | "understand" => "task_proposal".to_owned(),
        "final" | "answer" | "done" | "complete" | "completion" => "final_answer".to_owned(),
        "clarify" | "question" | "ask_user" => "clarification".to_owned(),
        "action_parameters" | "parameters" | "tool_arguments" => "action_parameters".to_owned(),
        other => other.to_owned(),
    }
}

fn infer_kind(object: &Map<String, Value>) -> Option<String> {
    if object.contains_key("candidate_actions") || object.contains_key("goal") {
        return Some("task_proposal".to_owned());
    }
    if object.contains_key("question") {
        return Some("clarification".to_owned());
    }
    if object.contains_key("text") || object.contains_key("answer") {
        return Some("final_answer".to_owned());
    }
    if object.contains_key("tool") && object.contains_key("arguments") {
        return Some("action_parameters".to_owned());
    }
    None
}

fn normalize_fields(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.get("kind").and_then(Value::as_str) == Some("final_answer")
        && !object.contains_key("text")
        && let Some(Value::String(answer)) = object.remove("answer")
    {
        object.insert("text".to_owned(), Value::String(answer));
    }
    if object.get("kind").and_then(Value::as_str) == Some("task_proposal")
        && let Some(Value::String(description)) = object.remove("description")
        && let Some(Value::Object(goal)) = object.get_mut("goal")
        && !goal.contains_key("description")
    {
        goal.insert("description".to_owned(), Value::String(description));
    }
    if let Some(Value::Object(goal)) = object.get_mut("goal")
        && !goal.contains_key("type")
        && let Some(Value::String(kind)) = goal.remove("kind")
    {
        goal.insert("type".to_owned(), Value::String(kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::GoalType;
    use nars_agent_ir::ir::MAX_CANDIDATE_ACTIONS;

    #[test]
    fn parses_plain_json() {
        let ir = parse_agent_ir(r#"{"kind":"final_answer","text":"ok"}"#).unwrap();
        assert!(matches!(ir, AgentIr::FinalAnswer(_)));
    }

    #[test]
    fn parses_fenced_json_with_preamble() {
        let raw = "Here is the plan:\n```json\n{\"goal\":{\"type\":\"modify_code\"},\"candidate_actions\":[{\"tool\":\"read_file\",\"arguments\":{\"path\":\"src/lib.rs\"}}]}\n```\nDone.";
        let ir = parse_agent_ir(raw).unwrap();
        let AgentIr::TaskProposal(proposal) = ir else {
            panic!("expected proposal");
        };
        assert_eq!(proposal.goal.goal_type, GoalType::ModifyCode);
        assert_eq!(proposal.candidate_actions.len(), 1);
    }

    #[test]
    fn unwraps_common_containers() {
        let raw = r#"{"agent_ir":{"kind":"clarification","question":"which crate?"}}"#;
        let ir = parse_agent_ir(raw).unwrap();
        assert!(matches!(ir, AgentIr::Clarification(_)));
    }

    #[test]
    fn normalizes_answer_to_final_answer() {
        let raw = r#"{"kind":"answer","answer":"done"}"#;
        let ir = parse_agent_ir(raw).unwrap();
        let AgentIr::FinalAnswer(answer) = ir else {
            panic!("expected final answer");
        };
        assert_eq!(answer.text, "done");
    }

    #[test]
    fn rejects_text_without_json() {
        let error = parse_agent_ir("no json here").unwrap_err();
        assert!(error.retryable);
    }

    #[test]
    fn candidate_constant_is_sane() {
        assert!(MAX_CANDIDATE_ACTIONS >= 8);
    }
}
