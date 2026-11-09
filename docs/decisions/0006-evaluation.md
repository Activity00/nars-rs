# ADR 0006：评测 runner 与固定评测集

- 状态：已接受
- 日期：2026-11-09
- 实现：`apps/nars-eval`、`evals/`

## 目标

阶段 8 需要回答两个问题：

1. Agent 是否真的完成任务，而不是只生成看起来合理的文本；
2. NARS 是否真的参与调度，而不是被模型或随机 fallback 覆盖。

因此评测不能只看最终回答，还要记录工具顺序、NARS 决策、fallback 次数、审批、
文件变化、重放一致性和模型调用次数。

## 决策

`nars-eval` 直接复用生产运行时：

```text
AgentRuntime → SessionActor → NarsPlanner → ToolBroker → Event Log → replay
```

不另写一套模拟逻辑，避免“评测通过但产品路径不同”。

### 任务格式

```json
{
  "id": "rename_symbol",
  "category": "coding_tasks",
  "goal_type": "modify_code",
  "prompt": "...",
  "setup": {"fixture": "rename_symbol"},
  "limits": {"max_steps": 10, "max_model_calls": 8},
  "script": [{"kind": "task_proposal", "...": "..."}],
  "deny_approvals": false,
  "pause_before_start": false,
  "pause_ms": 100,
  "expect": {
    "files_modified": ["src/lib.rs"],
    "tools_used": ["read_file", "apply_patch", "cargo_check"],
    "ordered_tools": ["list_files", "read_file"],
    "approval_required": true,
    "min_nars_decisions": 3,
    "fallback_allowed": false,
    "run_status": "completed",
    "replay_hash_equal": true,
    "plan_stages": ["investigate", "change", "verify", "answer"]
  }
}
```

`script` 是确定性的模型响应序列，使用 Scripted Provider 驱动。真实模型评测可以通过
`--provider env` 复用同一套 fixture 与期望，但结果不再完全确定。

### 执行流程

1. 复制 fixture 到临时 workspace；
2. 记录文件哈希快照；
3. 启动真实 runtime（Ask 审批策略）；
4. runner 轮询 pending approval：
   - 默认自动批准；
   - `deny_approvals=true` 时自动拒绝；
   - 记录审批次数；
5. 等待完成或超时；
6. 用 Event Log 投影状态并计算 `state_hash`；
7. 对比文件变化；
8. 执行 expectations；
9. 输出 JSON/Markdown 报告。

暂停恢复任务通过 `pause_before_start` 在首个命令前暂停，再恢复，验证控制通道与
checkpoint 之后的继续执行。

### 指标

每个任务记录：

- final status / steps；
- model calls；
- tool calls 及顺序；
- NARS decisions；
- fallback decisions；
- approvals；
- observations；
- replay hash equality；
- 变更文件（忽略 `target/`、`Cargo.lock` 等构建产物）；
- 最后 plan stages；
- duration；
- terminal error。

### 当前评测集

| 任务 | 验证点 |
|---|---|
| `rename_symbol` | read → patch → cargo_check → final |
| `fix_compile_error` | 读取类型错误、修复、验证 |
| `plan_order` | 模型先 patch、早 final 被 plan 拒绝，NARS 按 investigate → change → verify |
| `tool_failure_recovery` | read 失败后 search/list/read 纠正 |
| `interrupted_run` | 暂停/恢复后完成，重放一致 |
| `approval_denied` | 拒绝 patch 后文件不变、继续会话 |

### 运行

```bash
cargo run -p nars-eval -- --provider scripted \
  --out target/eval-report.json \
  --markdown-out target/eval-report.md
```

真实模型：

```bash
NARS_AGENT_PROVIDER=openai NARS_AGENT_API_KEY=... \
cargo run -p nars-eval -- --provider env
```

有失败时退出码为 1，可直接用于 CI。

## 限制

- Scripted 模式验证控制流，不衡量模型代码能力；
- 真实模型模式尚无多次采样与置信区间；
- 每个任务顺序执行，未并行；
- 没有跨任务的经验累计指标；
- `interrupted_run` 是暂停/恢复，不是进程崩溃恢复；
- 评测集规模仍小，需要更多多文件重构和长任务。

## 后续

- 真实模型基准与多次运行统计；
- 失败分类（模型、工具、计划、权限）；
- 进程崩溃恢复与 SQLite 后端；
- CI 门禁与历史趋势报告。
