# ADR 0016：前向链与 NARS 拥有的阶段完成判定

- 状态：已接受
- 日期：2026-11-10
- 实现：`Reasoner::forward_chain_implications`、`NarsPlanner::seed_stage_rules` / `stages_satisfied`、`SessionActor::handle_final_answer`

## 背景

[ADR 0015](0015-decision-learning.md) 之后，NARS 能选择、否决和补充工具候选，但还有两件事由 Rust 和模型掌握：

1. 模型返回 `final_answer` 时，是否接受由 Rust 的 `TaskPlan::can_finish` 判定；
2. 计划阶段是否完成，由 Rust 的计数器决定。

要让 NARS 成为核心，这两件必须由 NARS 的信念决定。而这依赖一个引擎能力：输入蕴含能参与前向推理。

实测发现这个移植版只做后向（目标）推理：`<a ==> b>` 加事实 `a` 推不出 `b`。原因是输入蕴含被存到结论概念的 `implication_links`，供目标回溯，不参与信念前向链。

## 决策

### 1. 输入蕴含参与前向链

`Reasoner` 记录一份 `forward_rules`（subject、consequent、truth）。`add_belief_term` 遇到蕴含句式时登记；`cycle()` 处理一个信念后，用该信念匹配 subject，按 NAL 演绎真值推导 consequent，并以 derived 事件写回记忆，等待下一轮继续链。

规则集很小（阶段规则十来条），每轮一跳、由 cycle 自然收敛。

### 2. 阶段完成由 NARS 推导

Planner 在构造时种下静态规则：

```text
<tool_read_file_observed_success ==> stage_investigate_satisfied>.
<tool_apply_patch_outcome_success  ==> stage_change_satisfied>.
<tool_cargo_test_outcome_success   ==> stage_verify_satisfied>.
...
```

工具观察/结果事实本来就会写入记忆，前向链因而推出 `stage_<name>_satisfied` 信念。`stages_satisfied(goal_type)` 按目标类型查询这些信念的期望值，全部 > 0.5 才算完成。

### 3. 是否收尾由 NARS 决定

`handle_final_answer` 不再调用 `plan.can_finish()`，改为询问 `planner.stages_satisfied(...)`：

- NARS 认为未完成 → 打回，要求继续动作（原有两次 deferral 作为安全阀保留）；
- NARS 认为完成 → 接受答案。

也就是说：**“做完了没有”这个判断从 Rust 计数器移到了 NARS 信念**。模型仍然提供候选、参数和最终文本，但它不再决定何时收尾。

## 评测

| 指标 | ADR 0015 后 | 本 ADR 后 |
|---|---|---|
| NARS 通过 | 8/8 | **8/8** |
| Model-top 基线 | 7/8 | **7/8** |
| override（对抗任务） | 50% | **50%** |
| NARS pickup | 3 | **3** |
| 对抗任务模型调用 | 5 | **3**（不再反复打回） |

## 仍然由模型/Rust 掌握的部分

- **动作类型中的 clarification**：仍然由模型在 IR 里选择，NARS 无法生成问题文本；
- **候选参数**：首次提案的参数来自模型，NARS 引入工具时用计划模板的参数；
- **阶段顺序与工具 allowlist**：仍由 Rust `TaskPlan` 维护，NARS 只判定“完成/未完成”；
- `forward_rules` 不持久化，随 run 重建；阶段规则是静态种子，不需要跨 run 记忆。

## 验证

- `implication_forward_chains_a_belief`：`<a ==> b>` + `a` 推出 `b`；
- `stage_rule_derives_satisfaction`：工具成功事实推出 `stage_change_satisfied`；
- 评测 8/8、基线 7/8、replay hash 一致。
