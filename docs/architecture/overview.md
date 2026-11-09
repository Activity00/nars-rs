# nars-agent 架构总览

## 1. 目标

`nars-agent` 是一个由 NARS 负责持续认知与任务调度、由小模型负责语言与代码生成、
由工具系统负责现实操作的开发型 Agent。

```text
用户消息 → 模型理解 → Agent IR 校验 → NARS 目标/事实
        → NARS 决策 → 审批 → Tool Broker → 观察
        → NARS 事实 → 下一步
```

## 2. Crate 结构

```text
crates/
├── nars-core             纯 NARS 推理内核（既有）
├── nars-agent-ir         Agent IR / Tool / Event / State 协议
├── nars-model            Provider 抽象 + OpenAI/Anthropic/Ollama/Echo
├── nars-tools            Manifest / Registry / Policy / Sandbox / Broker
├── nars-agent-runtime    Planner / SessionActor / Driver / Replay
├── nars-session          JSONL Event Log / Snapshot / 文件存储
└── nars-server           Axum API / SSE / 静态文件 / 可选鉴权
apps/narsd                daemon 入口
web                       React + TypeScript UI
```

依赖方向（无环）：

```text
nars-core
   ↑
nars-agent-ir ← nars-model
   ↑            ↑
nars-tools ─────┤
   ↑            │
nars-session ───┤
   ↑            │
nars-agent-runtime
   ↑
nars-server
   ↑
narsd
```

`nars-core` 不依赖任何新 crate；`nars-agent-ir` 不依赖 NARS。

## 3. Session Actor

每个 Run 由一个串行的 `SessionActor` 拥有：

```text
SessionActor
├── RunState                 （可序列化，带状态摘要）
├── NarsPlanner              （Reasoner + 工具历史 + 事实日志）
├── ToolBroker               （Arc，只读调用）
├── ToolManifest 目录
├── Phase 状态机
└── 待审批 / 待执行调用
```

Actor 的所有方法都是同步的，返回 `ActorOutput { events, commands }`。
异步工作由 `driver::run_loop` 执行：

```text
Command::CallModel  → provider.complete_with_retry → on_model_ok/on_model_err
Command::ExecuteTool→ broker.execute              → on_tool_result
Command::Checkpoint → sink.on_checkpoint
Command::Finish     → 结束
```

暂停/恢复/取消/审批通过 `tokio::sync::mpsc` 控制通道进入；
驱动在每个命令边界检查控制消息，因此暂停最多延迟一个工具/模型调用。

## 4. NARS 决策内核

每轮决策，Planner 为每个候选动作生成一条时序蕴含：

```text
<(ctx_<run>_<step> &/ <({SELF} * aN) --> ^tool>) =/> goal_<run>_<step>>. {1.0 c}
```

其中：

- `ctx` 在本轮作为“当前为真”的信念加入；
- `goal` 作为目标加入；
- `c` 由模型置信度、工具历史成功率、候选签名失败次数、副作用风险共同决定；
- 每个候选的 `c` 有微小递减偏移，保证并列时确定性。

NARS 自身的决策搜索会选择得分最高的前置条件并调用注册的操作回调。
回调**不执行工具**，只记录选择并返回 `failed: true`，因此 NARS 不会误以为动作已完成。
运行时随后：

1. 用选择结果构造 `ToolCall`；
2. 执行策略/审批/Broker；
3. 把结果转成观察与 NARS 事实；
4. 进入下一轮模型调用。

若 NARS 因阈值或编码问题没有选出动作，Planner 会显式标记
`DecisionSource::Fallback` 并选择得分最高的候选。该降级会出现在事件日志中，
不会被伪装成 NARS 决策。

### 任务计划（Phase 6）

运行时还持有一个线性阶段计划，例如：

```text
modify_code: investigate → change → verify → answer
```

阶段决定“哪些动作在原则上可以发生”，模型只提供具体候选与参数：

- `apply_patch` 只在 `change` 阶段可用；
- `cargo_check`/`cargo_test` 只在 `verify` 阶段可用；
- `investigate` 只有成功的 `read_file` 才算完成，list/search 只算进展；
- `verify` 失败会退回 `change`；
- 计划未完成时的 `final_answer` 会被拒绝并重新请求。

模型提出越界候选时，运行时会把它们标记为 `deferred`，并注入安全的只读/验证
fallback，仍由 NARS 选择。详见 [ADR 0004](../decisions/0004-task-plan.md)。

### 工具历史与经验

`ToolStat` 记录每个工具与每个“工具+参数签名”的尝试、成功、失败和连续失败。
下一轮评分会对失败签名扣分，对成功签名加分。该状态进入 Snapshot，恢复后继续生效。

跨 Run 的经验按 workspace 保存在
`.nars-agent/experience/<hash>.json`，新 Run 用
`NarsPlanner::new_with_experience` 载入，从而复用同一项目的工具可靠性历史。

### 事实日志

NARS 只保存短原子事实，最多保留 512 条用于恢复。完整内容在 Event Log。

## 5. 运行状态机

```text
Pending → Planning → (AwaitingApproval) → ExecutingTool → Planning → …
                  ↘ Paused ↔ Resume
                  ↘ Completed | Failed | Cancelled
```

状态更新只发生在 Actor 内。`RunState::state_hash()` 对除时间戳外的所有物化状态
计算 FNV-1a 摘要，用于重放校验。

## 6. 事件与重放

事件协议见 [docs/protocol/events.md](../protocol/events.md)。

