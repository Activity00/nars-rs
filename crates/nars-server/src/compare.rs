//! Run-level comparison derived from persisted event logs.

use std::collections::{BTreeMap, BTreeSet};

use nars_agent_ir::{AgentEvent, DecisionSource, RunId};
use nars_session::{EventEnvelope, RunSummary, SessionMeta, summarize_runs};
use serde::Serialize;

/// Metrics for one run.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RunMetrics {
    /// Terminal or current status.
    pub status: String,
    /// Highest decision step.
    pub steps: u32,
    /// Model requests.
    pub model_calls: u32,
    /// Tool executions.
    pub tool_calls: u32,
    /// Approval requests.
    pub approvals: u32,
    /// NARS decisions produced by fallback.
    pub fallbacks: u32,
    /// All NARS decisions.
    pub decisions: u32,
    /// Observations.
    pub observations: u32,
    /// Changed files aggregated from tool diffs.
    pub files: Vec<FileMetric>,
    /// Tool execution counts.
    pub tools: BTreeMap<String, u32>,
    /// Last assistant message.
    pub final_answer: Option<String>,
    /// First-to-last event duration.
    pub duration_ms: i64,
}

/// One file changed by a run.
#[derive(Clone, Debug, Serialize)]
pub struct FileMetric {
    /// Workspace-relative path.
    pub path: String,
    /// Added lines.
    pub added: u32,
    /// Removed lines.
    pub removed: u32,
}

/// One side of a comparison.
#[derive(Clone, Debug, Serialize)]
pub struct CompareSide {
    /// Session metadata.
    pub session: SessionMeta,
    /// Selected run.
    pub run_id: RunId,
    /// Metrics.
    pub metrics: RunMetrics,
}

/// Comparison response.
#[derive(Clone, Debug, Serialize)]
pub struct CompareResponse {
    /// Current/left session.
    pub left: CompareSide,
    /// Parent/right session.
    pub right: CompareSide,
    /// Runs available in the left session.
    pub left_runs: Vec<RunSummary>,
    /// Runs available in the right session.
    pub right_runs: Vec<RunSummary>,
    /// Files touched by both.
    pub shared_files: Vec<String>,
    /// Files touched only by the left run.
    pub only_left: Vec<String>,
    /// Files touched only by the right run.
    pub only_right: Vec<String>,
}

/// Computes comparison metrics for two sessions.
///
/// The default left run is the newest run in the left session. The default
/// right run is the left session's branch source when available, otherwise the
/// newest run in the right session. Callers may override either run.
#[must_use]
pub fn compare(
    left_meta: &SessionMeta,
    left_events: &[EventEnvelope],
    right_meta: &SessionMeta,
    right_events: &[EventEnvelope],
    left_run: Option<&RunId>,
    right_run: Option<&RunId>,
) -> Option<CompareResponse> {
    let left_runs = summarize_runs(left_events);
    let right_runs = summarize_runs(right_events);
    let left_default = left_runs.first()?.run_id.clone();
    let left_run = left_run
        .filter(|run_id| left_runs.iter().any(|summary| &summary.run_id == *run_id))
        .cloned()
        .unwrap_or(left_default);
    let source = left_meta.branched_from_run_id.as_ref().filter(|run_id| {
        right_events
            .iter()
            .any(|envelope| envelope.event.run_id() == Some(*run_id))
    });
    let right_default = source
        .cloned()
        .or_else(|| right_runs.first().map(|summary| summary.run_id.clone()))?;
    let right_run = right_run
        .filter(|run_id| right_runs.iter().any(|summary| &summary.run_id == *run_id))
        .cloned()
        .unwrap_or(right_default);

    let left_metrics = run_metrics(left_events, &left_run);
    let right_metrics = run_metrics(right_events, &right_run);
    let left_files = left_metrics
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let right_files = right_metrics
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    Some(CompareResponse {
        left: CompareSide {
            session: left_meta.clone(),
            run_id: left_run,
            metrics: left_metrics,
        },
        right: CompareSide {
            session: right_meta.clone(),
            run_id: right_run,
            metrics: right_metrics,
        },
        left_runs,
        right_runs,
        shared_files: left_files.intersection(&right_files).cloned().collect(),
        only_left: left_files.difference(&right_files).cloned().collect(),
        only_right: right_files.difference(&left_files).cloned().collect(),
    })
}

