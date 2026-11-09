# nars-agent 快速开始

nars-agent 是一个由 NARS 决策的开发型 Agent。模型只输出结构化的 Agent IR，
NARS 决定下一步动作，工具经过沙箱、策略和审批后执行，所有过程以事件流记录并可重放。

## 1. 准备

- Rust 1.85+
- Node.js 20+（仅在构建 Web UI 时需要）
- 可选：OpenAI-compatible / Anthropic / Ollama 的 API

```bash
cargo build -p narsd
```

## 2. 无 API Key 的离线演示

```bash
cargo run -p narsd -- --approval ask
```

默认使用 `demo` provider：它会自动执行 `list_files → read_file → 总结`，用于验证
NARS 决策、工具 Broker、SSE、UI 和事件日志全链路（只读，不修改文件）。
设置 `NARS_AGENT_PROVIDER=echo` 可切换到纯回显模式。
服务地址：<http://127.0.0.1:8787>。

## 3. 接入真实模型

### OpenAI-compatible（OpenAI、DeepSeek、OpenRouter、vLLM、LM Studio）

```bash
NARS_AGENT_PROVIDER=openai \
NARS_AGENT_BASE_URL=https://api.openai.com/v1 \
NARS_AGENT_API_KEY=sk-... \
NARS_AGENT_MODEL=gpt-4.1-mini \
cargo run -p narsd
```

### Ollama

```bash
NARS_AGENT_PROVIDER=ollama \
NARS_AGENT_BASE_URL=http://localhost:11434 \
NARS_AGENT_MODEL=qwen3 \
cargo run -p narsd
```

### Anthropic

```bash
NARS_AGENT_PROVIDER=anthropic \
NARS_AGENT_API_KEY=sk-ant-... \
NARS_AGENT_MODEL=claude-sonnet-4-5 \
cargo run -p narsd
```

可选变量：

| 变量 | 默认 | 说明 |
|---|---|---|
| `NARS_AGENT_PROVIDER` | `demo` | `openai` / `anthropic` / `ollama` / `demo` / `echo` |
| `NARS_AGENT_MODEL` | `gpt-4.1-mini` | 模型名 |
| `NARS_AGENT_BASE_URL` | 各 provider 默认 | API 根地址 |
| `NARS_AGENT_API_KEY` | 无 | 也读取 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` |
| `NARS_AGENT_JSON_SCHEMA` | 未设置 | 设为 `1` 时请求严格 JSON Schema |
| `NARS_AGENT_TOKEN` | 无 | 设置后 API 需要 Bearer token |

## 4. Web UI

开发模式（Vite 代理 `/api` 到 8787）：

```bash
cd web
npm install
npm run dev
# http://localhost:5173
```

生产模式：

```bash
cd web && npm run build && cd ..
cargo run -p narsd     # narsd 自动服务 web/dist
```

界面三栏：

1. **左**：会话、workspace、工具、连接状态；
2. **中**：对话、NARS 决策、工具调用、diff、审批；
3. **右**：目标、NARS goal atom、事实、候选评分、文件变化、控制按钮。

## 5. 使用流程

1. 在左侧输入项目绝对路径，点击 **New session**；
2. 在输入框描述任务，例如“找到失败的测试并修复”；
3. 模型提出候选动作；
4. 右侧显示任务计划（investigate → change → verify）、NARS 评分与选择结果；
5. 只读工具自动执行；`apply_patch` 会暂停并展示 diff；
6. 点击 **Approve & run** 或 **Deny**；
7. 顶部 **Run** 选择器可以切换 `All runs` 或单个 Run，时间线与 Inspector 会跟随过滤；
   服务重启后历史 Run 仍可回看；
8. 在已完成的 Run 上点击 **Branch & retry**：复制此前 Run 到新会话、重写 run/session id，
   写入父会话 lineage，并重新执行当前任务；原会话保持不变；
9. 侧栏按 lineage 嵌套展示分支树，分支显示 `↳`；顶栏 **branch of ...** 与 Inspector **Lineage** 可跳转父会话；
10. 选中会话可 **Rename** / **Delete**，删除当前会话后自动切换；
11. 分支 Run 可 **Compare with parent**：面板内左右各选一个 Run，对比状态、工具、fallback、文件变化和最终回答；
12. 观察工具输出、NARS 事实、下一次决策；
13. 可随时 **Pause / Resume / Cancel**。

## 5.5 接入 MCP 工具

创建 `mcp.json`：

```json
{
  "servers": [
    {
      "name": "files",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path"],
      "side_effect": "external_side_effect",
      "requires_approval": true
    }
  ]
}
```

启动：

```bash
cargo run -p narsd -- --mcp-config mcp.json
# 或
NARS_AGENT_MCP_SERVERS='[{"name":"files","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/tmp"]}]' \
  cargo run -p narsd
