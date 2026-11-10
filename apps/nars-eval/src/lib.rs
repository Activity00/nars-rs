//! Deterministic evaluation runner for nars-agent.
//!
//! Tasks are JSON files that describe a prompt, an optional fixture, a scripted
//! model (for deterministic regression), and expectations. The runner executes
//! the real runtime, tool broker, NARS planner, approval loop, and replay
//! projection, then writes a JSON/Markdown report.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nars_agent_ir::{AgentEvent, AgentIr, GoalSpec, GoalType, RunState, SessionId, TaskSpec};
use nars_agent_runtime::{
    ActorConfig, AgentRuntime, RuntimeConfig, RuntimeSink, SelectionMode, project,
};
use nars_model::{DemoProvider, ModelProvider, ScriptedProvider, from_env};
use nars_session::EventEnvelope;
use nars_tools::ToolBroker;
use nars_tools::policy::{ApprovalMode, Policy, PolicyConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use walkdir::WalkDir;

/// Errors raised before or during an evaluation run.
#[derive(Debug, Error)]
pub enum EvalError {
    /// Filesystem failure.
    #[error("evaluation I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed task JSON.
    #[error("evaluation JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Runtime or provider setup failure.
    #[error("evaluation setup error: {0}")]
    Setup(String),
}

/// Which model provider to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderChoice {
    /// Per-task scripted responses.
    Scripted,
    /// Deterministic read-only demo provider.
    Demo,
    /// Provider configured through environment variables.
    Env,
}

impl std::str::FromStr for ProviderChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "scripted" => Ok(Self::Scripted),
            "demo" => Ok(Self::Demo),
            "env" => Ok(Self::Env),
            other => Err(format!("unknown provider `{other}` (scripted|demo|env)")),
        }
    }
}

/// One evaluation task.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskDefinition {
    /// Stable identifier.
    pub id: String,
    /// Category directory name.
    #[serde(default)]
    pub category: String,
    /// User prompt.
    pub prompt: String,
    /// Goal type such as `modify_code`.
    #[serde(default)]
    pub goal_type: Option<String>,
    /// Fixture setup.
    #[serde(default)]
    pub setup: Setup,
    /// Run limits.
    #[serde(default)]
    pub limits: Limits,
    /// Scripted Agent IR responses for deterministic runs.
    #[serde(default)]
    pub script: Vec<AgentIr>,
    /// Deny approval requests instead of approving them.
    #[serde(default)]
    pub deny_approvals: bool,
    /// Pause immediately after starting, then resume (recovery smoke test).
    #[serde(default)]
    pub pause_before_start: bool,
    /// How long to stay paused.
    #[serde(default)]
    pub pause_ms: Option<u64>,
    /// Expectations interpreted by the runner.
    #[serde(default)]
    pub expect: Value,
}

/// Fixture configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Setup {
    /// Directory name under the fixture root.
    #[serde(default)]
    pub fixture: Option<String>,
}

/// Run limits.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Limits {
    /// Maximum agent steps.
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Maximum model calls.
    #[serde(default)]
    pub max_model_calls: Option<u32>,
}

/// Metrics captured for one task.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TaskMetrics {
    /// Final run status.
    pub status: String,
    /// Steps taken.
    pub steps: u32,
    /// Model requests.
    pub model_calls: u32,
    /// Tool calls in execution order.
    pub tools: Vec<String>,
    /// NARS decisions.
    pub decisions: u32,
    /// Decisions produced by fallback rather than NARS inference.
    pub fallbacks: u32,
    /// Fraction of decisions where NARS picked a candidate other than the
    /// highest pre-scored one. Zero means NARS never changed the outcome.
    pub override_rate: f64,
    /// Status of the same task under the model-top baseline (scripted only).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub baseline_status: String,
    /// Steps taken by the baseline run.
    #[serde(default)]
    pub baseline_steps: u32,
    /// Tools called by the baseline run.
    #[serde(default)]
    pub baseline_tools: Vec<String>,
    /// Model calls made by the baseline run.
    #[serde(default)]
    pub baseline_model_calls: u32,
    /// Approval requests observed.
    pub approvals: u32,
    /// Observations recorded.
    pub observations: u32,
    /// Whether replay produced the same state hash.
    pub replay_hash_equal: bool,
    /// Wall-clock duration.
    pub duration_ms: u64,
    /// Workspace files changed, excluding build artifacts.
    pub changed_files: Vec<String>,
    /// Final plan stages (id/status), when available.
    pub plan_stages: Vec<PlanStageMetric>,
    /// Terminal error, when the run failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One plan stage captured in a metrics report.
