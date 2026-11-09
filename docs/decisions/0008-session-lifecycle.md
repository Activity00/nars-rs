# ADR 0008：会话元数据、重命名与删除

- 状态：已接受
- 日期：2026-11-09
- 实现：`SessionMeta`、`SessionStore::create_session_with_lineage` / `delete_session`、`PATCH/DELETE /api/sessions/{id}`

## 背景

Session 分支已经能创建新会话，但最初只通过标题表达来源；用户也无法重命名或删除会话。
这会导致：

- 无法从数据层区分父子会话；
- 多分支后侧栏难以辨认；
- 测试/临时会话无法清理；
- 旧分支缺少可追溯的 lineage。

## 决策

### 1. 正式记录 lineage

`SessionMeta` 增加两个可空字段：

```text
parent_session_id: Option<SessionId>
branched_from_run_id: Option<RunId>
```

分支创建时写入；普通会话为 `null`。旧文件 meta 通过 serde default 兼容；旧 SQLite
数据库启动时自动 `ALTER TABLE` 增加列。

### 2. 重命名

```http
PATCH /api/sessions/{id}
{ "title": "new title" }
```

复用 `touch_session`，同时更新 `updated_at_ms` 和标题。空标题返回 400。

### 3. 删除

```http
DELETE /api/sessions/{id}
```

- 文件存储删除整个 session 目录；
- SQLite 在事务中删除 `events`、`snapshots`、`sessions`；
- 删除进程内该 session 的 RunHandle；
- 如果仍有非终止 Run，返回 409，避免删除正在执行的任务；
- 删除不可恢复。

### 4. UI

- 侧栏按 `parent_session_id` 嵌套展示分支树，分支会话显示 `↳` 并按层级缩进；
- 顶栏显示 **branch of ...** 链接，可直接跳到父会话；
- Inspector 的 **Lineage** 区块显示父会话、源 Run 短 ID 和直接子分支数；
- 选中会话下方显示 **Rename** / **Delete**；
- Rename 使用浏览器 prompt，Delete 使用 confirm；
- 删除当前会话后自动选中下一个会话，或进入空状态。

## 存储细节

文件：

```text
meta.json
{
  ...,
  "parent_session_id": "ses_...",
  "branched_from_run_id": "run_..."
}
```

SQLite：

```sql
ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;
ALTER TABLE sessions ADD COLUMN branched_from_run_id TEXT;
```

迁移是幂等的：通过 `PRAGMA table_info(sessions)` 检查后再添加。

## 限制

- 旧分支（本次改动前创建）没有 lineage，侧栏不会显示 `↳`；
- 删除是硬删除，没有回收站/软删除；
- 删除只检查当前进程内的 live Run，跨进程并发写不在保护范围；
- 有子分支的父会话不能直接删除，需要先删除分支，避免 lineage 悬空；
- 分支树只做嵌套缩进展示，不支持折叠/展开或分支对比；
- 重命名不会同步修改复制事件中的旧标题（标题不在事件里，通常无影响）。

## 后续

- 分支对比视图与折叠/展开；
- 软删除 / 恢复；
- 批量清理与保留策略；
- 基于 lineage 的分支评测统计。
