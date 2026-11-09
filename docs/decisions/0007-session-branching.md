# ADR 0007：Session 分支与 Branch & retry

- 状态：已接受
- 日期：2026-11-09
- 实现：`AppState::branch_session`、`POST /api/sessions/{id}/branch`、Inspector 的 **Branch & retry**、`SessionMeta` lineage

## 背景

Run 历史已经可以持久化回看，但用户还需要从某个历史 Run 重新尝试：换模型、换提示策略、
或对比不同执行路径，而不破坏原会话的审计记录。

直接复制整个会话会带来两个问题：

1. Run ID 在父子会话中重复，`/api/runs/:id` 无法区分；
2. 复制一个已完成 Run 的事件，会让新会话看起来已经执行过同一任务。

## 决策

**从选中 Run 的起点分支，并重新执行该任务。**

分支点取该 Run 的最早事件（通常是绑定 `run_id` 的 `user_message`，其次是 `run_started`）。
分支会话包含：

- 分支点之前的全部事件（即更早的 Run）；
- 新任务的 `user_message` 和新的 `run_started`；
- 不包含被选中 Run 本身的任何事件。

这样语义清晰：**“带着之前的历史，重试这个 Run”**。

## 算法

```text
POST /api/sessions/{parent}/branch { "run_id": "run_..." }

1. 读取 parent 事件与 meta；
2. 找到 source run 的最早事件序号，得到前缀 prefix；
3. 校验 source run 已终止（或进程内非 live）；
4. 为 prefix 中每个旧 run_id 生成新 run_id；
5. 创建新 session（同 workspace，标题 Branch · ...），写入
   `parent_session_id` / `branched_from_run_id`；
6. 将 prefix 事件复制到新 session：
   - 深度遍历 JSON，把旧 run_id 替换为新 run_id；
   - 把旧 session_id 替换为新 session_id；
7. 用 RunStarted.task 重新启动一个全新 Run；
8. 返回新 session、新 run_id、复制事件数。
```

重写是必要的：`RunStarted.session_id`、`UserMessage.session_id`、工具调用和观察中的
`run_id` 必须与分支会话一致，否则审计和重放会出现跨会话引用。

## 约束

- 只能从**已终止**的 Run 分支：completed / failed / cancelled；
- 进程内仍处于 planning / executing / paused 的 Run 返回 409；
- 进程重启后，若事件中没有终止事件，也拒绝分支；
- 分支会重新执行任务，因此需要模型/工具再次运行；
- 不复制 snapshot：分支状态由复制后的事件投影得到，新 Run 会生成自己的 snapshot。

## UI

Inspector 在 completed / failed / cancelled Run 上显示 **Branch & retry**：

1. 调用分支 API；
2. 刷新会话列表；
3. 选中新会话；
4. SSE 回放复制事件和新 Run 的实时事件。

## 限制

- 没有分支树折叠/展开视图，侧栏只显示单层 `↳`；
- 不支持从 Run 中间事件分支（例如某个工具观察之后）；
- 前缀中的未完成 Run 会按原样复制；
- 复制的前缀 Run 使用新 Run ID，外部书签里的旧 Run ID 不会指向分支副本；
- 分支标题截断到 48 字符。

## 后续

- 分支树 / 对比视图；
- 从任意事件位置 fork 并注入新的用户指令；
- 分支级 experience 隔离与对比评测。