#[derive(Clone, Debug, Serialize)]
pub struct PlanStageMetric {
    /// Stage identifier.
    pub id: String,
    /// Stage status.
    pub status: String,
}

/// Result of one task.
#[derive(Clone, Debug, Serialize)]
pub struct TaskResult {
    /// Task identifier.
    pub id: String,
    /// Task category.
    pub category: String,
    /// `passed`, `failed`, or `skipped`.
    pub status: String,
    /// Failure messages.
    #[serde(default)]
    pub failures: Vec<String>,
    /// Why a task was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Metrics.
    pub metrics: TaskMetrics,
}

/// Aggregate report.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// Provider used.
    pub provider: String,
    /// Start time in Unix milliseconds.
    pub started_at_ms: i64,
    /// Total duration.
    pub duration_ms: u64,
    /// Passed count.
    pub passed: usize,
    /// Failed count.
    pub failed: usize,
    /// Skipped count.
    pub skipped: usize,
    /// Per-task results.
    pub tasks: Vec<TaskResult>,
}

impl Report {
    /// Renders a Markdown summary.
    #[must_use]
    pub fn markdown(&self) -> String {
        let baseline_passed = self
            .tasks
            .iter()
            .filter(|task| task.metrics.baseline_status == "passed")
            .count();
        let baseline_total = self
            .tasks
            .iter()
            .filter(|task| !task.metrics.baseline_status.is_empty())
            .count();
        let mut output = format!(
            "# nars-agent evaluation report\n\nProvider: `{}`\n\n\
             Passed: **{}** · Failed: **{}** · Skipped: **{}** · Baseline (model-top): **{} / {}** · Duration: {:.2}s\n\n\
             | Task | Status | Tools | Decisions | NARS override | Baseline | Model calls | Approvals | Replay |\n\
             |---|---|---:|---:|---:|---|---:|---:|---|\n",
            self.provider,
            self.passed,
            self.failed,
            self.skipped,
            baseline_passed,
            baseline_total,
            self.duration_ms as f64 / 1_000.0
        );
        for task in &self.tasks {
            let baseline = if task.metrics.baseline_status.is_empty() {
                "—".to_owned()
            } else {
                format!(
                    "{} · {} tools",
                    task.metrics.baseline_status,
                    task.metrics.baseline_tools.len()
                )
            };
            output.push_str(&format!(
                "| `{}` | {} | {} | {} | {:.0}% | {} | {} | {} | {} |\n",
                task.id,
                task.status,
                task.metrics.tools.len(),
                task.metrics.decisions,
                task.metrics.override_rate * 100.0,
                baseline,
                task.metrics.model_calls,
                task.metrics.approvals,
                if task.metrics.replay_hash_equal {
                    "ok"
                } else {
                    "fail"
                }
            ));
        }
        for task in &self.tasks {
            if !task.failures.is_empty() {
                output.push_str(&format!("\n## `{}` failures\n\n", task.id));
                for failure in &task.failures {
                    output.push_str(&format!("- {failure}\n"));
                }
            }
        }
        output
    }

