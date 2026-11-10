# ADR 0017：从 Narsese 目标派生动作（第一切片）

- 状态：已接受
- 日期：2026-11-10
- 实现：`GoalSpec.narsese`、`NarsPlanner::derive_action`、`SessionActor::handle_proposal`

## 背景

此前模型与 NARS 的边界是 Agent IR（候选动作 + 参数）：动作和参数在模型侧就定了，NARS 只做排序与仲裁。目标形态是“模型把用户请求翻译成 Narsese，动作由 NARS 决定”。

## 决策

### 1. 目标可携带 Narsese

`GoalSpec` 增加可选 `narsese` 字段。模型在提案里同时给出自然语言目标与 Narsese 翻译，例如：

```json
{"goal": {"type": "other", "description": "read alpha", "narsese": "(read_file * alpha)"}}
```

### 2. NARS 派生动作

`NarsPlanner::derive_action` 解析目标项，取 `(tool * arg)` 的工具与单个字符串参数；工具必须在已注册操作里。运行时据此构造 `CandidateAction`，**忽略模型的候选列表**，执行该动作，并把决策记为 `model_proposed: false`。

### 3. 作用范围（第一切片）

- 仅支持“单个字符串参数”的工具：`read_file`、`list_files`、`search`；
- 使用乘积形式 `(tool * arg)`；普通括号列表在解析时会被拼接，不适用；
- 参数取原子名，路径中的点、斜杠等需要后续的编码约定。

## 验证

`nars_derives_action_from_a_translated_goal`：模型只给翻译和最终文本，NARS 推出 `read_file(path=alpha)` 并执行，工具序列为 `["read_file"]`，决策 `model_proposed = false`。

## 后续

- 多参数工具的参数编解码（manifest schema ↔ term）；
- 把翻译独立成一次模型调用（当前借用提案调用）；
- `ask_model` 作为 NARS 可调用的操作，用于冷启动补知识。
