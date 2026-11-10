# ADR 0019：问答的下沉知识

- 状态：已接受
- 日期：2026-11-10
- 实现：`nars-server/src/knowledge.rs`、`start_task`、`StoreSink::capture_knowledge`

## 背景

问题类目标此前直接 finalize，模型答完即丢；下一次同样的问题还要再问一遍模型，NARS 没有积累。

## 决策

`ask_model` 成功后，答案写入工作区 `.nars-agent/knowledge.jsonl`（问题归一化后作为键，最多 200 条）。下一次启动 run 时：

1. 载入条目，最近 20 条以 `Known answer — Q: ... A: ...` 追加到 `TaskSpec.memory_brief`，模型在提示词里直接看到；
2. 同时传入 `ActorConfig.prior_knowledge`；
3. 问题与已知条目完全匹配（忽略大小写和空白）时，actor 不再调用 `ask_model`，直接进入收尾，答案来自记忆而不是新调用。

## 边界

- 只做精确匹配，不做语义检索；
- 知识按工作区隔离；
- 答案过长时不截断（超出提示词预算由现有截断处理）。

## 测试

`stores_and_reuses_answers_per_workspace`：写入、大小写/空白归一化命中、同问题覆盖不重复。
