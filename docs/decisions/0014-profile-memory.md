# ADR 0014：跨工作区 profile 记忆

- 状态：已接受
- 日期：2026-11-09
- 实现：`crates/nars-session/src/profile.rs`、`crates/nars-server/src/memory_context.rs`（`promote_facts`）、`GET/POST /api/memory/profile`

## 背景

[ADR 0012](0012-long-term-memory.md) 把记忆限定在工作区：不同仓库完全隔离。这保护了仓库特定事实，但也意味着每换一个仓库，代理都要重新学一遍"工具怎么表现、计划阶段怎么推进、用户偏好什么"，而且用户说过一次的偏好无法带到下一个项目。

有些知识天然是代理级的，不属于任何仓库：

- 工具与计划结果的成败（`tool_*`、`step_*`、`plan_*`）；
- 用户偏好（例如"用 pnpm 而不是 npm"）；
- 运行级约定（审批习惯、常用命令）。

## 决策

### 1. 独立的 profile 存储

```text
.nars-agent/memory/profile.json
{
  "version": 1,
  "generation": 7,
  "profile": { "version": 0, "facts": [NarsFact, ...] }
}
```

与工作区记忆同目录，但独立文件、独立 generation。写入沿用乐观并发：generation 不匹配时重试最多 3 次，仍冲突则放弃并告警。

### 2. 注入：作为先验信念

每个 Run 启动时（有或没有工作区记忆）把 profile facts 逐条 `commit_fact` 到 NARS。无法解析的 term 跳过，不影响 Run。这样 profile 事实进入工作区记忆，参与推理和 prompt 的 `long_term_memory` 摘要。

### 3. 提升：只沉淀代理词表事实

Run 结束时从工作区记忆里挑选**原子 term 且带代理词表前缀**的概念：

```text
tool_  step_  plan_  op_  approval_  run_
```

提升为 `NarsFact`（term 字符串 + 当前 belief 真值），按 term 去重、高置信度版本胜出，上限 200 条。

不整表合并概念图，原因有三：

1. 原子编号按工作区独立分配，合并要先按名字重映射两张表；
2. 每张表上限 `ATOMS_MAX = 255`，把多个仓库的词表合进 profile 会很快耗尽；
3. 仓库专用概念混进 profile 会污染其它仓库的判断。

### 4. 手工与检视

- `GET /api/memory/profile`：返回是否可用、generation、事实数与前 20 条。
- `POST /api/memory/profile/facts`：写入任意 term（默认 frequency 1.0、confidence 0.9），用于用户偏好这类无法自动识别的知识。
- Web UI 的 **Memory** 面板同时展示工作区记忆与共享 profile。

## 限制

- 提升是启发式词表匹配，非代理词表的可移植知识不会自动进入 profile；
- profile 只存事实，不存学到的蕴含（规则仍留在各工作区）；
- 同名 term 在不同仓库共享真值：这是共享的目的，但如果两个仓库对同一词含义不同，会互相影响；
- 没有逐条删除/审查 profile 的接口，只能通过 API 覆盖或手工编辑文件。

## 验证

- `profile_facts_are_shared_between_workspaces`：POST 一条偏好 → 第一个工作区记忆载入该 term → profile 里出现该偏好和 `tool_` 事实 → 第二个工作区同样载入；
- `promotes_only_agent_vocabulary_facts`：仓库概念不被提升；
- `round_trips_and_guards_generations`、`merge_keeps_the_most_confident_version`、`merge_is_bounded`：存储、去重与上限。
