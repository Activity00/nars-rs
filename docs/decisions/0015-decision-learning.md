# ADR 0015：让学到的工具经验真正进入决策

- 状态：已接受
- 日期：2026-11-10
- 实现：`Reasoner::store_implication_sentence` / `learn_tool_outcome` / `rank_goal_operations`、`NarsPlanner` 决策覆盖、`evals/nars_tasks/nars_override_after_failure.json`

## 背景

[ADR 0013](0013-memory-maintenance.md) 让记忆能持久化，但评测暴露了一个更根本的问题：在 scripted 任务集上，NARS 决策与“直接取模型最高置信候选”完全等价，override 率 0%。

排查后确认了三层原因：

1. planner 把模型预打分写进种子蕴含的真值，NARS 读回来必然还是同一个排序；
2. 输入蕴含 `<A =/> B>` 经普通 `add_sentence` 路径时，操作语句形态的前置条件不会进入结果概念的 `precondition_beliefs`——而决策循环只读这张表；
3. 失败/成功只产生事实，从未反向修正任何供决策使用的蕴含。

结果是“学习”只发生在 Rust 的 `score_candidate` 里，NARS 推理没有参与。

## 决策

### 1. 显式写入学习到的蕴含

新增 `Reasoner::store_implication_sentence`：解析 `<precondition =/> goal>`，直接构造 `Implication`（带合法的 `source_concept_id`，因为 `implication_valid` 要求），写入结果概念 `precondition_beliefs[operation_slot]`。

`learn_tool_outcome(goal, tool, success)` 在其上封装：成功写入 `{1.0 0.9}`，失败写入 `{0.0 0.9}`。

### 2. 读出决策用的排名

`Reasoner::rank_goal_operations(goal)` 扫描同一张 `precondition_beliefs`（以及 `implication_links`），把蕴含里的 `^tool` 按期望值聚合排序。

跳过前置条件里带 `ctx_` 原子的条目：那些是每步的临时决策种子，编码的是先验，不是结果。

### 3. planner 的学习/决策分离

- **学习目标稳定**：`goal_<goal_type>`，让证据跨步、跨 run 累积（记忆快照会持久化这些蕴含）；
- **决策目标每步新建**：`goal_<goal_type>_<step>`，保证 NARS 每次都会重新触发目标循环；
- **决策上下文**：`ctx_<goal_type>_<step>`，不按 stage 细分——阶段切换会让学习键与决策键错位，导致“学到的经验查不到”（这是端到端 override 一直为 0 的直接原因）；
- **先验**：已有学习值时用它，否则用 `score_candidate`（模型置信度 + 工具/签名历史）映射成弱先验；
- **覆盖**：NARS 选定后，如果学到的证据明显冲突，planner 改为选择证据最优的候选。判定条件有三种：当前工具期望 < 0.2 且候选 ≥ 0.4；候选高出 0.1 以上；或者当前选中的是计划回退项、而模型候选的期望与之相当（`+0.05` 以内）。覆盖只发生在有明确反证或明显退化时，不改变无证据场景的行为。

### 4. 计划回退不再盖过模型候选

`fallback_candidates` 仍然补充（验证失败后切换工具依赖它），但覆盖判定会记录候选来源：模型候选数量之内的项优先。这样 NARS 选到一个“中性”回退工具时，只要模型候选的期望不更低，就会切回模型候选。

## 评测

新增 `nars_override_after_failure`：模型两次都把刚失败的 `read_file` 作为最高置信候选，`search` 只有 0.45。

| 指标 | 修复前 | 修复后 |
|---|---|---|
| NARS 通过 | 6/6 | **7/7** |
| Model-top 基线 | 6/6 | **6/7**（新任务失败） |
| 新任务 override | 0% | **50%** |

基线在新任务上仍然失败，说明 NARS 这次确实改变了结果，而不是又跟着模型走。

## 限制

- 学习键只到 goal type，暂不区分 stage；同一目标类型下不同阶段的工具偏好会互相影响；
- 覆盖规则是保守的阈值判断，只处理“明显更差”的候选；
- 学习依赖记忆快照持久化，跨 workspace 不共享（profile 只提升事实，不提升决策蕴含）；
- `rank_goal_operations` 每次线性扫描目标概念的蕴含表，概念很多时需要考虑索引。
