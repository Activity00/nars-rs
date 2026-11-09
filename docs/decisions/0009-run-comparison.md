# ADR 0009：Run 对比

- 状态：已接受
- 日期：2026-11-09
- 实现：`crates/nars-server/src/compare.rs`、`GET /api/sessions/{id}/compare`、`ComparePanel`

## 背景

分支树和 lineage 解决了“从哪里来”，但用户还需要回答“这次重试和原 Run 有什么不同”：

- 是否真的完成了？
- 多调用了哪些工具？
- 是否使用了 fallback？
- 改了哪些文件？
- 模型/工具调用次数是否增加？
- 最终回答是否变化？

## 决策

### 对比对象

`GET /api/sessions/{id}/compare?with={parent_id}`：

- **left**：当前 session 的最新 Run；
- **right**：当前 session 的 `branched_from_run_id`（源 Run）；如果源 Run 不存在，则取父 session 的最新 Run。

这样对比的是“分支新 Run”和“父会话源 Run”，不会把复制的历史 Run 计入 left。

### 指标

从事件日志折叠，不依赖 snapshot：

| 指标 | 来源 |
|---|---|
| status | RunStarted / Paused / Completed / Failed / Cancelled |
| steps | NarsDecisionMade.step 最大值 |
| model_calls | ModelRequested 数量 |
| tool_calls | ToolStarted 数量 |
| approvals | ApprovalRequested 数量 |
| fallbacks | NarsDecision.source == fallback |
| decisions | NarsDecisionMade 数量 |
| observations | ObservationRecorded 数量 |
| files | ToolFinished.result.diff 聚合 added/removed |
| tools | ToolStarted.call.tool 计数 |
| final_answer | 最后一条 AssistantMessage |
| duration_ms | 首尾事件时间差 |

文件集合按路径求交集/差集：

- `shared_files`
- `only_left`
- `only_right`

### API 返回

```json
{
  "left": {"session": {...}, "run_id": "...", "metrics": {...}},
  "right": {"session": {...}, "run_id": "...", "metrics": {...}},
  "shared_files": [],
  "only_left": ["src/lib.rs"],
  "only_right": []
}
```

### UI

Inspector 的 Lineage 区块显示 **Compare with parent**：

- 打开模态面板；
- 左右两列展示状态、步骤、模型/工具/审批/fallback/决策/观察、耗时；
- 工具调用计数；
- 文件 diff 行数；
- 最终回答；
- 文件集合差异。

移动端 760px 以下自动变为单列。

## 限制

- 只比较单个 Run，不做多 Run 统计或置信区间；
- 只比较最新 branch Run 和源 Run，不能手选任意两个 Run；
- 文件指标依赖工具返回的 diff，读取/搜索操作不会出现在文件列表；
- 不比较 token usage、NARS 候选评分和完整事件序列；
- 旧分支没有 `branched_from_run_id` 时退化为父会话最新 Run；
- 对比是只读视图，不影响 Run 或会话。

## 后续

- 任意两个 Run 的自由对比；
- 多 Run 重复实验的均值/方差；
- 并排 diff 文本和事件时间线对齐；
- 对比结果导出与评测集联动。
