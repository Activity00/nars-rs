//! JSON Schema for the Agent IR, shown to models and used by strict providers.

use serde_json::{Value, json};

/// Returns the JSON Schema of [`nars_agent_ir::AgentIr`].
///
/// The schema is intentionally permissive about optional fields but narrow
/// about the four allowed kinds and the shape of candidate actions.
#[must_use]
pub fn agent_ir_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "NarsAgentIr",
        "oneOf": [
            task_proposal_schema(),
            action_parameters_schema(),
            final_answer_schema(),
            clarification_schema()
        ]
    })
}

fn task_proposal_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "goal"],
        "additionalProperties": true,
        "properties": {
            "kind": {"const": "task_proposal"},
            "goal": {
                "type": "object",
                "required": ["type"],
                "additionalProperties": true,
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["modify_code", "answer_question", "investigate", "run_tests", "refactor", "create_files", "other"]
                    },
                    "target": {"type": "string", "description": "Workspace-relative path when applicable."},
                    "description": {"type": "string"}
                }
            },
            "constraints": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
            "candidate_actions": {
                "type": "array",
                "maxItems": 16,
                "items": {
                    "type": "object",
                    "required": ["tool", "arguments"],
                    "additionalProperties": true,
                    "properties": {
                        "id": {"type": "string"},
                        "tool": {"type": "string"},
                        "arguments": {"type": "object"},
                        "rationale": {"type": "string"},
                        "confidence": {"type": "number", "minimum": 0, "maximum": 1}
                    }
                }
            },
            "confidence": {"type": "number", "minimum": 0, "maximum": 1},
            "notes": {"type": "string"},
            "summary": {"type": "string"}
        }
    })
}

fn action_parameters_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "tool", "arguments"],
        "additionalProperties": true,
        "properties": {
            "kind": {"const": "action_parameters"},
            "tool": {"type": "string"},
            "arguments": {"type": "object"},
            "rationale": {"type": "string"}
        }
    })
}

fn final_answer_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "text"],
        "additionalProperties": true,
        "properties": {
            "kind": {"const": "final_answer"},
            "text": {"type": "string"},
            "success": {"type": "boolean"},
            "artifacts": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn clarification_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "question"],
        "additionalProperties": true,
        "properties": {
            "kind": {"const": "clarification"},
            "question": {"type": "string"},
            "options": {"type": "array", "items": {"type": "string"}}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_describes_all_kinds() {
        let schema = agent_ir_schema();
        let kinds = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["properties"]["kind"]["const"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "task_proposal",
                "action_parameters",
                "final_answer",
                "clarification"
            ]
        );
    }
}