    /// Writes the JSON report.
    ///
    /// # Errors
    ///
    /// Returns an I/O or serialization error.
    pub fn write_json(&self, path: &Path) -> Result<(), EvalError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// Loads all task JSON files from a directory tree.
///
/// # Errors
///
/// Returns an error for unreadable files or invalid task JSON.
pub fn load_tasks(directory: &Path) -> Result<Vec<TaskDefinition>, EvalError> {
    let mut tasks: Vec<TaskDefinition> = Vec::new();
    for entry in WalkDir::new(directory).sort_by_file_name() {
        let entry = entry.map_err(|error| EvalError::Setup(error.to_string()))?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.starts_with("report") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        tasks.push(serde_json::from_str(&content)?);
    }
    tasks.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(tasks)
}

/// Runs every task sequentially.
///
/// # Errors
///
/// Returns an error when the task directory cannot be read.
pub async fn run_all(
    tasks_dir: &Path,
    fixtures_dir: &Path,
    provider: ProviderChoice,
    timeout: Duration,
) -> Result<Report, EvalError> {
    let started = Instant::now();
    let started_at_ms = now_ms();
    let tasks = load_tasks(tasks_dir)?;
    let mut results = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let mut result = match run_task(task, fixtures_dir, provider, timeout).await {
            Ok(result) => result,
            Err(error) => TaskResult {
                id: task.id.clone(),
                category: task.category.clone(),
                status: "failed".to_owned(),
                failures: vec![error.to_string()],
                skipped_reason: None,
                metrics: TaskMetrics::default(),
            },
        };
        if provider == ProviderChoice::Scripted && result.status != "skipped" {
            if let Ok(baseline) = run_task_with(
                task,
                fixtures_dir,
                provider,
                timeout,
                SelectionMode::Baseline,
            )
            .await
            {
                // Task-level verdict, comparable with the NARS row.
                result.metrics.baseline_status = baseline.status;
                result.metrics.baseline_steps = baseline.metrics.steps;
                result.metrics.baseline_tools = baseline.metrics.tools;
                result.metrics.baseline_model_calls = baseline.metrics.model_calls;
            }
        }
        results.push(result);
    }
    let passed = results
        .iter()
        .filter(|task| task.status == "passed")
        .count();
    let failed = results
        .iter()
        .filter(|task| task.status == "failed")
        .count();
    let skipped = results
        .iter()
        .filter(|task| task.status == "skipped")
        .count();
    Ok(Report {
        provider: match provider {
            ProviderChoice::Scripted => "scripted",
            ProviderChoice::Demo => "demo",
            ProviderChoice::Env => "env",
        }
        .to_owned(),
        started_at_ms,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        passed,
        failed,
        skipped,
        tasks: results,
    })
}

/// Runs one task.
///
/// # Errors
///
/// Returns an error for fixture, provider, or runtime setup failures.
pub async fn run_task(
    task: &TaskDefinition,
    fixtures_dir: &Path,
    provider_choice: ProviderChoice,
    timeout: Duration,
) -> Result<TaskResult, EvalError> {
    run_task_with(
        task,
        fixtures_dir,
        provider_choice,
        timeout,
        SelectionMode::Nars,
    )
    .await
}