- 每次状态变化都产生事件；
- 服务端 `StoreSink` 同步追加 JSONL 并广播；
- SSE 先回放历史，再推送实时事件，客户端按 `seq` 去重；
- `replay::project` 把事件流折叠成 `RunState`；
- `state_hash` 必须与实时状态一致（集成测试已覆盖）；
- Snapshot 只是缓存，Event Log 才是事实来源。

## 7. 持久化布局

默认文件存储：

```text
.nars-agent/sessions/
└── ses_<id>/
    ├── meta.json       # workspace / title / timestamps
    ├── events.jsonl    # append-only，seq 单调递增
    └── snapshot.json   # 最新 checkpoint（RunState + PlannerSnapshot + hash + last_seq）
```

SQLite 存储（`narsd --sqlite .nars-agent/sessions.db`）：

```sql
sessions(session_id, workspace, title, created_at_ms, updated_at_ms,
         parent_session_id, branched_from_run_id)
events(session_id, seq, timestamp_ms, event_json)
snapshots(session_id, run_id, state_hash, last_seq, created_at_ms, snapshot_json)
```

`SessionStore` 是 trait，文件与 SQLite 实现可互换，运行时无感知。SQLite 适配器
通过独立 worker 线程运行 `sqlx` 的 current-thread runtime，以保持同步 trait。

Run 历史不依赖进程内状态：`summarize_runs` 从事件日志投影 `RunSummary`，
`/api/sessions/{id}/runs` 合并实时与历史 Run；`/api/runs/{id}` 在进程重启后
从事件日志投影 `RunState` 并返回 `live=false`。

分支通过 `POST /api/sessions/{id}/branch` 创建：复制选中 Run 起点之前的事件，
重写 run_id / session_id，写入 `parent_session_id` / `branched_from_run_id`，然后重新执行
该 Run 的任务。`PATCH /api/sessions/{id}` 重命名，`DELETE /api/sessions/{id}` 删除。
详见 [ADR 0007](../decisions/0007-session-branching.md)、
[ADR 0008](../decisions/0008-session-lifecycle.md) 与
[ADR 0009](../decisions/0009-run-comparison.md)。

## 8. Web UI

三栏：

1. 左：会话分支树（按 `parent_session_id` 缩进）、workspace、工具目录、连接状态；
   分支显示 `↳`，选中会话可 Rename/Delete；
2. 中：Run 选择器 + 对话与执行时间线（NARS 决策、工具卡片、diff、审批、状态）；
   可选择 `All runs` 或单个 Run，时间线按 Run 过滤；终止 Run 可 **Branch & retry**；
   顶栏 **branch of ...** 跳转父会话；Inspector 可 **Compare with parent** 查看 Runs 指标对比；
3. 右：当前目标、NARS goal atom、事实、候选评分、文件变化、token、控制按钮。

UI 不直接访问文件系统，也不自行推断执行结果；所有内容来自 SSE 事件。

## 9. 失败处理

| 失败 | 行为 |
|---|---|
| Provider 超时/429/5xx | 指数退避重试，仍失败则 RunFailed |
| 模型输出非法 | 校验反馈修复一次，仍失败则 RunFailed(Validation) |
| 工具失败 | 转成失败观察，NARS 可换工具/重试/请求用户 |
| 工具超时 | `TimedOut` 结果，子进程被 kill |
| 审批拒绝 | `Denied` 观察，继续下一步 |
| NARS 未选出 | 显式 fallback 或失败 |
| 步数超限 | `RunFailed(StepLimit)` |
| 事件重放摘要不一致 | `AgentError::Replay` |

## 9.5 MCP 工具

`narsd --mcp-config <file>` 或 `NARS_AGENT_MCP_SERVERS` 可以连接 stdio 或 HTTP MCP server：

```text
MCP tools/list → ToolManifest(mcp_<server>_<tool>)
              → 注册进 ToolRegistry
              → NARS/Planner/Broker/UI 与内置工具一致
```

外部工具默认 `ExternalSideEffect` 且需要审批。退出时子进程随 `StdioTransport` 回收。
详见 [ADR 0005](../decisions/0005-mcp.md)。

## 9.6 评测

`nars-eval` 直接驱动真实运行时，记录工具顺序、NARS 决策、fallback、审批、文件变化
与重放一致性：

```bash
cargo run -p nars-eval -- --provider scripted
```

任务与 fixture 位于 `evals/`，详见 [ADR 0006](../decisions/0006-evaluation.md)。

## 10. 阶段状态

| 阶段 | 状态 |
|---|---|
| 0 架构决策 | ✅ 0001/0002/0003 |
| 1 最小 Runtime | ✅ Actor / Fake Model / Fake Tool / 重放测试 |
| 2 真实模型 | ✅ OpenAI-compatible / Anthropic / Ollama / 重试 / 校验 |
| 3 Tool Broker | ✅ 六个内置工具 / 沙箱 / 审批 / 超时 / 截断 |
| 4 API + Web UI | ✅ Axum + SSE + React 三栏 |
| 5 持久化与恢复 | ✅ JSONL / SQLite + Snapshot + 重放 + checkpoint resume + 重启后 Run 历史 + Session 分支 + 会话生命周期 |
| 6 NARS 原生任务调度 | ✅ 线性阶段计划、前置条件、失败回退、软时间预算、workspace 经验复用 |
| 7 MCP | ✅ stdio + Streamable HTTP、工具转换、外部权限；GET stream/续传待补 |
| 8 评测与发布 | ✅ `nars-eval` runner、6 个固定任务、指标与报告、GitHub Actions 工作流；真实模型基准待补 |