```

HTTP MCP server：

```json
{
  "servers": [
    {
      "name": "remote",
      "url": "https://example.com/mcp",
      "headers": {"authorization": "Bearer ..."},
      "requires_approval": true
    }
  ]
}
```

工具会以 `mcp_<server>_<tool>` 出现在 `/api/tools`、提示词和 Web UI 中。默认外部副作用 + 审批；
连接失败只记录 warning，不影响 `narsd` 启动。详见 [ADR 0005](../decisions/0005-mcp.md)。

## 5.6 非 Rust 项目验证

非 Rust 项目使用 `run_command`，默认需要审批。可以在仓库中创建
`.nars-agent/project.json`：

```json
{
  "verify": {
    "label": "unit tests",
    "command": "npm test",
    "timeout_ms": 120000
  }
}
```

也支持 `"verify": [{"command":"npm run lint"},{"command":"npm test"}]`。

未配置时自动检测：

- `package.json` 有 `scripts.test` → `npm test`
- `pyproject.toml` / `pytest.ini` / `tests/` → `python3 -m pytest -q`
- `go.mod` → `go test ./...`
- `Makefile` 含 `test:` → `make test`
- `gradlew` / `mvnw` / `Package.swift` / `mix.exs` 等

Cargo 仓库默认继续使用 `cargo_check`/`cargo_test`。命令在 workspace 内执行，
但 shell 本身不是 OS 沙箱，审批是主要安全边界。详见
[ADR 0010](../decisions/0010-generic-verification.md)。

## 5.7 跨 Run 上下文

同一 Session 的后续消息会带上前 6 个已完成 Run 的问答摘要，因此可以直接说
“现在把它改成 beta”。上下文有字符预算，且只取成功的 Run；
历史写入 `RunStarted.task.history`，重启和分支后仍可重放。详见
[ADR 0011](../decisions/0011-session-context.md)。

## 6. 审批策略

```bash
cargo run -p narsd -- --approval ask    # 默认：读自动，写/网络/历史操作审批
cargo run -p narsd -- --approval auto   # 自动批准（仅受信环境）
cargo run -p narsd -- --approval deny    # 自动拒绝所有需审批操作
```

高风险操作（网络、Git 历史、workspace 外副作用）默认需要审批。
`.git`、`.env` 等路径永远禁止写入。

## 7. HTTP API

```bash
# 健康检查
curl http://127.0.0.1:8787/api/health

# 创建会话
curl -X POST http://127.0.0.1:8787/api/sessions \
  -H 'content-type: application/json' \
  -d '{"workspace":"/absolute/path/to/project","title":"demo"}'

# 发送任务
curl -X POST http://127.0.0.1:8787/api/sessions/<session_id>/messages \
  -H 'content-type: application/json' \
  -d '{"text":"inspect this repository"}'

# 查看 Run
curl http://127.0.0.1:8787/api/runs/<run_id>

# SSE 事件流
curl -N http://127.0.0.1:8787/api/sessions/<session_id>/events

# 审批
curl -X POST http://127.0.0.1:8787/api/tool-calls/<call_id>/approve \
  -H 'content-type: application/json' \
  -d '{"approved":true}'

# 暂停 / 恢复 / 取消
curl -X POST http://127.0.0.1:8787/api/runs/<run_id>/pause
curl -X POST http://127.0.0.1:8787/api/runs/<run_id>/resume
curl -X POST http://127.0.0.1:8787/api/runs/<run_id>/cancel

# 事件重放校验：返回投影状态、快照哈希、是否一致
curl 'http://127.0.0.1:8787/api/sessions/<session_id>/replay?run_id=<run_id>'

# 从最新 checkpoint 恢复中断的 Run
curl -X POST http://127.0.0.1:8787/api/sessions/<session_id>/resume
```

如果设置了 `NARS_AGENT_TOKEN`，所有 `/api` 请求（除 `/api/health`）需要：

```bash
-H 'Authorization: Bearer <token>'
# SSE 可用 ?token=<token>
```

## 8. 持久化

```text
.nars-agent/sessions/ses_<id>/
├── meta.json
├── events.jsonl
└── snapshot.json
```

- `events.jsonl` 是事实来源；
- `snapshot.json` 是 checkpoint，含 `state_hash`；
- Run 列表与历史状态由事件日志投影，不依赖进程内存；
- `../experience/<hash>.json` 保存同一 workspace 的工具成功率，供后续 Run 复用；
- `../memory/<workspace-hash>.json` 保存该 workspace 的 NARS 长期记忆
  （概念、信念、蕴含、原子表）：Run 启动时加载，结束时写回；
- 记忆快照带 schema 版本和原子表；原子表与当前构建不兼容时拒绝加载，
  不会用错位的编号污染记忆；
- `GET /api/sessions/<id>/memory` 返回该工作区记忆摘要（概念、蕴含、原子数），
  Web UI 顶栏 **Memory** 按钮可查看；
- 也可以使用 SQLite：`cargo run -p narsd -- --sqlite .nars-agent/sessions.db`；
- 服务重启后会话仍在，工作区记忆也仍在；
- 同一事件流重放得到相同 `state_hash`（集成测试覆盖）。

## 9. 测试

```bash
cargo test -p nars-agent-ir -p nars-model -p nars-tools \
           -p nars-session -p nars-agent-runtime -p nars-server

# 全量（含 nars-core 上游兼容测试）
cargo test --workspace

# Web 类型检查 + 构建
cd web && npm run build

# 确定性评测集（6 个任务）
cargo run -p nars-eval -- --provider scripted \
  --out target/eval-report.json --markdown-out target/eval-report.md

# 本地完整门禁（fmt + agent tests + eval + web build；--full 包含全工作区 release 测试）
./scripts/agent-check.sh --full
```

## 10. 当前限制

- NARS 已具备线性阶段计划（investigate → change → verify）、阶段门控、失败回退和 workspace 经验复用；任意 DAG 与并行子任务待增强；
- 持久化支持 JSONL 文件与 SQLite；
- 沙箱是路径级，不是容器级；不要在生产环境运行不可信任务；
- MCP、浏览器、Git 提交尚未接入；
- 第一版不做多 Agent、向量库、桌面端。

## 11. 进一步阅读

- [架构总览](../architecture/overview.md)
- [ADR 0001：总体架构](../decisions/0001-agent-architecture.md)
- [ADR 0002：Agent IR](../decisions/0002-agent-ir.md)
- [ADR 0003：工具协议](../decisions/0003-tool-protocol.md)
- [事件协议](../protocol/events.md)
