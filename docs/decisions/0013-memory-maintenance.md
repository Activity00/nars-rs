# ADR 0013：记忆的并发、衰减与迁移

- 状态：已接受
- 日期：2026-11-09
- 实现：`AppState::claim_workspace`、`MemoryStore::save(generation)`、`Memory::prune_stale`、`Reasoner::maintain_memory`、`memory_snapshot::migrate`

## 背景

[ADR 0012](0012-long-term-memory.md) 把长期记忆落盘后，留下三个会实际影响正确性的缺口：

1. 同一工作区并发 Run 各自加载同一份记忆，结束时后写覆盖先写，先写学到的东西丢失；
2. 记忆只增不减，旧概念的高优先级会长期压过新证据，决策变慢变钝；
3. 存储的原子表与当前构建不兼容时只能拒绝加载，升级构建等于丢记忆。

## 决策

### 1. 并发：进程内串行 + 存储层 generation

**进程内**：`AppState` 维护一个 `active_workspaces` 集合。启动新 Run、恢复 Run、分支重跑都要先 `claim_workspace`；工作区已被占用时返回 `409 Conflict`，不会两个 Run 同时改同一份记忆。Run 终止时 `StoreSink::on_memory` 释放占用。

**存储层**：记忆文件带 `generation`（单调递增）。`save` 必须传入加载时的 generation：

- 文件不存在 → 期望 `None`，写入后 generation = 1；
- 文件存在 → 期望当前 generation，成功后 +1；
- 不匹配 → `StoreError::Conflict`，本次写回放弃并告警，磁盘上是更新的那份。

这样即使有第二个 `narsd` 进程或手工修改，也不会被静默覆盖。

### 2. 衰减与回收

每次新 Run 启动、恢复记忆后执行一次 `maintain_memory`：

1. `Memory::relative_forgetting()`：概念优先级 × 0.9、事件优先级 × 0.9999（沿用 ONA 常量）；
2. `Memory::prune_stale()`：删除 `priority < 0.01` 且 `usefulness < 0.005` 且**没有任何蕴含**的概念。

带规则的概念不删：规则是记忆里最贵的部分，低频规则可能重新变得相关。容量上限仍由 `CONCEPTS_MAX` 的既有淘汰逻辑兜底。

### 3. 原子表迁移

`is_compatible` 判断存储表是否以当前构建的内置表为前缀且 `SELF` 一致。不兼容时 `migrate`：

1. 以当前 `Narsese::new()` 为基底；
2. 按**名字**把旧原子逐个映射到新编号，构建不认识的旧名字重新 intern 到表尾；
3. 用映射重写所有 term 的原子字节（term 是定长原子数组，逐字节替换即可，复合结构自动保留）；
4. 保留 concept id、usage、priority、信念、蕴含、证据 stamp。

迁移只改原子编号，不改语义；迁移后仍不兼容才报错。

## 取舍与限制

- 暂停中的 Run 仍占用工作区，暂停期间不能在同一工作区启动新 Run；
- 多进程并发只由 generation 拦住：不会丢数据，但落后的一方会放弃写回（日志告警），不做自动合并；
- 衰减频率是“每个 Run 一次”，不是推理周期级别；高强度连续推理的会话衰减会偏慢；
- 迁移不做原子表压缩，旧构建的废弃原子会一直留在表尾。

## 验证

- `rejects_a_stale_generation`：generation 不匹配时拒绝写回；
- `busy_workspace_rejects_a_second_run`：占用工作区时第二次启动返回 409；
- `migrates_a_shifted_atom_table`：整表偏移后迁移，term 渲染结果与原始一致；
- `migrates_memory_with_a_drifted_atom_table`：planner 对漂移表仍能恢复记忆；
- `prunes_decayed_concepts_but_keeps_learned_rules`：衰减后清理无规则概念、保留带规则概念。
