# ADR 0011：跨 Run 会话上下文

- 状态：已接受
- 日期：2026-11-09
- 实现：`crates/nars-server/src/history.rs`、`TaskSpec.history`、`prompt::plan_message`

## 背景

一个 Session 中的每个 Run 都是自包含、可重放的任务。后续消息如果只说“现在把它改成 beta”，单看当前 `user_message` 模型不知道“它”指什么。

直接读取进程内的对话状态会破坏两个性质：

- 历史 Run 重启后无法重放；
- Session 分支复制事件后，运行时状态与事件不一致。

## 决策

在 Run 启动时，从持久化事件日志投影**已完成的对话轮次**，写入 `TaskSpec.history`，随 `RunStarted` 一起持久化。

```rust
pub struct ChatMessage {
    pub role: ChatRole,
    pub text: String,
    pub run_id: Option<RunId>,
    pub created_at_ms: i64,
}
```

`TaskSpec.history` 只包含更早的轮次；当前用户消息仍在 `task.user_message`，避免重复。

### 选择规则

- 只取同时具备以下条件 Run：
  - `user_message.run_id` 匹配；
  - 收到 `run_completed`；
  - 完成 summary 非空。
- 失败、取消、未完成的 Run 不进入上下文；
- 按用户消息事件序号排序；
- 只保留最近 6 轮；
- 单条消息最多 2000 字符；
- 总字符预算 8000；
- 超出预算时从最旧的轮次开始丢弃。

### Prompt

`plan_message` 会增加：

```json
"conversation": [
  {"role": "user", "text": "..."},
  {"role": "assistant", "text": "..."}
]
```

System prompt 明确要求把 conversation 当背景，当前 `task.user_message` 才是本次指令。

### 重放与分支

- history 存在 `RunStarted.task` 中，事件日志即可重建；
- 分支复制前缀事件后，重新从分支事件投影 history；
- 改写 run_id 时，history 内的 run_id 也会被深替换；
- 旧会话没有 `run_id` 的 user_message 不会贡献上下文，但其余功能正常。

## 限制

- 只保留完成后的 summary，不保留完整 assistant 流式文本、工具输出或 NARS 决策；
- 失败 Run 的上下文不进入下一轮；
- 旧会话（user_message 未绑定 run_id）没有历史；
- 这是字符预算，不是模型 token 精确预算；
- 上下文只按时间选择，没有检索或相关性排序。

## 后续

- 基于 token 的预算；
- 失败 Run 的错误摘要；
- 选择/固定上下文轮次；
- 跨 Session 的经验与检索增强；
- 对话摘要压缩。
