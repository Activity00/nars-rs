# ADR 0004：运行时任务计划与经验复用

- 状态：已接受
- 日期：2026-11-06
- 实现：`crates/nars-agent-runtime/src/plan.rs`、`crates/nars-session/src/experience.rs`

## 背景

只有“模型提候选、NARS 排序”还不足以阻止模型主导长期流程：模型可以在没有读取
文件时要求打补丁，也可以在验证之前宣布完成。Phase 6 需要让运行时自己持有任务
计划，并把阶段前置条件变成硬约束。

## 决策

运行时为每个任务维护一个 **线性阶段计划**（`TaskPlan`）：

```text
ModifyCode / Refactor / CreateFiles:
    investigate → change → verify → answer

RunTests:
    verify → answer

Investigate / AnswerQuestion:
    investigate → answer

Other:
    (empty plan, model-driven)
```

每个阶段声明：

- `tools`：可满足该阶段的首选工具；
- `attempts` / `failures`：尝试与失败计数；
- `failed_tools`：本阶段已失败的工具；
- `progressed_tools`：已经产生进展但不足以完成阶段的只读工具；
- `entered_step` / `deadline_steps`：软时间预算；
- `status`：`pending | active | satisfied | blocked`。

### 阶段门控

`TaskPlan::filter` 把模型候选分为：

- **allowed**：当前阶段可执行；
- **deferred**：当前阶段不允许，记录在决策 rationale 中。

规则：

- 只读工具（`read_file`、`list_files`、`search`、`git_diff`、`git_status`）在
  非 `answer` 阶段始终可用，避免死锁；
- `apply_patch` 只在 `change` 阶段可用；
- `cargo_check` / `cargo_test` 只在 `verify` 阶段可用；
- `answer` 阶段不允许任何工具。

模型若只给出被拒绝的候选，运行时会：

1. 注入阶段安全的 fallback（`list_files`、`read_file`、`search`、
   `cargo_check`、`cargo_test`）；
2. 仍为空时，向模型发送阶段提示消息（最多 2 次）；
3. 仍无法推进时，以内部错误结束，而不是让模型绕过阶段。

### 阶段推进

| 阶段 | 满足条件 |
|---|---|
| investigate | 成功的 `read_file`。仅 list/search 只算进展，不算完成。 |
| change | 成功的 `apply_patch`（带 diff） |
| verify | 成功的 `cargo_check` 或 `cargo_test` |
| answer | 无需事件；进入后允许 `final_answer` |

特殊回退：

- `verify` 失败（编译/测试错误）会把计划退回 `change`，让 Agent 先修复；
- `change` 失败保持 `change`，失败统计会降低同一签名的分数；
- `investigate` 中已成功的 list/search 会进入 `progressed_tools`，下一轮不再重复，
  直到 `read_file` 完成阶段。

### 提前完成

模型在计划未完成时返回 `final_answer`：

- 前两次会被拒绝，并收到阶段提示；
- 第三次接受，但事件日志保留完整拒绝记录。

这保证“能否结束”由运行时计划决定，而不是模型单方面宣布。

### 时间预算

每个阶段有软步数预算。超时不会强制终止，而是：

- 在 `TaskPlanView` 标记 `overdue`；
- 写入 NARS 事实 `plan_<stage>_overdue`；
- 供后续策略加权（当前实现记录并展示，尚未驱动硬截止）。

### 经验复用

`nars-session::ExperienceStore` 按 canonical workspace 持久化：

```text
.nars-agent/experience/<hash>.json
```

内容：

- `tools`：每个工具的 attempts/successes/failures；
- `signatures`：每个“工具 + 参数”签名的历史；
- `tasks`：每种 goal type 的完成/失败次数。

新 Run 启动时用 `NarsPlanner::new_with_experience` 载入这些统计；候选评分会：

- 用 Laplace-smoothed 成功率混合模型置信度；
- 对历史失败按次数扣分；
- 对历史成功的签名加分。

每个 checkpoint 把本 Run 的统计合并回 workspace 经验（取 `max`，避免重复计数）。
这样同一 workspace 的后续任务会从既有工具可靠性出发，而不是从零开始。

## 与 NARS 的关系

- 计划负责“哪些动作在原则上可以发生”；
- 模型负责“具体做什么、参数是什么”；
- NARS 负责“在这些候选中选哪一个”。

三者都不能单独完成整个循环。计划不调用工具，NARS 不生成参数，模型不决定阶段。

## 限制

- 阶段图是线性的，不支持任意 DAG、并行子任务或跨任务依赖；
- 经验是 workspace 级别的工具统计，不做语义检索，也没有时间衰减；
- 软时间预算只记录和展示，尚无强制超时；
- 阶段提示最多两次，复杂任务仍可能需要模型澄清。

这些限制留待后续迭代，不影响当前闭环。