/// Folds one run's events into metrics.
#[must_use]
pub fn run_metrics(events: &[EventEnvelope], run_id: &RunId) -> RunMetrics {
    let mut metrics = RunMetrics::default();
    let mut first = None;
    let mut last = None;
    let mut files: BTreeMap<String, FileMetric> = BTreeMap::new();
    let mut tools: BTreeMap<String, u32> = BTreeMap::new();
    for envelope in events {
        if envelope.event.run_id() != Some(run_id) {
            continue;
        }
        first.get_or_insert(envelope.timestamp_ms);
        last = Some(envelope.timestamp_ms);
        match &envelope.event {
            AgentEvent::RunStarted { .. } => metrics.status = "planning".to_owned(),
            AgentEvent::ModelRequested { .. } => metrics.model_calls += 1,
            AgentEvent::NarsDecisionMade { decision, .. } => {
                metrics.decisions += 1;
                metrics.steps = metrics.steps.max(decision.step);
                if decision.source == DecisionSource::Fallback {
                    metrics.fallbacks += 1;
                }
            }
            AgentEvent::ApprovalRequested { .. } => metrics.approvals += 1,
            AgentEvent::ToolStarted { call, .. } => {
                metrics.tool_calls += 1;
                *tools.entry(call.tool.clone()).or_default() += 1;
            }
            AgentEvent::ToolFinished { result, .. } => {
                if let Some(diff) = &result.diff {
                    let entry = files
                        .entry(diff.path.clone())
                        .or_insert_with(|| FileMetric {
                            path: diff.path.clone(),
                            added: 0,
                            removed: 0,
                        });
                    entry.added += diff.added;
                    entry.removed += diff.removed;
                }
            }
            AgentEvent::ObservationRecorded { .. } => metrics.observations += 1,
            AgentEvent::AssistantMessage { text, .. } => {
                metrics.final_answer = Some(text.clone());
            }
            AgentEvent::RunPaused { .. } => metrics.status = "paused".to_owned(),
            AgentEvent::RunResumed { .. } => metrics.status = "planning".to_owned(),
            AgentEvent::RunCompleted { .. } => metrics.status = "completed".to_owned(),
            AgentEvent::RunFailed { .. } => metrics.status = "failed".to_owned(),
            AgentEvent::RunCancelled { .. } => metrics.status = "cancelled".to_owned(),
            _ => {}
        }
    }
    metrics.files = files.into_values().collect();
    metrics.tools = tools;
    if let (Some(first), Some(last)) = (first, last) {
        metrics.duration_ms = last.saturating_sub(first);
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{
        GoalSpec, GoalType, ModelPurpose, SessionId, TaskSpec, ToolCall, ToolCallId, ToolResult,
    };

    fn task() -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(GoalType::ModifyCode, "rename"),
            "/tmp/ws",
            "rename",
        )
    }

    fn envelope(seq: u64, event: AgentEvent) -> EventEnvelope {
        EventEnvelope {
            seq,
            timestamp_ms: i64::try_from(seq).unwrap() * 10,
            event,
        }
    }

    fn run_id() -> RunId {
        RunId::from_static("run_1")
    }

    fn events() -> Vec<EventEnvelope> {
        let session = SessionId::from_static("ses_1");
        vec![
            envelope(
                1,
                AgentEvent::RunStarted {
                    run_id: run_id(),
                    session_id: session.clone(),
                    task: task(),
                },
            ),
            envelope(
                2,
                AgentEvent::ModelRequested {
                    run_id: run_id(),
                    request_id: nars_agent_ir::RequestId::from_static("req_1"),
                    step: 1,
                    purpose: ModelPurpose::Plan,
                },
            ),
            envelope(
                3,
                AgentEvent::NarsDecisionMade {
                    run_id: run_id(),
                    step: 1,
                    decision: nars_agent_ir::NarsDecision {
                        step: 1,
                        selected: None,
                        scored: Vec::new(),
                        rationale: String::new(),
                        goal_term: String::new(),
                        source: DecisionSource::Fallback,
                        plan: None,
                    },
                },
            ),
            envelope(
                4,
                AgentEvent::ToolStarted {
                    run_id: run_id(),
                    call: ToolCall {
                        id: ToolCallId::from_static("call_1"),
                        run_id: run_id(),
                        tool: "apply_patch".to_owned(),
                        arguments: serde_json::json!({}),
                        side_effect: nars_agent_ir::SideEffect::WorkspaceWrite,
                        requires_approval: true,
                        created_at_ms: 0,
                    },
                },
            ),
            envelope(
                5,
                AgentEvent::ApprovalRequested {
                    run_id: run_id(),
                    call: ToolCall {
                        id: ToolCallId::from_static("call_1"),
                        run_id: run_id(),
                        tool: "apply_patch".to_owned(),
                        arguments: serde_json::json!({}),
                        side_effect: nars_agent_ir::SideEffect::WorkspaceWrite,
                        requires_approval: true,
                        created_at_ms: 0,
                    },
                    reason: "patch".to_owned(),
                    preview: None,
                },
            ),
            envelope(
                6,
                AgentEvent::ToolFinished {
                    run_id: run_id(),
                    result: ToolResult {
                        call_id: ToolCallId::from_static("call_1"),
                        status: nars_agent_ir::ToolStatus::Succeeded,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        truncated: false,
                        duration_ms: 1,
                        diff: Some(nars_agent_ir::Diff {
                            path: "src/lib.rs".to_owned(),
                            before_existed: true,
                            unified: String::new(),
                            added: 2,
                            removed: 1,
                        }),
                        artifacts: Vec::new(),
                        error: None,
                        metadata: serde_json::Map::new(),
                    },
                },
            ),
            envelope(
                7,
                AgentEvent::ObservationRecorded {
                    run_id: run_id(),
                    observation: nars_agent_ir::Observation {
                        id: nars_agent_ir::ObservationId::from_static("obs_1"),
                        run_id: run_id(),
                        call_id: ToolCallId::from_static("call_1"),
                        tool: "apply_patch".to_owned(),
                        summary: "patched".to_owned(),
                        outcome: nars_agent_ir::Outcome::Success,
                        facts: Vec::new(),
                        artifacts: Vec::new(),
                    },
                    plan: None,
                },
            ),
            envelope(
                8,
                AgentEvent::AssistantMessage {
                    run_id: run_id(),
                    text: "done".to_owned(),
                },
            ),
            envelope(
                9,
                AgentEvent::RunCompleted {
                    run_id: run_id(),
                    summary: "done".to_owned(),
                    success: true,
                },
            ),
        ]
    }

    #[test]
    fn folds_metrics_for_one_run() {
        let metrics = run_metrics(&events(), &run_id());
        assert_eq!(metrics.status, "completed");
        assert_eq!(metrics.steps, 1);
        assert_eq!(metrics.model_calls, 1);
        assert_eq!(metrics.tool_calls, 1);
        assert_eq!(metrics.approvals, 1);
        assert_eq!(metrics.fallbacks, 1);
        assert_eq!(metrics.decisions, 1);
        assert_eq!(metrics.observations, 1);
        assert_eq!(metrics.tools["apply_patch"], 1);
        assert_eq!(metrics.files[0].path, "src/lib.rs");
        assert_eq!(metrics.files[0].added, 2);
        assert_eq!(metrics.files[0].removed, 1);
        assert_eq!(metrics.final_answer.as_deref(), Some("done"));
        assert_eq!(metrics.duration_ms, 80);
    }
}
