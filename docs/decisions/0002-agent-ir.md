# ADR 0002：Agent IR 与模型输出约束

- 状态：已接受
- 日期：2026-11-06
- 实现：`crates/nars-agent-ir`、`crates/nars-model`

## 背景

让模型直接输出工具调用（function calling）会把“选择哪个工具、何时停止、失败后
怎么办”交给模型。NARS 项目需要相反的约束：

- 模型负责语言理解与代码生成；
- NARS 负责目标、事实、优先级和下一步选择；
- Rust 负责校验、权限、执行和审计。

因此模型只能输出一个受限的结构化对象，称为 **Agent IR**。

## 决策

模型每次响应必须是一个 JSON 对象，且 `kind` 只能是以下四种之一：

| kind | 用途 |
|---|---|
| `task_proposal` | 理解任务并给出候选动作 |
| `action_parameters` | 为 NARS 已选动作补全参数（预留） |
| `final_answer` | 任务完成，向用户总结 |
| `clarification` | 信息不足，向用户提问 |

### Task Proposal

```json
{
  "kind": "task_proposal",
  "goal": {
    "type": "modify_code",
    "target": "crates/nars-core/src/reasoner.rs",
    "description": "修复决策队列的边界条件"
  },
  "constraints": ["保持现有 API", "修改后运行相关测试"],
  "candidate_actions": [
    {
      "tool": "search",
      "arguments": {"query": "decision queue", "path": "crates/nars-core"},
      "rationale": "先定位相关代码",
      "confidence": 0.8
    }
  ],
  "confidence": 0.85
}
```

`goal.type` 取值：`modify_code`、`answer_question`、`investigate`、`run_tests`、
`refactor`、`create_files`、`other`。

### Final Answer

```json
{"kind":"final_answer","text":"已完成修复并通过 cargo test。","success":true,"artifacts":["crates/nars-core/src/reasoner.rs"]}
```

### Clarification

```json
{"kind":"clarification","question":"要修改哪个 crate？","options":["nars-core","nars-tools"]}
```

## 校验规则

校验入口：`nars_agent_ir::validate_ir`。

| 检查 | 处理 |
|---|---|
| `kind` 合法 | 拒绝 |
| 工具名在注册表中 | 拒绝 |
| `arguments` 是对象 | 拒绝 |
| 路径字段为相对路径、无 `..`、无 NUL | 拒绝 |
| 候选动作 ≤ 16 | 拒绝 |
| 约束 ≤ 32 | 拒绝 |
| 文本长度上限 | 拒绝 |
| 候选 id 重复 | 拒绝 |
| 重复候选动作 | 警告 |
| 空 `goal.description` | 警告，运行时补全 |

非法输出不会进入 NARS。运行时先尝试一次修复（把校验错误反馈给模型），
仍失败则 `RunFailed(Validation)`。

## NARS 映射

NARS 不保存大段代码、日志或用户原文。运行时只写入短小的符号事实：

```text
task_<hash>_goal_modify_code
task_<hash>_target_crates_nars_core_src_reasoner_rs
task_<hash>_constraint_0_keep_api
step_3_outcome_failure
tool_search_outcome_success
file_src_lib_rs_modified
check_failed
```

完整内容保存在 Event Log、Tool Result 和文件系统中；NARS 只保存可用于推理的
摘要和引用。这样避免 Term 数量爆炸，也避免把不可信文本注入逻辑层。

### 转义

所有来自用户/模型/文件路径的字符串都经过 `nars_agent_ir::atomize`：

- ASCII 字母数字保留并小写；
- 其他字符折叠为 `_`；
- 截断到 48 字符；
- 数字开头加 `n_` 前缀；
- 空串映射为 `nil`。

原文仍保留在事件日志中，供 UI 和模型回读。

## 控制流

```text
模型 → AgentIr
     → validate_ir
     → planner.commit_task（目标/事实/约束）
     → planner.decide（候选打分 + NARS 选择）
     → ToolCall → Policy → Broker
     → Observation → NARS facts
     → 下一次模型调用
```

`task_proposal.candidate_actions` 为空时，运行时会请求 `finalize`，而不是让模型
继续空转。

## 版本与兼容

- `AgentIr` 使用 `#[serde(tag = "kind")]`，未知字段允许通过（前向兼容）。
- JSON Schema 由 `nars_model::agent_ir_schema()` 提供，随 IR 同步维护。
- 新增 kind 需要同时更新：
  1. `AgentIr` 枚举；
  2. `validate_ir`；
  3. JSON Schema；
  4. 运行时分支；
  5. 回归测试。

## 后果

- 模型无法绕过 NARS 直接执行 `rm -rf`、`git push` 等操作；
- 模型输出可被单元测试完全覆盖；
- 代价是模型需要理解 IR 约定，提示词必须携带 Schema。

## 测试

- `crates/nars-agent-ir`：协议解析、校验、路径安全、上限。
- `crates/nars-model`：结构化输出提取、代码围栏、常见形状修复。
- `crates/nars-agent-runtime`：非法工具名/参数被拒绝，合法提案进入 NARS。
