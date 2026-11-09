# 事件协议

事件是 nars-agent 的唯一事实来源。UI、审计、重放和 SSE 都消费同一份
`AgentEvent` 流。

## 1. 信封

持久化与传输统一使用 `EventEnvelope`：

```json
{
  "seq": 42,
  "timestamp_ms": 1762345678901,
  "event": { "type": "tool_finished", "...": "..." }
}
```

- `seq`：会话内单调递增，从 1 开始；
- `timestamp_ms`：Unix 毫秒；
- `event`：内部带 `type` 标签的联合类型。

SSE 帧：

```text
event: agent
id: 42
data: {"seq":42,"timestamp_ms":...,"event":{...}}
```

## 2. 事件表

| type | 关键字段 | 说明 |
|---|---|---|
| `session_created` | session_id, workspace | 会话创建 |
| `user_message` | session_id, run_id, text | 用户输入；新事件绑定 run_id，便于按 Run 回看 |
| `run_started` | run_id, session_id, task | Run 开始 |
| `model_requested` | request_id, step, purpose | 调用模型 |
| `model_proposed` | ir, usage | 模型返回合法 IR |
| `model_failed` | request_id, error | 模型失败/校验失败 |
| `nars_goal_committed` | goal, facts | NARS 接受目标与事实 |
| `nars_decision_made` | decision, plan | NARS 完成一轮决策，附带任务计划视图 |
| `approval_requested` | call, reason, preview | 需要用户审批 |
| `approval_resolved` | call_id, approved | 审批结果 |
| `tool_started` | call | 工具开始（含被拒绝的尝试） |
| `tool_finished` | result | 工具结果 |
| `observation_recorded` | observation, plan | 语义观察、事实，以及应用后的任务计划 |
| `assistant_message` | text | Agent 回复 |
| `run_paused` | reason | 暂停 |
| `run_resumed` | — | 恢复 |
| `run_completed` | summary, success | 完成 |
| `run_failed` | error | 失败 |
| `run_cancelled` | reason | 取消 |

`model_failed` 在两种情况下出现：

1. Provider 重试耗尽；
2. 模型输出未通过校验，运行时正在发起一次修复请求。
   若修复后仍失败，会再出现 `run_failed`。

## 3. 顺序保证

- Actor 串行产生事件；
- `StoreSink` 先 append 到 JSONL，再广播；
- SSE 先发送历史，再发送订阅之后的实时事件；
- 同一 `seq` 最多出现一次（客户端按 `seq` 去重）；
- EventSource 自动重连时服务端会重放历史，去重保证 UI 不重复。

## 4. 重放

`nars_agent_runtime::replay::project(events)` 折叠事件得到 `RunState`：

- `run_started` 创建状态；
- `nars_decision_made` 推进 `step` 与决策列表；
- `tool_started` 记录调用；
- `tool_finished` 记录结果；
- `observation_recorded` 记录观察；
- `assistant_message` 记录消息（时间戳不参与摘要）；
- 终态事件设置状态与错误。

`RunState::state_hash()` 排除所有时间戳字段，只对物化内容计算摘要。
集成测试要求：

```text
project(recorded_events).state_hash() == live_state.state_hash()
```

## 5. 快照

`SessionSnapshot`：

```json
{
  "version": 1,
  "session_id": "ses_...",
  "run_id": "run_...",
  "state": { "run_id": "...", "status": "completed", "...": "..." },
  "runtime": { "seed": 7, "facts": [], "tool_stats": {} },
  "state_hash": "…",
  "last_seq": 42,
  "created_at_ms": 1762345678901
}
```

不变量：

- `snapshot.state_hash == snapshot.state.state_hash()`；
- `snapshot.last_seq` 不大于该会话日志最后一条的 `seq`；
- 恢复时可用 `NarsPlanner::restore(snapshot.runtime)` 重建决策历史。

## 6. 新增事件

新增事件需要同时修改：

1. `nars-agent-ir/src/event.rs`：枚举、`kind()`、`run_id()`；
2. `nars-agent-runtime`：产生事件的状态机；
3. `replay::project`：折叠规则；
4. `web/src/store.ts`：UI 投影；
5. `docs/protocol/events.md`；
6. 测试：序列化、重放摘要。
