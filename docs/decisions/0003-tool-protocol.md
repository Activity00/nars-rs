# ADR 0003：工具协议、策略与沙箱

- 状态：已接受
- 日期：2026-11-06
- 实现：`crates/nars-tools`、`crates/nars-agent-ir/src/tool.rs`

## 决策

工具是唯一允许接触文件系统和子进程的组件。所有调用经过同一入口
`ToolBroker`，顺序固定：

```text
ToolCall
  → 查找 manifest
  → Policy 判定（Allow / RequireApproval / Deny）
  → Sandbox 路径检查（工具内部）
  → spawn_blocking 执行
  → 超时 / 异常处理
  → 输出截断
  → ToolResult
```

## Manifest

```rust
pub struct ToolManifest {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub side_effect: SideEffect,
    pub timeout_ms: u64,
    pub requires_approval: bool,
    pub version: String,
    pub tags: Vec<String>,
}
```

`input_schema` 是 JSON Schema，同时用于提示词、MCP 转换和后续参数校验。

## 副作用分级

| 级别 | 含义 | 默认策略 |
|---|---|---|
| `ReadOnly` | 只读 | 自动执行 |
| `WorkspaceWrite` | 写 workspace | 若 manifest 要求审批则审批，否则执行 |
| `ProcessExecution` | 启动子进程 | 可信工具自动，其他审批 |
| `NetworkAccess` | 访问网络 | 审批 |
| `GitHistoryChange` | 改写 Git 历史 | 审批 |
| `ExternalSideEffect` | workspace 外副作用 | 审批 / 不提供 |

审批模式：

- `ask`：遵循上述策略；
- `auto`：把 `RequireApproval` 自动升级为 `Allow`（测试 / 受信环境）；
- `deny`：把 `RequireApproval` 自动降级为 `Deny`（CI / 只读演示）。

## 调用与结果

```rust
pub struct ToolCall {
    pub id: ToolCallId,
    pub run_id: RunId,
    pub tool: String,
    pub arguments: Value,
    pub side_effect: SideEffect,
    pub requires_approval: bool,
    pub created_at_ms: i64,
}

pub struct ToolResult {
    pub call_id: ToolCallId,
    pub status: ToolStatus, // succeeded | failed | denied | timed_out | cancelled
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
    pub diff: Option<Diff>,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
    pub metadata: Map<String, Value>,
}
```

结果会被转换成 `Observation`，再转成 NARS 事实（`step_3_outcome_success`、
`file_..._modified`、`check_failed` 等）。

## 第一版内置工具

| 工具 | 副作用 | 审批 | 说明 |
|---|---|---|---|
| `read_file` | ReadOnly | 否 | UTF-8 读取，16 MiB 上限，支持 offset/limit |
| `list_files` | ReadOnly | 否 | 跳过 `.git`/`target`/`node_modules` |
| `search` | ReadOnly | 否 | 字面量搜索，`path:line: text` |
| `apply_patch` | WorkspaceWrite | 是 | 精确文本替换，原子写入，返回 unified diff |
| `cargo_check` | ProcessExecution | 否 | `cargo check --message-format=short` |
| `cargo_test` | ProcessExecution | 否 | `cargo test` |

`apply_patch` 的输入支持两种形式：

```json
{"path":"src/lib.rs","old_text":"alpha","new_text":"beta","replace_all":false}
```

```json
{"patches":[{"path":"a.rs","old_text":"x","new_text":"y"}],"dry_run":false}
```

规则：

- `old_text` 必须唯一匹配，除非 `replace_all=true`；
- 空 `old_text` 仅用于创建新文件；
- 多文件修改先全部在内存中计算，再统一原子写入；
- `dry_run=true` 只返回 diff，不落盘（审批预览使用）；
- 默认 30 秒超时，单文件 16 MiB 上限。

## 沙箱

`nars_tools::Sandbox`：

- 构造时 canonicalize workspace root；
- 拒绝绝对路径、`..`、NUL；
- 已存在路径 canonicalize 后必须仍在 root 下（防 symlink 逃逸）；
- 新文件检查父目录 canonicalize；
- 禁止写入 `.git`、`.env`、`.env.local`、`.env.production`；
- 读取同样限制在 root 下。

沙箱不是完整安全边界。WASM / Landlock / 容器是后续阶段。

## 超时与输出

- 每个 manifest 声明 `timeout_ms`；
- Broker 用 `tokio::time::timeout` 包裹 `spawn_blocking`；
- 进程工具内部也会轮询 `try_wait` 并在截止时间 kill 子进程；
- stdout/stderr 各截断到 64 KiB，`truncated=true`；
- 子进程 stdout/stderr 由独立线程排空，避免管道阻塞死锁。

## 审批流

```text
ToolCall
  → Policy.RequireApproval
  → ApprovalRequested { call, reason, preview }
  → UI 显示 diff
  → POST /api/tool-calls/:id/approve
  → ApprovalResolved
  → 执行 或 生成 Denied 结果
```

拒绝不是错误终止：`Denied` 会作为观察回到 NARS，模型可以换一种做法。

## MCP 扩展（Phase 7）

MCP 工具的 manifest 由外部 server 的 `tools/list` 转换而来：

- `inputSchema` 直接映射；
- 默认 `ExternalSideEffect`，必须审批；
- 输出转换为 `ToolResult`，再进入同一观察管道；
- 外部工具崩溃不能阻塞 Session。

## 测试

- 沙箱路径穿越、symlink 逃逸、保护路径；
- 读取、分页、二进制拒绝；
- 搜索、列目录；
- patch 创建/替换/歧义拒绝/diff 预览；
- 策略 allow/deny/auto；
- Broker 超时；
- 端到端 read → patch → final 与审批/拒绝流程。
