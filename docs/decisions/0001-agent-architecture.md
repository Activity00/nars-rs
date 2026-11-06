# ADR 0001：NARS Agent 总体架构

- 状态：已接受
- 日期：2026-11-06
- 分支：`feat/nars-agent`
- 项目名：`nars-agent`

## 背景

仓库中已经有可运行的 `nars-core`（ONA 兼容的 NAL 推理内核）。目标是构建一个
可持续运行的开发型 Agent：读取和修改项目文件、执行命令与测试、实时展示计划与
工具调用、支持暂停/恢复/重放。

评估过两条捷径：

1. 直接以 `pi-web` 为底座；
2. fork `thClaws` / `Moltis` 等完整 Rust Agent 产品。

两者都会把某个现成 Agent 框架的 LLM 控制循环、会话模型和 UI 一起带进来，导致
NARS 只能作为外挂，而不是决策内核。项目的核心假设是 **NARS 能成为 Agent 的顶层
调度器**，因此必须保留对控制循环的完全所有权。

## 决策

1. **不使用 `pi-web` 作为基础，不 fork 完整 Rust Agent 产品。**
2. **`nars-core` 保持纯推理内核**：不引入 Web、模型 SDK、数据库、工具执行逻辑。
3. **自研 `nars-agent-runtime`**：Session Actor、Agent IR、NARS 决策内核、事件循环。
4. **Web API 使用 Rust + Axum，Web UI 使用 React + TypeScript。**
5. **工具统一经过 Tool Broker**：策略、审批、沙箱、超时、输出截断。
6. **事件日志是唯一事实来源**：JSONL + Snapshot（SQLite 适配器预留），UI 由事件流渲染。
7. 只吸收外部项目的接口与实现经验：
   - `pi-rust`：Provider / Session / Tool / 事件流；
   - `AutoAgents`：Tool Schema、MCP；
   - `Moltis`：持久化、权限、Web API；
   - `thClaws`：聊天、文件、终端、审批交互；
   - `Chidori`：事件重放、暂停恢复、Durable Run。

## 职责边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| `nars-core` | 真值、证据戳、NAL 推理、目标队列、决策候选 | 模型调用、HTTP、文件、数据库 |
| `nars-agent-ir` | Agent IR、Tool/Event 协议、校验、状态摘要 | 执行、网络、NARS 依赖 |
| `nars-model` | Provider 适配、结构化输出解析、重试 | 选择工具、执行工具 |
| `nars-tools` | Manifest、Registry、Broker、Policy、Sandbox、内置工具 | 决定“做什么” |
| `nars-agent-runtime` | Session Actor、NARS 规划器、事件循环、重放 | HTTP、静态资源 |
| `nars-session` | JSONL Event Log、Snapshot、会话目录 | 决策 |
| `nars-server` | Axum API、SSE、静态文件、可选鉴权 | 业务决策 |
| `web` | 三栏 UI、事件流消费、审批交互 | 直接访问文件系统 |
| `narsd` | 组装依赖、读取环境变量、启动服务 | 业务逻辑 |

## 固定执行链路

```text
用户消息
  → 模型理解任务，只输出 Agent IR
  → Rust 校验字段、工具名、路径、上限
  → NARS 写入目标、事实、约束
  → NARS 在候选动作中选择一个
  → Rust 构造 ToolCall，策略判断是否需要审批
  → Tool Broker 执行（沙箱、超时、截断）
  → 工具结果转成观察与 NARS 事实
  → NARS 继续决策；模型解释结果或请求澄清
```

模型**不能**直接调用工具，也不能直接写 NARS 状态。模型输出永远是“提案”，
Rust 层拥有最终解释权。

## 技术选型

| 层次 | 选择 |
|---|---|
| 推理核心 | `nars-core` |
| Agent Loop | `nars-agent-runtime`（自研） |
| 模型适配 | OpenAI-compatible / Anthropic / Ollama / Demo / Echo |
| Web API | Axum 0.8 + Tokio |
| Web UI | React 19 + TypeScript + Vite |
| 工具协议 | 强类型 `ToolManifest` + JSON Schema |
| 持久化 | 会话目录（JSONL Event Log + JSON Snapshot） |
| 状态 | 内存运行 + 周期 Snapshot |
| 沙箱 | workspace canonical path 限制 + 保护路径 |
| 测试基线 | 保留 `upstream/ona` 作为兼容性审计基线 |

## 第一版明确不做

- 多 Agent 协作、Agent Teams
- 自动部署、自动 Git 提交
- 浏览器自动化
- 长期无人值守运行
- 向量数据库
- 桌面应用、语音、多租户、云端托管
- 自动修改自身代码
- Fork 任何现成 Agent 产品

## 后果

正面：

- NARS 的决策过程可审计、可重放；
- 模型可替换，不绑架运行时；
- 工具安全边界集中在一个 Broker；
- UI 与后端共享同一事件源，不会状态漂移。

代价：

- 需要自己维护 Actor、Provider、Broker、持久化；
- NARS 决策质量需要评测集验证；
- v1 的 Planner 仍是“确定性控制器 + NARS 排序”，需要 Phase 6 继续增强。

## 备选方案

| 方案 | 拒绝原因 |
|---|---|
| fork `pi-web` | 控制循环与 UI 深度耦合，NARS 会被降级为插件 |
| fork `thClaws` / `Moltis` | 引入完整产品模型、许可证与升级负担 |
| 直接用 LangChain 类框架 | 控制循环在外部，无法保证 NARS 顶层调度 |
| 先做 UI 再做内核 | 无法验证核心假设“NARS 能否成为决策内核” |

## 验收

- `cargo test --workspace` 通过；
- `web` 可 `npm run build`；
- 不依赖真实模型即可跑通完整 Agent Loop（Scripted Provider）；
- 同一 Run 的事件重放可重建相同状态摘要；
- 高风险操作必须经过策略与审批。
