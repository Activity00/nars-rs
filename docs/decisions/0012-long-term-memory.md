# ADR 0012：长期记忆持久化

- 状态：已接受
- 日期：2026-11-09
- 实现：`crates/nars-core/src/memory_snapshot.rs`、`crates/nars-session/src/memory_store.rs`、`NarsPlanner::new_with_snapshot` / `memory_snapshot`、`RuntimeSink::on_memory`

## 背景

在此之前，每个 Run 都从 `Memory::default()` 开始：概念、信念、真值和学到的蕴含随 Run
结束一起消失。跨 Run 留下的只有事件日志（情景记录）和工作区 `experience`（工具成功率
统计）。

这使 nars-agent 退化成“每个任务独立打分”的工具：它不会因为跑过很多次同一个仓库而更
了解这个仓库，也不会保留“上次学到的那条蕴含”。NARS 的长期记忆正是它相对普通 Agent 的
差异点，因此必须持久化。

约束是确定性：同样的输入和事件必须得到同样的决策，历史 Run 必须能重放。

## 决策

### 1. 可序列化的记忆快照（nars-core）

新增 `MemorySnapshot`（schema 版本 1），只保存**语义状态**：

- 概念：term、usage、priority、当前 belief/goal 事件、蕴含表、precondition 表；
- 分类概念列表和 `next_concept_id`；
- **原子表**（`LanguageRecord`）。

term 的 atom 是 `Narsese` 里从 1 开始分配的编号，不是字符串。因此快照必须带原子表，否则
重启后同一个 term 会指向别的名字，记忆会被静默污染。

加载时重建派生结构：`term_slots` / `id_slots`、倒排原子索引；瞬时结构（cycling 事件队列、
occurrence 时间索引）不保存。

原子表兼容规则：存储的原子表必须以当前构建的内置表（`Narsese::new()`）为前缀，且
`self_atom` 一致；否则拒绝加载并报错，绝不做“尽量猜”的恢复。工具名（`^read_file` 等）
按名字 intern，工具顺序变化不影响已存编号。

恢复时还会把 `EventFactory` 的证据编号推进到历史最大值之后（`Memory::max_evidence_id`），
否则新事件会复用旧 id，revision 会把新证据误判为“已经算过”而丢弃。

### 2. 工作区级存储（nars-session）

`MemoryStore` 每个规范工作区保存一份 JSON；`FileMemoryStore` 用工作区路径的哈希作文件名，
写入采用临时文件 + rename，避免半截文件。

作用域选工作区而不是全局：事实和蕴含与具体代码库强相关，跨仓库混合会污染判断。

### 3. 运行期接入（runtime + server）

- Run 启动：`AppState::start_task` 从 `MemoryStore` 读取快照，写入
  `ActorConfig::initial_memory`，由 `NarsPlanner::new_with_snapshot` 恢复 Reasoner。
- Run 结束：driver 在退出前调用 `RuntimeSink::on_memory`，`StoreSink` 把
  `planner.memory_snapshot()` 写回工作区。
- 恢复（resume）：同样加载当前工作区记忆，再把 checkpoint 里的 facts 重新提交。

### 4. 让模型看得见（prompt）

记忆存在但模型看不到等于没存。Run 启动时从快照投影最多 12 条概念，写成
`term [frequency, confidence]` 存入 `TaskSpec.memory_brief`，随 `RunStarted` 持久化，
prompt 以 `long_term_memory` 数组呈现。system prompt 明确：这是先验知识，不是事实基准，
可能变化的内容要先验证。

## 重放与分支

- `memory_brief` 在 `RunStarted` 里，所以重放该 Run 不依赖当前记忆；
- 分支复制前缀事件后，重新从分支的事件投影 history，并按分支所在工作区加载记忆；
- checkpoint 仍然只保存 `PlannerSnapshot`（facts + 统计），不复制整份记忆。

## 已知限制

- 同一工作区并发 Run 采用“后写覆盖”，尚无版本合并或写锁；
- occurrence 时间索引不恢复，重启后时序联想会短暂退化；
- 尚无记忆衰减/容量回收策略，长期运行需要配合 `relative_forgetting` 的定期调用；
- 原子表一旦与构建内置表不兼容，只能拒绝加载；后续需要迁移工具。