async fn run_task_with(
    task: &TaskDefinition,
    fixtures_dir: &Path,
    provider_choice: ProviderChoice,
    timeout: Duration,
    selection: SelectionMode,
) -> Result<TaskResult, EvalError> {
    if provider_choice == ProviderChoice::Scripted && task.script.is_empty() {
        return Ok(TaskResult {
            id: task.id.clone(),
            category: task.category.clone(),
            status: "skipped".to_owned(),
            failures: Vec::new(),
            skipped_reason: Some("no scripted responses for this task".to_owned()),
            metrics: TaskMetrics::default(),
        });
    }
    let started = Instant::now();
    let workspace = tempfile::tempdir()?;
    if let Some(fixture) = &task.setup.fixture {
        let source = fixtures_dir.join(fixture);
        if !source.is_dir() {
            return Err(EvalError::Setup(format!(
                "fixture `{}` does not exist at {}",
                fixture,
                source.display()
            )));
        }
        copy_directory(&source, workspace.path())?;
    }
    let before = snapshot_files(workspace.path())?;

    let (provider, model_name) = build_provider(task, provider_choice)?;
    let policy = Policy::new(PolicyConfig {
        mode: ApprovalMode::Ask,
        ..PolicyConfig::default()
    });
    let broker = Arc::new(
        ToolBroker::with_builtin_tools(workspace.path(), policy)
            .map_err(|error| EvalError::Setup(error.to_string()))?,
    );
    let sink = Arc::new(CollectSink::default());
    let runtime = AgentRuntime::new(
        provider,
        broker,
        Arc::clone(&sink) as Arc<dyn RuntimeSink>,
        RuntimeConfig {
            actor: ActorConfig {
                model: model_name,
                seed: 7,
                selection,
                ..ActorConfig::default()
            },
            ..RuntimeConfig::default()
        },
    );
    let goal_type = parse_goal_type(task.goal_type.as_deref(), &task.category);
    let task_spec = TaskSpec {
        goal: GoalSpec::new(goal_type, task.prompt.clone()),
        constraints: Vec::new(),
        workspace: workspace.path().display().to_string(),
        user_message: task.prompt.clone(),
        history: Vec::new(),
        memory_brief: Vec::new(),
        max_steps: task.limits.max_steps.unwrap_or(16).clamp(1, 100),
        verification: Vec::new(),
    };
    let handle = runtime
        .start(SessionId::new(), task_spec)
        .map_err(|error| EvalError::Setup(error.to_string()))?;

    if task.pause_before_start {
        handle.pause("evaluation pause");
        tokio::time::sleep(Duration::from_millis(task.pause_ms.unwrap_or(100))).await;
        handle.resume();
    }

    let mut timed_out = false;
    let mut approved = BTreeSet::new();
    let wait = handle.wait();
    tokio::pin!(wait);
    loop {
        tokio::select! {
            () = &mut wait => break,
            () = tokio::time::sleep(Duration::from_millis(20)) => {
                let state = handle.state();
                if let Some(call) = state.pending_approval.clone()
                    && approved.insert(call.id.clone())
                {
                    handle.approve(call.id, !task.deny_approvals, None);
                }
                if started.elapsed() > timeout {
                    timed_out = true;
                    handle.cancel("evaluation timeout");
                    break;
                }
            }
        }
    }
    if timed_out {
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.wait()).await;
    }

    let state = handle.state();
    let events = sink.events.lock().unwrap().clone();
    let after = snapshot_files(workspace.path())?;
    let changed_files = changed_files(&before, &after);
    let model_calls = events
        .iter()
        .filter(|envelope| matches!(envelope.event, AgentEvent::ModelRequested { .. }))
        .count();
    let approvals = events
        .iter()
        .filter(|envelope| matches!(envelope.event, AgentEvent::ApprovalRequested { .. }))
        .count();
    let decisions = state.decisions.len();
    let fallbacks = state
        .decisions
        .iter()
        .filter(|decision| decision.source == nars_agent_ir::DecisionSource::Fallback)
        .count();
    let overrides = state
        .decisions
        .iter()
        .filter(|decision| {
            let Some(selected) = decision.selected.as_ref() else {
                return false;
            };
            decision
                .scored
                .first()
                .is_some_and(|top| top.candidate_id != selected.candidate_id)
        })
        .count();
    let override_rate = if decisions == 0 {
        0.0
    } else {
        f64::from(u32::try_from(overrides).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(decisions).unwrap_or(u32::MAX))
    };
    let tools = state
        .tool_calls
        .iter()
        .map(|call| call.tool.clone())
        .collect::<Vec<_>>();
    let plan = final_plan(&events, &state);
    let replay_hash_equal = project(&events)
        .map(|projected| projected.state_hash() == state.state_hash())
        .unwrap_or(false);
    let plan_stages = plan
        .as_ref()
        .map(|plan| {
            plan.stages
                .iter()
                .map(|stage| PlanStageMetric {
                    id: stage.id.clone(),
                    status: stage.status.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let metrics = TaskMetrics {
        status: if timed_out {
            "timed_out".to_owned()
        } else {
            state.status.label().to_owned()
        },
        steps: state.step,
        model_calls: u32::try_from(model_calls).unwrap_or(u32::MAX),
        tools,
        decisions: u32::try_from(decisions).unwrap_or(u32::MAX),
        fallbacks: u32::try_from(fallbacks).unwrap_or(u32::MAX),
        override_rate,
        baseline_status: String::new(),
        baseline_steps: 0,
        baseline_tools: Vec::new(),
        baseline_model_calls: 0,
        approvals: u32::try_from(approvals).unwrap_or(u32::MAX),
        observations: u32::try_from(state.observations.len()).unwrap_or(u32::MAX),
        replay_hash_equal,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        changed_files,
        plan_stages,
        error: state.error.as_ref().map(ToString::to_string),
    };
    let mut failures = evaluate(task, &metrics, &state, plan.as_ref());
    if let Some(limit) = task.limits.max_model_calls
        && metrics.model_calls > limit
    {
        failures.push(format!(
            "model calls {} exceed limit {limit}",
            metrics.model_calls
        ));
    }
    if timed_out {
        failures.push("task exceeded its timeout".to_owned());
    }
    Ok(TaskResult {
        id: task.id.clone(),
        category: task.category.clone(),
        status: if failures.is_empty() {
            "passed".to_owned()
        } else {
            "failed".to_owned()
        },
        failures,
        skipped_reason: None,
        metrics,
    })
}

fn build_provider(
    task: &TaskDefinition,
    choice: ProviderChoice,
) -> Result<(Arc<dyn ModelProvider>, String), EvalError> {
    match choice {
        ProviderChoice::Scripted => Ok((
            Arc::new(ScriptedProvider::new(task.script.clone())) as Arc<dyn ModelProvider>,
            "scripted".to_owned(),
        )),
        ProviderChoice::Demo => Ok((
            Arc::new(DemoProvider::default()) as Arc<dyn ModelProvider>,
            "demo".to_owned(),
        )),
        ProviderChoice::Env => {
            let provider = from_env().map_err(|error| EvalError::Setup(error.to_string()))?;
            let model = std::env::var("NARS_AGENT_MODEL").unwrap_or_else(|_| "env".to_owned());
            Ok((provider, model))
        }
    }
}

fn parse_goal_type(value: Option<&str>, category: &str) -> GoalType {
    match value {
        Some("modify_code") => GoalType::ModifyCode,
        Some("answer_question") => GoalType::AnswerQuestion,
        Some("investigate") => GoalType::Investigate,
        Some("run_tests") => GoalType::RunTests,
        Some("refactor") => GoalType::Refactor,
        Some("create_files") => GoalType::CreateFiles,
        Some("other") => GoalType::Other,
        Some(_) => GoalType::Other,
        None if category == "recovery" => GoalType::Other,
        None => GoalType::ModifyCode,
    }
}

fn evaluate(
    task: &TaskDefinition,
    metrics: &TaskMetrics,
    state: &RunState,
    plan: Option<&nars_agent_ir::TaskPlanView>,
) -> Vec<String> {
    let mut failures = Vec::new();
    let Some(expect) = task.expect.as_object() else {
        return failures;
    };
    let final_text = state
        .messages
        .iter()
        .filter(|message| message.role == nars_agent_ir::ChatRole::Assistant)
        .map(|message| message.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(expected) = expect
        .get("final_answer_contains")
        .and_then(Value::as_array)
    {
        for needle in expected.iter().filter_map(Value::as_str) {
            if !final_text.contains(needle) {
                failures.push(format!("final answer does not contain `{needle}`"));
            }
        }
    }
    if let Some(expected) = expect.get("files_modified").and_then(Value::as_array) {
        let expected = expected
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let actual = metrics
            .changed_files
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if expected != actual {
            failures.push(format!(
                "changed files mismatch: expected {expected:?}, actual {actual:?}"
            ));
        }
    }
    if let Some(unchanged) = expect.get("files_unchanged").and_then(Value::as_array) {
        for path in unchanged.iter().filter_map(Value::as_str) {
            if metrics.changed_files.iter().any(|changed| changed == path) {
                failures.push(format!("file `{path}` should not have changed"));
            }
        }
    }
    if let Some(expected) = expect.get("tools_used").and_then(Value::as_array) {
        for tool in expected.iter().filter_map(Value::as_str) {
            if !metrics.tools.iter().any(|actual| actual == tool) {
                failures.push(format!("tool `{tool}` was not used"));
            }
        }
    }
    if let Some(expected) = expect.get("ordered_tools").and_then(Value::as_array) {
        let expected = expected
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !is_subsequence(&expected, &metrics.tools) {
            failures.push(format!(
                "tool order mismatch: expected {expected:?}, actual {:?}",
                metrics.tools
            ));
        }
    }
    if let Some(expected) = expect.get("approval_required").and_then(Value::as_bool)
        && expected != (metrics.approvals > 0)
    {
        failures.push(format!(
            "approval_required expected {expected}, observed {} approval(s)",
            metrics.approvals
        ));
    }
    if let Some(minimum) = expect.get("min_override_rate").and_then(Value::as_f64)
        && metrics.override_rate < minimum
    {
        failures.push(format!(
            "override rate {:.2} is below the required {minimum:.2}",
            metrics.override_rate
        ));
    }
    if let Some(minimum) = expect.get("min_nars_decisions").and_then(Value::as_u64)
        && u64::from(metrics.decisions) < minimum
    {
        failures.push(format!(
            "expected at least {minimum} NARS decisions, got {}",
            metrics.decisions
        ));
    }
    if let Some(allowed) = expect.get("fallback_allowed").and_then(Value::as_bool)
        && !allowed
        && metrics.fallbacks > 0
    {
        failures.push(format!(
            "fallback was not allowed but {} decision(s) used it",
            metrics.fallbacks
        ));
    }
    if let Some(expected) = expect.get("run_status").and_then(Value::as_str)
        && expected != metrics.status
    {
        failures.push(format!(
            "run status expected `{expected}`, got `{}`",
            metrics.status
        ));
    }
    if let Some(expected) = expect.get("replay_hash_equal").and_then(Value::as_bool)
        && expected != metrics.replay_hash_equal
    {
        failures.push(format!(
            "replay hash equality expected {expected}, got {}",
            metrics.replay_hash_equal
        ));
    }
    if let Some(expected) = expect.get("plan_stages").and_then(Value::as_array) {
        let actual = plan
            .map(|plan| {
                plan.stages
                    .iter()
                    .map(|stage| stage.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for stage in expected.iter().filter_map(Value::as_str) {
            if !actual.iter().any(|actual| actual == stage) {
                failures.push(format!("plan stage `{stage}` was not present"));
            }
        }
    }
    failures
}

fn is_subsequence(expected: &[String], actual: &[String]) -> bool {
    let mut index = 0;
    for item in actual {
        if index < expected.len() && &expected[index] == item {
            index += 1;
        }
    }
    index == expected.len()
}

fn final_plan(events: &[EventEnvelope], state: &RunState) -> Option<nars_agent_ir::TaskPlanView> {
    for envelope in events.iter().rev() {
        if let AgentEvent::ObservationRecorded {
            plan: Some(plan), ..
        } = &envelope.event
        {
            return Some(plan.clone());
        }
    }
    state
        .decisions
        .iter()
        .rev()
        .find_map(|decision| decision.plan.clone())
}

fn snapshot_files(root: &Path) -> Result<BTreeMap<String, u64>, EvalError> {
    let mut files = BTreeMap::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let entry = entry.map_err(|error| EvalError::Setup(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if is_ignored(&relative) {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        files.insert(relative, hasher.finish());
    }
    Ok(files)
}

fn changed_files(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) -> Vec<String> {
    let mut changed = BTreeSet::new();
    for (path, hash) in after {
        if before.get(path) != Some(hash) {
            changed.insert(path.clone());
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            changed.insert(path.clone());
        }
    }
    changed.into_iter().collect()
}

fn is_ignored(path: &str) -> bool {
    path == "Cargo.lock"
        || path == ".DS_Store"
        || path.starts_with("target/")
        || path.starts_with(".git/")
        || path.starts_with(".nars-agent/")
        || path.starts_with("node_modules/")
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), EvalError> {
    for entry in WalkDir::new(source).sort_by_file_name() {
        let entry = entry.map_err(|error| EvalError::Setup(error.to_string()))?;
        let relative = entry.path().strip_prefix(source).unwrap_or(entry.path());
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[derive(Default)]
struct CollectSink {
    events: Mutex<Vec<EventEnvelope>>,
    seq: AtomicU64,
}

impl RuntimeSink for CollectSink {
    fn on_event(&self, event: &AgentEvent) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.events.lock().unwrap().push(EventEnvelope {
            seq,
            timestamp_ms: i64::try_from(seq).unwrap_or(0) * 10,
            event: event.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matching_is_order_sensitive() {
        let expected = vec!["a".to_owned(), "b".to_owned()];
        assert!(is_subsequence(
            &expected,
            &["x".to_owned(), "a".to_owned(), "b".to_owned()]
        ));
        assert!(!is_subsequence(
            &expected,
            &["b".to_owned(), "a".to_owned()]
        ));
    }

    #[test]
    fn ignores_build_artifacts() {
        assert!(is_ignored("target/debug/foo"));
        assert!(is_ignored("Cargo.lock"));
        assert!(!is_ignored("src/lib.rs"));
    }

    #[test]
    fn goal_type_defaults_are_stable() {
        assert_eq!(parse_goal_type(None, "coding_tasks"), GoalType::ModifyCode);
        assert_eq!(parse_goal_type(None, "recovery"), GoalType::Other);
        assert_eq!(
            parse_goal_type(Some("investigate"), "x"),
            GoalType::Investigate
        );
    }
}
