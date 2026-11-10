# ADR 0018：把大模型注册成 NARS 可调用的操作

- 状态：已接受
- 日期：2026-11-10
- 实现：`nars-server/src/ask_model.rs`、`AppState::broker_for`

## 决策

`ask_model` 是一个普通工具（manifest + schema），注册进 broker 后由 planner 自动变成 NARS 操作 `^ask_model`。它和 `read_file`、`search` 没有区别：模型可以提案，NARS 可以排序、引入、学习它的成败。

执行时工具在阻塞线程上同步调用 provider（`Handle::block_on`），要求模型返回 Agent IR 的 `final_answer`，答案作为工具输出进入观察，随后写入记忆。

冷启动保护：当模型和计划都没有给出候选动作时，actor 会补一个 `ask_model` 候选（问题取任务描述），让 NARS 有“问模型”这条路，而不是直接卡住。

## 边界

- 只在配置了真实 provider 的 server 中注册；评测用 scripted provider，不注册，避免改变基线；
- 它不满足任何阶段（提问不等于完成任务），不会推进阶段；
- 回答质量取决于模型，NARS 只负责记录和使用。

## 测试

`returns_the_model_answer_as_tool_output`：用 scripted provider 构造最终答案，工具在阻塞线程执行并返回文本。
