# ADR 0010：通用项目验证与 `run_command`

- 状态：已接受
- 日期：2026-11-09
- 实现：`crates/nars-tools/src/builtin/run_command.rs`、`crates/nars-server/src/verify.rs`、`TaskPlan`

## 背景

此前的验证路径只有 `cargo_check` / `cargo_test`。这带来两个问题：

1. 非 Rust 仓库无法完成 `modify_code` 计划的 verify 阶段；
2. 即使接入 MCP 测试工具，`TaskPlan` 也不会把它放进 verify allowlist。

结果是 Agent 能修改任意文本文件，但“修改后的验证”实际上是 Rust 专用的。

## 决策

### 1. 新增 `run_command`

```json
{
  "command": "npm test",
  "cwd": ".",
  "timeout_ms": 120000
}
```

- 通过平台 shell 执行（Unix `sh -c`，Windows `cmd /C`）；
- 工作目录被限制在 workspace sandbox 内；
- 默认超时 120s，最大 300s；
- 命令长度上限 8 KiB；
- 副作用为 `ProcessExecution`；
- 默认**需要审批**，除非 policy 的 trusted prefix 命中（默认只有 cargo/git 前缀）；
- `denied_command_patterns` 仍然生效；
- 审批预览显示 `$ <command>`。

Shell 能访问 workspace 之外的路径，因此**审批是主要安全边界**；
`run_command` 不是 OS 级沙箱。

### 2. 项目验证发现

新增 `nars-server::verify::detect`，按顺序：

1. `.nars-agent/project.json` 的 `verify`；
2. 常见项目清单自动检测。

配置示例：

```json
{
  "verify": {
    "label": "unit tests",
    "command": "npm test",
    "cwd": ".",
    "timeout_ms": 120000
  }
}
```

也可以给数组：

```json
{"verify": [{"command": "npm run lint"}, {"command": "npm test"}]}
```

自动检测：

| 项目 | 默认验证命令 |
|---|---|
| `package.json` 有 `scripts.test` | `npm test` |
| `pyproject.toml` / `pytest.ini` / `tests/` | `python3 -m pytest -q` |
| `go.mod` | `go test ./...` |
| `Makefile` 含 `test:` | `make test` |
| `justfile` | `just test` |
| `gradlew` | `./gradlew test` |
| `mvnw` | `./mvnw test` |
| `Package.swift` | `swift test` |
| `mix.exs` | `mix test` |

Cargo 仓库默认仍然使用专用 `cargo_check` / `cargo_test`，避免多一层 shell 和审批。
配置可以覆盖默认行为。

### 3. 计划接入

`TaskSpec` 新增：

```rust
verification: Vec<VerificationCommand>
```

- `start_task` 根据 workspace 检测并填充；
- `TaskPlan` 的 verify 阶段允许 `run_command`；
- 有 verification 时，fallback 候选使用配置命令，而不是 cargo；
- guidance 直接列出 `run_command: <commands>`；
- `run_command` 成功即可满足 verify 阶段；
- `run_command` 在所有阶段都允许（仍需审批），方便安装依赖、构建或检查。

### 4. 测试

- `run_command` 成功、非零退出、cwd 解析、越界拒绝；
- policy 对 `npm test` 要求审批、对 `cargo test` 使用 trusted prefix；
- 自动检测 npm/pytest/go/Makefile；
- 项目配置覆盖自动检测；
- 服务端 RunStarted.task.verification 包含检测结果；
- 端到端非 Rust workspace：read → patch → `run_command` → final，verify 成功。

## 限制

- 一条成功的命令就满足 verify；多命令配置时建议写一个聚合命令；
- 命令由模型/配置提供，审批弹窗是最后防线；
- 没有 OS 级沙箱（无 Landlock/seccomp/容器）；
- 环境变量、交互式命令、后台进程、PTY 尚未支持；
- 自动检测只覆盖常见清单，其他项目需要 `.nars-agent/project.json`；
- `run_command` 在 `--approval auto` 下会自动执行，仅应在受信环境使用。

## 后续

- 多检查项全部通过才推进 verify；
- 命令 allowlist/denylist 的项目级配置；
- 结构化 `run_verify`（语言服务器、诊断、测试报告）；
- 容器/OS 级沙箱；
- 依赖安装与缓存策略。
