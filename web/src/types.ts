export interface SessionMeta {
  session_id: string;
  workspace: string;
  title: string;
  created_at_ms: number;
  updated_at_ms: number;
  parent_session_id?: string | null;
  branched_from_run_id?: string | null;
}

export interface GoalSpec {
  type: string;
  target?: string;
  description: string;
}

export interface CompareFile {
  path: string;
  added: number;
  removed: number;
}

export interface CompareMetrics {
  status: string;
  steps: number;
  model_calls: number;
  tool_calls: number;
  approvals: number;
  fallbacks: number;
  decisions: number;
  observations: number;
  files: CompareFile[];
  tools: Record<string, number>;
  final_answer?: string | null;
  duration_ms: number;
}

export interface CompareSide {
  session: SessionMeta;
  run_id: string;
  metrics: CompareMetrics;
}

export interface CompareResponse {
  left: CompareSide;
  right: CompareSide;
  left_runs: RunSummary[];
  right_runs: RunSummary[];
  shared_files: string[];
  only_left: string[];
  only_right: string[];
}

export interface RunSummary {
  run_id: string;
  status: string;
  goal: string;
  step: number;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface TaskSpec {
  goal: GoalSpec;
  constraints: string[];
  workspace: string;
  user_message: string;
  memory_brief?: string[];
  max_steps: number;
}

export interface NarsFact {
  term: string;
  truth: { frequency: number; confidence: number };
  eternal: boolean;
}

export interface NarsGoal {
  id: string;
  description: string;
  term: string;
  priority: number;
}

export interface ToolManifest {
  name: string;
  description: string;
  input_schema: unknown;
  side_effect: string;
  timeout_ms: number;
  requires_approval: boolean;
  version: string;
  tags: string[];
}

export interface ToolCall {
  id: string;
  run_id: string;
  tool: string;
  arguments: Record<string, unknown>;
  side_effect: string;
  requires_approval: boolean;
  created_at_ms: number;
}

export interface Diff {
  path: string;
  before_existed: boolean;
  unified: string;
  added: number;
  removed: number;
}

export interface Artifact {
  id: string;
  kind: string;
  location: string;
  description: string;
  bytes?: number;
}

export interface ToolResult {
  call_id: string;
  status: string;
  exit_code?: number;
  stdout: string;
  stderr: string;
  truncated: boolean;
  duration_ms: number;
  diff?: Diff;
  artifacts: Artifact[];
  error?: string;
  metadata: Record<string, unknown>;
}

export interface ScoredCandidate {
  candidate_id: string;
  tool: string;
  score: number;
  reason: string;
}

export interface SelectedAction {
  candidate_id: string;
  tool: string;
  arguments: Record<string, unknown>;
  score: number;
}

export interface PlanStageView {
  id: string;
  label: string;
  status: string;
  tools: string[];
  overdue: boolean;
  attempts: number;
  failures: number;
}

export interface TaskPlanView {
  active?: string;
  stages: PlanStageView[];
  notes: string[];
  working_memory: string[];
  can_finish: boolean;
}

export interface NarsDecision {
  step: number;
  selected?: SelectedAction;
  scored: ScoredCandidate[];
  rationale: string;
  goal_term: string;
  source: string;
  plan?: TaskPlanView;
}

export interface Observation {
  id: string;
  run_id: string;
  call_id: string;
  tool: string;
  summary: string;
  outcome: string;
  facts: NarsFact[];
  artifacts: Artifact[];
}

export interface ToolPreview {
  summary: string;
  unified_diff?: string;
  files: string[];
}

export interface PendingApproval {
  call: ToolCall;
  reason: string;
  preview?: ToolPreview;
}

export interface TokenUsage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

export interface AgentEvent {
  type: string;
  [key: string]: any;
}

export interface WireEnvelope {
  seq: number;
  timestamp_ms: number;
  event: AgentEvent;
}

export interface RunView {
  id: string;
  sessionId: string;
  status: string;
  step: number;
  task?: TaskSpec;
  goal?: NarsGoal;
  plan?: TaskPlanView;
  facts: NarsFact[];
  decisions: NarsDecision[];
  observations: Observation[];
  toolCalls: Record<string, ToolCallView>;
  pendingApproval?: PendingApproval;
  error?: { message: string };
  usage: TokenUsage;
  modelPurpose?: string;
  createdAt: number;
}

export interface ToolCallView {
  call: ToolCall;
  result?: ToolResult;
  observation?: Observation;
}

export type TimelineItem =
  | { kind: "user"; id: string; text: string; runId?: string; ts: number }
  | { kind: "assistant"; id: string; text: string; runId: string; ts: number }
  | { kind: "decision"; id: string; runId: string; decision: NarsDecision; ts: number }
  | { kind: "tool"; id: string; runId: string; callId: string; ts: number }
  | { kind: "approval"; id: string; runId: string; callId: string; ts: number }
  | {
      kind: "status";
      id: string;
      runId?: string;
      text: string;
      tone: "info" | "success" | "error";
      ts: number;
    };

export interface SelectedSessionState {
  sessionId: string;
  timeline: TimelineItem[];
  runs: Record<string, RunView>;
  runOrder: string[];
  summaries: RunSummary[];
  activeRunId?: string;
  lastSeq: number;
  connection: "connecting" | "live" | "error";
}

export interface AppState {
  sessions: SessionMeta[];
  selected?: SelectedSessionState;
  tools: ToolManifest[];
  provider?: string;
  model?: string;
  globalError?: string;
  loadingSessions: boolean;
}

export interface MemoryConceptView {
  term: string;
  priority: number;
  use_count: number;
  frequency: number;
  confidence: number;
  implications: number;
}

export interface MemoryImplicationView {
  from: string;
  to: string;
  frequency: number;
  confidence: number;
}

export interface MemorySummary {
  workspace: string;
  available: boolean;
  concept_count: number;
  implication_count: number;
  atom_count: number;
  top_concepts: MemoryConceptView[];
  top_implications: MemoryImplicationView[];
}
