# Codex 对话记录导入 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 把本机 OpenAI Codex 的 rollout 对话记录导入 ai-chat-memory：用户消息/最终回答/过程说明进 SQLite 可搜索；思考、工具调用、文件 diff、子代理事件、计划进按会话的 JSONL 工作文件；子代理会话在列表中挂到父会话下可展开；新增"项目"列；可选自动监听增量导入。

**架构：** 新增 Rust 模块 `codex.rs` 流式解析 rollout（按 thread_id 合并分页窗口文件、按 ordinal 去重、跳过子代理继承前缀），产出 `NormalizedSession` + `Vec<WorkItem>`；工作项写入 `<数据目录>/codex-work/<thread_id>.jsonl`；`sessions` 表加 `project`/`parent_platform_session_id`/`agent_label` 三列并新增 `codex_import_state` 增量表；`AppService::import_codex` 做扫描/增量/落库/删除同步；新增三个 Tauri 命令；前端 `SessionList` 加项目列与子会话折叠、新组件 `CodexWorkPanel.vue` 替代思考栏、`SettingsDialog` 加导入分区。

**技术栈：** Rust（sqlx SQLite、tokio spawn_blocking、serde_json、chrono、uuid、新增 `notify` 8 + `walkdir` 2 依赖）、Vue 3 + TypeScript、vitest + happy-dom。

---

## 数据源事实（已在本机 + openai/codex main 源码核实）

- Codex 数据根默认 `%USERPROFILE%\.codex`。rollout 在 `sessions/YYYY/MM/DD/rollout-<ts>-<threadid>[_<window>].jsonl`，归档在 `archived_sessions/*.jsonl`。
- 每行 `{timestamp, ordinal?, type, payload}`，首行 `type="session_meta"`。`type` 取值：`session_meta` / `event_msg` / `response_item` / 及 `turn_context`、`world_state`、`token_usage_record` 等（后者忽略）。
- **界面可见内容以 `event_msg` 且 `payload.type="item_completed"` 的 `payload.item` 为准。** `item.type`：`UserMessage`、`AgentMessage`（有 `phase`：`final_answer`/`commentary`）、`Reasoning`（`summary_text[]`、`raw_content[]`）、`McpToolCall`、`CommandExecution`、`FunctionCallOutput`、`FileChange`（`changes{path:{type:add/update/delete, unified_diff?, content?}}`）、`SubAgentActivity`（`kind`、`agent_thread_id`、`agent_path`）、`Plan`（`text`）、`ContextCompaction`（忽略）。
- **分页多文件线程**：同一 `session_meta.id` 可能有多个窗口文件，靠 `history_base={thread_id,end_ordinal_exclusive,end_byte_offset}` 串联；窗口间 ordinal 有重叠 → 按 thread_id 合并、按 `ordinal` 去重（保留首个）。`ordinal` 从 1 递增（分页线程），旧格式（`history_mode!="paginated"`）无 ordinal。
- **子代理会话**：`session_meta` 有 `parent_thread_id` 或 `source.subagent` 或 `thread_source="subagent"`；`agent_nickname`/`agent_role` 在 `source.subagent.thread_spawn` 或顶层。含字段 `subagent_history_start_ordinal`：**ordinal 小于它的记录是从父会话复制来的继承前缀**（上游 `thread_history_materialization.rs:222` 对这些行用空 changeset，即不投影），必须跳过，只保留该 ordinal 及以后的记录。旧子代理无此字段时全部保留。
- 兜底：无任何 `item_completed` 的 rollout（旧版本）用 `response_item`：`message` role=user（过滤注入片段）/assistant（按 phase）；`reasoning`（`summary[].text` + `content`/`encrypted_content`）；`function_call`+`function_call_output` / `custom_tool_call`+`custom_tool_call_output` 按 `call_id` 配对。`role="developer"` 丢弃。同一 rollout 只要有一条 item_completed 就只走 item_completed 路径。
- 元数据兜底：`state_5.sqlite` 表 `threads`（列含 `id, rollout_path, title, cwd, archived, agent_nickname, agent_role, model, created_at_ms, updated_at_ms, first_user_message, thread_source`）只读打开；失败降级 `session_index.jsonl`（`{id, thread_name, updated_at}`）；再降级首条用户消息截断 60 字符。

## 文件结构

创建：
- `app/src-tauri/src/codex.rs` — rollout 解析、注入过滤、轮次/work_seq、工作 JSONL 读写、`ThreadMetaLookup`、`WorkItem`/`CodexThread` 类型。职责：把磁盘 rollout 变成 `(NormalizedSession, Vec<WorkItem>, parent_id?)`。
- `app/src-tauri/src/codex/tests.rs` — 单元测试与 fixture（`#[path]` 挂到 codex.rs）。
- `app/src/components/CodexWorkPanel.vue` — Codex 消息的"工作过程"下拉栏组件。
- `app/src/components/CodexWorkPanel.test.ts` — 组件测试。

修改：
- `app/src-tauri/src/database/connection.rs` — 加三列 + `codex_import_state` 表 + `ensure_codex_columns`，`SCHEMA_VERSION` 1→2。
- `app/src-tauri/src/models.rs` — `SessionSummary` 加 `project`/`child_count`；`NormalizedSession` 加 `project`/`parent_platform_session_id`/`agent_label`；`ImportResponse` 加 `updated`/`failed`；新增 `CodexImportResponse`/`WorkItem`(serde)。
- `app/src-tauri/src/database/sessions.rs` — `summary_from_row` 读新列、`SELECT` 加列、列表排除库内有父的子会话。
- `app/src-tauri/src/database/imports.rs` — `INSERT sessions` 写三列。
- `app/src-tauri/src/database/details.rs`、`app/src-tauri/src/semantic/index.rs` — `SELECT` 补三列。
- `app/src-tauri/src/database/maintenance.rs` — `delete_session` 返回 `platform_session_id` 供删 JSONL。
- `app/src-tauri/src/database/mod.rs` — 新增 `list_child_sessions`、`session_platform_key`，测试 schema 补列。
- `app/src-tauri/src/normalizer.rs` — `NormalizedSession` 新字段默认值补齐。
- `app/src-tauri/src/import_history.rs` / `sync/store.rs` / `sync/engine/tests.rs` / `service/tests/switch.rs` — `NormalizedSession` 字段补齐。
- `app/src-tauri/src/service.rs` — `import_codex`、`get_codex_work`、`list_child_sessions`、`codex_work_dir()`、删除时删 JSONL、`import` 触发索引后写 project、自动监听 watcher。
- `app/src-tauri/src/commands.rs` — 三个新命令 + `validate_dir_path`。
- `app/src-tauri/src/lib.rs` — 注册命令、启动增量导入。
- `app/src-tauri/src/settings.rs` / `models.rs` — `AppSettings` 加 `codex` 配置块。
- `app/src-tauri/Cargo.toml` — 加 `notify`、`walkdir`。
- `app/src/desktop-api.ts` — 三个新方法 + 类型（`project`/`child_count`/`WorkItem`/`CodexImportResponse`/settings 的 codex 块）。
- `app/src/conversation.ts` — `SessionSummary` 加 `project`/`child_count`；`WorkItem` 类型。
- `app/src/components/SessionList.vue` — 项目列、折叠箭头、子会话行。
- `app/src/App.vue` — 子会话展开状态、选中子会话展开父、`platformName` 加 codex、`MessageBlock` 传 codex 工作项/搜索联动。
- `app/src/MessageBlock.vue` — Codex 消息渲染 `CodexWorkPanel` 替代思考栏。
- `app/src/components/SettingsDialog.vue` — 导入分区。
- `app/src/composables/useSettings.ts`、`useSessionCatalog.ts` — 导入调用、子会话缓存。
- `app/src/i18n/locales/{zh-CN,en-US}.ts` — 新文案。
- `app/src/style.css` — 项目列网格、子会话缩进、工作栏样式。

---

## 任务 1：新增 Cargo 依赖

**文件：**
- 修改：`app/src-tauri/Cargo.toml`

- [ ] **步骤 1：加依赖**

在 `[dependencies]` 段（`walkdir` 已缓存 2.5.0，`notify` 取 8.x）加入：

```toml
notify = "8"
walkdir = "2"
```

- [ ] **步骤 2：验证可解析依赖树**

运行：`cd app/src-tauri; cargo tree -p notify -p walkdir --no-default-features`
预期：打印 notify 8.x 与 walkdir 2.x，无 error。

- [ ] **步骤 3：Commit**

```bash
git add app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock
git commit -m "build(codex): 新增 notify 与 walkdir 依赖"
```

## 任务 2：数据库 schema 迁移

**文件：**
- 修改：`app/src-tauri/src/database/connection.rs`

- [ ] **步骤 1：编写失败的测试**

在 `connection.rs` 底部测试模块追加（与既有 `#[sqlx::test]` 风格一致）：

```rust
#[sqlx::test]
async fn ensure_codex_columns_is_idempotent(pool: SqlitePool) {
    // 初次建库后应含新列
    initialize_schema(&pool).await.unwrap();
    let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(&pool).await.unwrap();
    for c in ["project", "parent_platform_session_id", "agent_label"] {
        assert!(cols.contains(&c.to_string()), "缺少列 {c}");
    }
    // codex_import_state 表存在
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='codex_import_state'",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // 二次调用不报错（幂等）
    ensure_codex_columns(&pool).await.unwrap();
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --no-default-features ensure_codex_columns_is_idempotent`
预期：编译失败（`ensure_codex_columns` 未定义）。

- [ ] **步骤 3：新增 codex_import_state 表**

在 `initialize_schema`（connection.rs）末尾 `ensure_embedding_vec_table(pool, None).await?;`（约第 229 行）**之前**插入建表与 ensure 调用：

```rust
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS codex_import_state (
            rollout_path TEXT PRIMARY KEY,
            thread_id TEXT NOT NULL,
            byte_len INTEGER NOT NULL,
            mtime_ms INTEGER NOT NULL,
            imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .execute(pool)
    .await?;
    ensure_codex_columns(pool).await?;
```

- [ ] **步骤 4：实现 ensure_codex_columns**

在 `ensure_sync_published_bundle_columns`（约第 368 行）之后新增函数（复用既有 `pragma_table_info` + `ALTER TABLE ADD COLUMN` 幂等模式）：

```rust
async fn ensure_codex_columns(pool: &SqlitePool) -> Result<()> {
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
            .fetch_all(pool)
            .await?;
    for (name, definition) in [
        ("project", "TEXT"),
        ("parent_platform_session_id", "TEXT"),
        ("agent_label", "TEXT"),
    ] {
        if !existing.iter().any(|column| column == name) {
            sqlx::query(&format!(
                "ALTER TABLE sessions ADD COLUMN {name} {definition}"
            ))
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
```

- [ ] **步骤 5：Bump SCHEMA_VERSION**

将 `connection.rs:87` 的 `const SCHEMA_VERSION: i32 = 1;` 改为 `= 2;`。否则既有安装因 user_version 快速路径永不执行迁移。

- [ ] **步骤 6：更新测试 fixture**

`connection.rs` 测试 fixture 里手写 `CREATE TABLE sessions(...)` 处（约第 1168 行）在 `imported_at TEXT` 之后补 `, project TEXT, parent_platform_session_id TEXT, agent_label TEXT`，使 fixture 与生产 schema 对齐。

- [ ] **步骤 7：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features ensure_codex_columns_is_idempotent`
预期：PASS。

- [ ] **步骤 8：Commit**

```bash
git add app/src-tauri/src/database/connection.rs
git commit -m "feat(codex): sessions 表新增 project 等三列与 codex_import_state 表"
```

---

## 任务 3：models 类型扩展

**文件：**
- 修改：`app/src-tauri/src/models.rs`

- [ ] **步骤 1：编写失败的测试**

在 `models.rs` 测试模块追加（验证 WorkItem 序列化字段名与 CodexSettings 默认值）：

```rust
#[test]
fn work_item_serializes_snake_case() {
    let item = WorkItem {
        work_seq: 3,
        seq: 1,
        kind: "command".into(),
        title: "ls".into(),
        body: Some("total 0".into()),
        expandable: true,
        truncated: false,
        agent_thread_id: None,
        agent_label: None,
    };
    let json = serde_json::to_value(&item).unwrap();
    assert_eq!(json["work_seq"], 3);
    assert_eq!(json["kind"], "command");
    assert_eq!(json["expandable"], true);
}

#[test]
fn codex_settings_default_is_disabled() {
    let s = CodexSettings::default();
    assert!(!s.auto_watch);
    assert!(s.codex_home.is_none());
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --no-default-features work_item_serializes_snake_case`
预期：编译失败（`WorkItem`/`CodexSettings` 未定义）。

- [ ] **步骤 3：给 SessionSummary 加字段**

`SessionSummary`（约 48-56 行）在 `imported_at` 后加：

```rust
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub child_count: i64,
```

- [ ] **步骤 4：给 NormalizedSession 加字段**

`NormalizedSession`（约 117-127 行）在 `raw_data` 后加（注意该结构体**无 Default 派生**，后续任务需在所有构造点补齐）：

```rust
    pub project: Option<String>,
    pub parent_platform_session_id: Option<String>,
    pub agent_label: Option<String>,
```

- [ ] **步骤 5：新增 WorkItem / CodexImportResponse / CodexSettings**

在 `models.rs` 合适位置（ImportResponse 附近）新增：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub work_seq: i64,
    pub seq: i64,
    /// commentary | reasoning | command | mcp_tool | file_change | subagent | plan | output
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub expandable: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexImportResponse {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodexSettings {
    #[serde(default)]
    pub auto_watch: bool,
    #[serde(default)]
    pub codex_home: Option<String>,
}

impl Default for CodexSettings {
    fn default() -> Self {
        Self { auto_watch: false, codex_home: None }
    }
}
```

- [ ] **步骤 6：AppSettings 挂 codex 块**

`AppSettings`（约 357-385 行）在 `custom_themes` 前加：

```rust
    #[serde(default)]
    pub codex: CodexSettings,
```

（serde-default，不破坏既有 `..Default::default()` 与反序列化。）

- [ ] **步骤 7：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features work_item_serializes_snake_case codex_settings_default_is_disabled`
预期：两个测试 PASS（此时其他文件因 NormalizedSession 缺字段可能编译失败——由任务 5 统一补齐；本步只需这两个单测所在 crate 能编译则先跳到任务 5，再回来验证。若整包无法编译，先完成任务 4/5 再统一 `cargo test`）。

> **执行提示：** 任务 3-5 存在跨文件编译耦合（NormalizedSession 新字段）。按 3→4→5 顺序完成后再统一编译验证；任务 3/4 的单测断言在任务 5 补齐构造点后一起转绿。

- [ ] **步骤 8：Commit**

```bash
git add app/src-tauri/src/models.rs
git commit -m "feat(codex): 扩展 SessionSummary/NormalizedSession 与新增 WorkItem 等类型"
```

## 任务 4：数据库读写补齐新列 + 子会话查询

**文件：**
- 修改：`app/src-tauri/src/database/sessions.rs`、`imports.rs`、`details.rs`、`maintenance.rs`、`mod.rs`、`app/src-tauri/src/semantic/index.rs`

- [ ] **步骤 1：编写失败的测试（sessions.rs 测试模块）**

```rust
#[sqlx::test]
async fn list_excludes_child_sessions_with_present_parent(pool: SqlitePool) {
    crate::database::connection::initialize_schema(&pool).await.unwrap();
    // 父会话
    sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, project) VALUES ('p','codex','T-parent','父','proj')")
        .execute(&pool).await.unwrap();
    // 子会话（parent 指向已入库的父）
    sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, parent_platform_session_id, agent_label) VALUES ('c','codex','T-child','子','T-parent','审阅者')")
        .execute(&pool).await.unwrap();
    let top = search(&pool, &SearchQuery::default()).await.unwrap();
    assert!(top.iter().any(|s| s.platform_session_id == "T-parent"));
    assert!(!top.iter().any(|s| s.platform_session_id == "T-child"), "子会话不应出现在顶层列表");
    let parent = top.iter().find(|s| s.platform_session_id == "T-parent").unwrap();
    assert_eq!(parent.child_count, 1);
    assert_eq!(parent.project.as_deref(), Some("proj"));

    let children = crate::database::list_child_sessions(&pool, "T-parent").await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].platform_session_id, "T-child");
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --no-default-features list_excludes_child_sessions_with_present_parent`
预期：编译失败（`list_child_sessions` 未定义、`child_count`/`project` 字段缺失）。

- [ ] **步骤 3：summary_from_row 读新列**

`sessions.rs` `summary_from_row`（约 160-170 行）改为：

```rust
pub(crate) fn summary_from_row(row: sqlx::sqlite::SqliteRow) -> SessionSummary {
    SessionSummary {
        id: row.get("id"),
        platform: row.get("platform"),
        platform_session_id: row.get("platform_session_id"),
        title: row.get::<Option<String>, _>("title").unwrap_or_default(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        imported_at: row.get("imported_at"),
        project: row.try_get("project").unwrap_or(None),
        child_count: row.try_get("child_count").unwrap_or(0),
    }
}
```

> `try_get(...).unwrap_or(...)` 使不含这些列的 SELECT（如 branch/detail 里未 JOIN 子计数的查询）仍可复用 `summary_from_row`。

- [ ] **步骤 4：列表 SELECT 增列 + 子会话排除 + 子计数**

`search_fts`（约 43 行）与 `search_like`（约 75 行）的 SELECT 列表统一改为下面的列，并在 WHERE 增加"排除库内有父的子会话"，`FROM sessions s` 用 LEFT JOIN 计子数。以 `search_like` 为例：

```rust
    let sql = format!(
        "SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at, s.project,
                (SELECT COUNT(*) FROM sessions c WHERE c.parent_platform_session_id = s.platform_session_id AND c.platform = s.platform) AS child_count
         FROM sessions s
         WHERE (? IS NULL OR s.platform = ?)
           AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL))
           AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL))
           AND (? IS NULL OR s.title LIKE '%' || ? || '%' ESCAPE '\\' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%' ESCAPE '\\'))
           AND (s.parent_platform_session_id IS NULL
                OR NOT EXISTS (SELECT 1 FROM sessions p WHERE p.platform = s.platform AND p.platform_session_id = s.parent_platform_session_id))
         ORDER BY ({{timestamp}}) DESC, s.id ASC LIMIT ? OFFSET ?"
    );
```

`search_fts` 同理：SELECT 列表补 `s.project` 与相同的 `child_count` 子查询，WHERE 末尾追加相同的"排除有父子会话"子句。`count_fts`/`count_like` 的 WHERE 也追加同一排除子句，保持列表与总数一致。

- [ ] **步骤 5：新增 list_child_sessions**

`sessions.rs` 新增（供展开时懒加载）：

```rust
pub async fn list_child_sessions(
    pool: &SqlitePool,
    parent_platform_session_id: &str,
) -> Result<Vec<SessionSummary>> {
    let ts = timestamp::expression("s.updated_at");
    let sql = format!(
        "SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at, s.project,
                0 AS child_count
         FROM sessions s
         WHERE s.parent_platform_session_id = ?
         ORDER BY ({ts}) ASC, s.id ASC"
    );
    let rows = sqlx::query(&sql)
        .bind(parent_platform_session_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(summary_from_row).collect())
}
```

- [ ] **步骤 6：导出 list_child_sessions + session_platform_key（mod.rs）**

`database/mod.rs` 在 `pub use sessions::{...}` 处加入 `list_child_sessions`。并新增按 id 取 `(platform, platform_session_id)` 的辅助（供删除时定位 JSONL）：

```rust
pub async fn session_platform_key(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> crate::error::Result<Option<(String, String)>> {
    let row = sqlx::query("SELECT platform, platform_session_id FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| {
        use sqlx::Row;
        (r.get::<String, _>("platform"), r.get::<String, _>("platform_session_id"))
    }))
}
```

- [ ] **步骤 7：codex INSERT 写三列（imports.rs）**

`import_one_session`（imports.rs 约 62 行）的 upsert 改为写 project/parent/agent_label（普通平台这三值为 None）：

```rust
    sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data, project, parent_platform_session_id, agent_label) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(platform, platform_session_id) DO UPDATE SET title=excluded.title, created_at=excluded.created_at, updated_at=excluded.updated_at, imported_at=excluded.imported_at, raw_data=excluded.raw_data, project=excluded.project, parent_platform_session_id=excluded.parent_platform_session_id, agent_label=excluded.agent_label")
        .bind(&id).bind(&session.platform).bind(&session.platform_session_id).bind(&session.title)
        .bind(&session.created_at).bind(&session.updated_at).bind(&session.imported_at).bind(serde_json::to_string(&session.raw_data)?)
        .bind(&session.project).bind(&session.parent_platform_session_id).bind(&session.agent_label)
        .execute(&mut *tx).await?;
```

- [ ] **步骤 8：details.rs 与 semantic/index.rs 的 SELECT 补 project**

`details.rs` `open_session`（约 20 行）的 SELECT 在 `imported_at` 后加 `, project`（`child_count` 详情页不需要，`summary_from_row` 的 `try_get(...).unwrap_or(0)` 会补 0）。

`semantic/index.rs` 批量 summary SELECT（约 599 行）改为：

```rust
            "SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at, project FROM sessions WHERE id IN (",
```

- [ ] **步骤 9：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features list_excludes_child_sessions_with_present_parent`
预期：PASS（依赖任务 5 补齐 NormalizedSession 构造点后整包可编译；若此时未做任务 5，先做 5 再回本步）。

- [ ] **步骤 10：Commit**

```bash
git add app/src-tauri/src/database app/src-tauri/src/semantic/index.rs
git commit -m "feat(codex): 数据库读写补齐 project 等列并支持子会话查询"
```

---

## 任务 5：补齐 NormalizedSession 构造点

**文件：**
- 修改：`app/src-tauri/src/normalizer.rs`、`import_history.rs`、`sync/store.rs`、`sync/engine/tests.rs`、`service/tests/switch.rs`、`database/mod.rs`

- [ ] **步骤 1：normalizer.rs 两处构造点**

`normalize_session`（约 78 行）与 `normalize_deepseek_export`（约 368 行）的 `NormalizedSession { ... }` 在 `raw_data` 后各加：

```rust
        project: None,
        parent_platform_session_id: None,
        agent_label: None,
```

- [ ] **步骤 2：import_history.rs 构造点**

对 import_history.rs 中每个 `NormalizedSession { ... }` 字面量（约 273、461、592、757 行）在 `raw_data` 字段后补相同三行 `project: None, parent_platform_session_id: None, agent_label: None,`。

- [ ] **步骤 3：sync/store.rs 两处**

`sync/store.rs`（约 239、329 行，从远端快照重建会话处）补相同三行。远端快照当前不含这些字段，统一置 `None`（Codex 会话不走云同步重建路径）。

- [ ] **步骤 4：测试构造点**

`sync/engine/tests.rs`（约 906、1535、1815、3622、3720、3829、3979 行）、`service/tests/switch.rs`（约 175 行）、`database/mod.rs` 的 `session_fixture`（约 61 行）每个 `NormalizedSession { ... }` 补相同三行。

> **提示：** 用 `cargo build --no-default-features` 的报错逐个定位缺字段的行，比手工找更可靠——编译器会精确列出每个缺 `project` 的构造点。

- [ ] **步骤 5：整包编译 + 跑任务 3/4 单测**

运行：`cd app/src-tauri; cargo test --no-default-features work_item_serializes_snake_case codex_settings_default_is_disabled list_excludes_child_sessions_with_present_parent ensure_codex_columns_is_idempotent`
预期：全部 PASS。

- [ ] **步骤 6：Commit**

```bash
git add app/src-tauri/src
git commit -m "refactor(codex): 补齐 NormalizedSession 新字段的所有构造点"
```

## 任务 6：codex.rs 解析器骨架与类型

**文件：**
- 创建：`app/src-tauri/src/codex.rs`、`app/src-tauri/src/codex/tests.rs`
- 修改：`app/src-tauri/src/lib.rs`（`mod codex;`）

- [ ] **步骤 1：声明模块**

在 `lib.rs` 现有 `mod` 声明区（如 `mod import_history;` 附近）加 `mod codex;`。

- [ ] **步骤 2：codex.rs 类型与常量骨架**

创建 `app/src-tauri/src/codex.rs`：

```rust
//! OpenAI Codex rollout 解析：把磁盘 rollout JSONL 转成可入库的
//! NormalizedSession（用户消息 + 最终回答 + 过程说明），其余过程性内容
//! （思考、工具调用、diff、子代理、计划）产出 WorkItem 写入按会话的
//! 工作 JSONL。分页窗口按 thread_id 合并、按 ordinal 去重，子代理继承前缀
//! （ordinal < subagent_history_start_ordinal）跳过。
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::models::{NormalizedMessage, NormalizedSession, WorkItem};

/// 单条工作项正文的截断上限（UTF-8 字符边界安全）。
pub(crate) const WORK_ITEM_MAX_BYTES: usize = 130 * 1024;

/// 解析一个线程（可能由多个窗口文件合并）后的产物。
#[derive(Debug, Clone)]
pub struct CodexThread {
    pub session: NormalizedSession,
    pub work_items: Vec<WorkItem>,
    pub parent_platform_session_id: Option<String>,
}

/// rollout 单行。
#[derive(Debug, Clone)]
struct RolloutLine {
    ordinal: Option<i64>,
    kind: String,
    timestamp: Option<String>,
    payload: Value,
}

/// 按 UTF-8 字符边界把正文截断到 WORK_ITEM_MAX_BYTES，返回 (截断后文本, 是否截断)。
pub(crate) fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;
```

- [ ] **步骤 3：编写截断测试**

创建 `app/src-tauri/src/codex/tests.rs`：

```rust
use super::*;

#[test]
fn truncate_respects_char_boundary() {
    // 每个中文字符占 3 字节，取上限 4 字节应回退到 3 字节边界（1 个字符）。
    let s = "汉字漢字";
    let (out, truncated) = truncate_on_char_boundary(s, 4);
    assert!(truncated);
    assert_eq!(out, "汉");
    assert!(out.len() <= 4);
}

#[test]
fn truncate_keeps_short_text() {
    let (out, truncated) = truncate_on_char_boundary("hello", 130 * 1024);
    assert!(!truncated);
    assert_eq!(out, "hello");
}
```

- [ ] **步骤 4：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features codex::tests::truncate`
预期：两个测试 PASS。

- [ ] **步骤 5：Commit**

```bash
git add app/src-tauri/src/codex.rs app/src-tauri/src/codex/tests.rs app/src-tauri/src/lib.rs
git commit -m "feat(codex): 新增 codex 解析模块骨架与截断工具"
```

---

## 任务 7：rollout 行解析与 item_completed 分流

**文件：**
- 修改：`app/src-tauri/src/codex.rs`、`app/src-tauri/src/codex/tests.rs`

- [ ] **步骤 1：编写失败的测试**

在 `codex/tests.rs` 追加（用最小 rollout 文本走完整解析）：

```rust
fn line(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

#[test]
fn parses_user_message_and_final_answer_into_session() {
    let lines = vec![
        line(r#"{"type":"session_meta","payload":{"id":"T-1","cwd":"/home/u/my-proj","timestamp":"2026-09-01T10:00:00Z"}}"#),
        line(r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"你好"}}}"#),
        line(r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"你好，我能帮忙"}}}"#),
        line(r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"commentary","text":"我先看看文件"}}}"#),
    ];
    let thread = parse_lines("T-1", &lines, None).unwrap();
    assert_eq!(thread.session.platform, "codex");
    assert_eq!(thread.session.platform_session_id, "T-1");
    assert_eq!(thread.session.project.as_deref(), Some("my-proj"));
    // 用户消息 + 最终回答 + commentary 都进 DB messages
    let roles: Vec<&str> = thread.session.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "assistant"]);
    // commentary 也生成一个 work item（供工作栏时间线展示）
    assert!(thread.work_items.iter().any(|w| w.kind == "commentary"));
}

#[test]
fn reasoning_and_command_go_to_work_items_only() {
    let lines = vec![
        line(r#"{"type":"session_meta","payload":{"id":"T-2","cwd":"/x/proj"}}"#),
        line(r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"跑测试"}}}"#),
        line(r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","summary_text":["计划"],"raw_content":["细节明文"]}}}"#),
        line(r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":"cargo test","aggregated_output":"ok"}}}"#),
    ];
    let thread = parse_lines("T-2", &lines, None).unwrap();
    // 只有用户消息进 DB
    assert_eq!(thread.session.messages.len(), 1);
    assert!(thread.work_items.iter().any(|w| w.kind == "reasoning"));
    assert!(thread.work_items.iter().any(|w| w.kind == "command" && w.title.contains("cargo test")));
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --no-default-features codex::tests::parses_user_message`
预期：编译失败（`parse_lines` 未定义）。

- [ ] **步骤 3：实现行读取与元数据**

在 `codex.rs` 追加：

```rust
fn line_field<'a>(line: &'a Value, key: &str) -> Option<&'a Value> {
    line.get(key)
}

fn as_str(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// 取 cwd 的末段作为 project。
fn project_from_cwd(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?.trim_end_matches(['/', '\\']);
    let seg = cwd.rsplit(['/', '\\']).next()?;
    (!seg.is_empty()).then(|| seg.to_string())
}
```

- [ ] **步骤 4：实现 parse_lines 主流程**

在 `codex.rs` 追加 `parse_lines`（`parent_hint` 为来自 session_meta 的父线程 id）：

```rust
pub fn parse_lines(
    thread_id: &str,
    raw_lines: &[Value],
    parent_hint: Option<&str>,
) -> crate::error::Result<CodexThread> {
    // 1. session_meta（首行或首个该类型行）
    let meta = raw_lines
        .iter()
        .find(|l| line_field(l, "type").and_then(Value::as_str) == Some("session_meta"))
        .and_then(|l| l.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);
    let cwd = as_str(meta.get("cwd"));
    let project = project_from_cwd(cwd.as_deref());
    let created_at = crate::normalizer::normalize_timestamp(meta.get("timestamp"));
    let parent_platform_session_id = parent_hint
        .map(str::to_owned)
        .or_else(|| as_str(meta.get("parent_thread_id")));
    let agent_label = as_str(meta.get("agent_nickname"))
        .or_else(|| as_str(meta.get("agent_role")));
    let subagent_start = meta
        .get("subagent_history_start_ordinal")
        .and_then(Value::as_i64);

    // 2. 归一化并按 ordinal 去重（保留首个），跳过子代理继承前缀
    let mut seen_ordinals = std::collections::HashSet::new();
    let mut lines: Vec<RolloutLine> = Vec::new();
    for raw in raw_lines {
        let Some(kind) = as_str(line_field(raw, "type")) else { continue };
        if kind == "session_meta" {
            continue;
        }
        let ordinal = raw.get("ordinal").and_then(Value::as_i64);
        if let (Some(start), Some(ord)) = (subagent_start, ordinal)
            && ord < start
        {
            continue; // 继承自父会话的前缀，跳过
        }
        if let Some(ord) = ordinal
            && !seen_ordinals.insert(ord)
        {
            continue; // 分页窗口重叠，保留首个
        }
        lines.push(RolloutLine {
            ordinal,
            kind,
            timestamp: as_str(raw.get("timestamp")),
            payload: raw.get("payload").cloned().unwrap_or(Value::Null),
        });
    }

    // 3. 判定是否存在 item_completed
    let has_item_completed = lines.iter().any(|l| {
        l.kind == "event_msg"
            && l.payload.get("type").and_then(Value::as_str) == Some("item_completed")
    });

    let mut builder = ThreadBuilder::new();
    if has_item_completed {
        for l in &lines {
            if l.kind == "event_msg"
                && l.payload.get("type").and_then(Value::as_str) == Some("item_completed")
                && let Some(item) = l.payload.get("item")
            {
                builder.push_item(item, l.timestamp.as_deref());
            }
        }
    } else {
        builder.push_response_items(&lines);
    }

    let title = builder.derive_title();
    let updated_at = builder.last_timestamp.clone().or_else(|| created_at.clone());
    let session = NormalizedSession {
        id: uuid::Uuid::new_v4().to_string(),
        platform: "codex".into(),
        platform_session_id: thread_id.to_string(),
        title,
        created_at,
        updated_at,
        imported_at: chrono::Utc::now().to_rfc3339(),
        messages: builder.messages,
        raw_data: serde_json::json!({ "source": "codex", "thread_id": thread_id }),
        project,
        parent_platform_session_id: parent_platform_session_id.clone(),
        agent_label,
    };
    Ok(CodexThread {
        session,
        work_items: builder.work_items,
        parent_platform_session_id,
    })
}
```

- [ ] **步骤 5：实现 ThreadBuilder**

在 `codex.rs` 追加。`ThreadBuilder` 累积 DB 消息与工作项，并维护共享递增序号 `work_seq`（工作项）与 `seq`（用于工作栏与消息时间线对齐——这里用 rollout 内单调计数）：

```rust
#[derive(Default)]
struct ThreadBuilder {
    messages: Vec<NormalizedMessage>,
    work_items: Vec<WorkItem>,
    seq: i64,
    work_seq: i64,
    first_user_text: Option<String>,
    last_timestamp: Option<String>,
}

impl ThreadBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn push_work(&mut self, seq: i64, kind: &str, title: String, body: Option<String>, expandable: bool) {
        let (body, truncated) = match body {
            Some(text) => {
                let (t, tr) = truncate_on_char_boundary(&text, WORK_ITEM_MAX_BYTES);
                (Some(t), tr)
            }
            None => (None, false),
        };
        self.work_items.push(WorkItem {
            work_seq: self.work_seq,
            seq,
            kind: kind.to_string(),
            title,
            body,
            expandable,
            truncated,
            agent_thread_id: None,
            agent_label: None,
        });
        self.work_seq += 1;
    }

    fn push_message(&mut self, role: &str, content: String, ts: Option<&str>) {
        self.messages.push(NormalizedMessage {
            role: role.to_string(),
            content,
            metadata: serde_json::json!({ "source": "codex" }),
            created_at: ts.map(str::to_owned),
        });
    }

    fn derive_title(&self) -> String {
        self.first_user_text
            .as_deref()
            .map(|t| t.chars().take(60).collect::<String>())
            .unwrap_or_default()
    }

    /// item_completed 路径：按 item.type 分流。
    fn push_item(&mut self, item: &Value, ts: Option<&str>) {
        if let Some(t) = ts {
            self.last_timestamp = Some(t.to_string());
        }
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let seq = self.next_seq();
        match item_type {
            "UserMessage" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                if is_injected_user_text(text) {
                    return;
                }
                if self.first_user_text.is_none() && !text.is_empty() {
                    self.first_user_text = Some(text.to_string());
                }
                self.push_message("user", text.to_string(), ts);
            }
            "AgentMessage" => {
                let phase = item.get("phase").and_then(Value::as_str).unwrap_or("final_answer");
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                match phase {
                    "commentary" => {
                        // 过程说明：既进 DB（可搜索）又进工作栏时间线
                        self.push_message("assistant", text.clone(), ts);
                        self.push_work(seq, "commentary", "过程说明".into(), Some(text), true);
                    }
                    _ => self.push_message("assistant", text, ts),
                }
            }
            "Reasoning" => {
                let summary = item
                    .get("summary_text")
                    .and_then(Value::as_array)
                    .map(|a| join_strings(a))
                    .unwrap_or_default();
                let raw = item
                    .get("raw_content")
                    .and_then(Value::as_array)
                    .map(|a| join_strings(a));
                let encrypted = raw.as_deref().map(str::is_empty).unwrap_or(true)
                    || item.get("encrypted_content").is_some();
                let title = if summary.is_empty() {
                    "思考".to_string()
                } else {
                    format!("思考 · {}", summary.lines().next().unwrap_or("").trim())
                };
                // 明文可展开；加密只留一行不可展开
                self.push_work(seq, "reasoning", title, raw.filter(|s| !s.is_empty()), !encrypted);
            }
            "CommandExecution" => {
                let cmd = item.get("command").and_then(Value::as_str).unwrap_or_default();
                let out = item.get("aggregated_output").and_then(Value::as_str).map(str::to_owned);
                self.push_work(seq, "command", format!("$ {cmd}"), out, true);
            }
            "McpToolCall" => {
                let name = item.get("tool").or_else(|| item.get("name")).and_then(Value::as_str).unwrap_or("tool");
                let body = item.get("result").map(compact_json).or_else(|| item.get("arguments").map(compact_json));
                self.push_work(seq, "mcp_tool", format!("MCP · {name}"), body, true);
            }
            "FunctionCallOutput" => {
                let body = item.get("output").and_then(Value::as_str).map(str::to_owned)
                    .or_else(|| Some(compact_json(item)));
                self.push_work(seq, "output", "工具输出".into(), body, true);
            }
            "FileChange" => {
                let body = render_file_change(item);
                self.push_work(seq, "file_change", "文件改动".into(), Some(body), true);
            }
            "SubAgentActivity" => {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("update");
                let agent_thread_id = item.get("agent_thread_id").and_then(Value::as_str).map(str::to_owned);
                let label = item.get("agent_nickname").and_then(Value::as_str).unwrap_or("子代理");
                let verb = if kind.eq_ignore_ascii_case("start") { "已开始工作" } else { "已更新" };
                self.work_items.push(WorkItem {
                    work_seq: self.work_seq,
                    seq,
                    kind: "subagent".into(),
                    title: format!("子代理 {label} {verb}"),
                    body: None,
                    expandable: false,
                    truncated: false,
                    agent_thread_id,
                    agent_label: Some(label.to_string()),
                });
                self.work_seq += 1;
            }
            "Plan" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                self.push_work(seq, "plan", "计划".into(), Some(text), true);
            }
            // ContextCompaction 等忽略
            _ => {}
        }
    }
}

fn join_strings(arr: &[Value]) -> String {
    arr.iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn render_file_change(item: &Value) -> String {
    let Some(changes) = item.get("changes").and_then(Value::as_object) else {
        return compact_json(item);
    };
    let mut out = String::new();
    for (path, change) in changes {
        let kind = change.get("type").and_then(Value::as_str).unwrap_or("update");
        out.push_str(&format!("[{kind}] {path}\n"));
        if let Some(diff) = change.get("unified_diff").and_then(Value::as_str) {
            out.push_str(diff);
            out.push('\n');
        }
    }
    out
}
```

- [ ] **步骤 6：注入片段过滤 + response_item 兜底**

在 `codex.rs` 追加：

```rust
/// Codex 会把环境说明/工具注入以 UserMessage 形式写入，需从可见正文剔除。
fn is_injected_user_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("# Instructions")
        || t.starts_with("<INSTRUCTIONS>")
}

impl ThreadBuilder {
    /// 旧格式（无 item_completed）：走 response_item，按 call_id 配对工具调用/输出。
    fn push_response_items(&mut self, lines: &[RolloutLine]) {
        for l in lines {
            if l.kind != "response_item" {
                continue;
            }
            let p = &l.payload;
            let rtype = p.get("type").and_then(Value::as_str).unwrap_or_default();
            let seq = self.next_seq();
            match rtype {
                "message" => {
                    let role = p.get("role").and_then(Value::as_str).unwrap_or("assistant");
                    if role == "developer" {
                        continue; // 丢弃
                    }
                    let text = response_message_text(p);
                    if role == "user" {
                        if is_injected_user_text(&text) {
                            continue;
                        }
                        if self.first_user_text.is_none() && !text.is_empty() {
                            self.first_user_text = Some(text.clone());
                        }
                        self.push_message("user", text, l.timestamp.as_deref());
                    } else {
                        self.push_message("assistant", text, l.timestamp.as_deref());
                    }
                }
                "reasoning" => {
                    let summary = p.get("summary").and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|s| s.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
                        .unwrap_or_default();
                    let body = p.get("content").and_then(Value::as_str).map(str::to_owned);
                    let encrypted = p.get("encrypted_content").is_some() && body.is_none();
                    let title = if summary.is_empty() { "思考".into() } else { format!("思考 · {}", summary.lines().next().unwrap_or("").trim()) };
                    self.push_work(seq, "reasoning", title, body, !encrypted);
                }
                "function_call" | "custom_tool_call" => {
                    let name = p.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let args = p.get("arguments").or_else(|| p.get("input")).map(compact_json);
                    self.push_work(seq, "command", format!("$ {name}"), args, true);
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let out = p.get("output").and_then(Value::as_str).map(str::to_owned).or_else(|| Some(compact_json(p)));
                    self.push_work(seq, "output", "工具输出".into(), out, true);
                }
                _ => {}
            }
        }
    }
}

fn response_message_text(p: &Value) -> String {
    // content 可能是字符串或 [{type, text}]
    match p.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|it| it.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
```

- [ ] **步骤 7：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features codex::tests`
预期：`parses_user_message_and_final_answer_into_session` 与 `reasoning_and_command_go_to_work_items_only` PASS。

- [ ] **步骤 8：Commit**

```bash
git add app/src-tauri/src/codex.rs app/src-tauri/src/codex/tests.rs
git commit -m "feat(codex): 实现 rollout 行分流与 response_item 兜底解析"
```

## 任务 8：磁盘扫描、分页合并与工作 JSONL 读写

**文件：**
- 修改：`app/src-tauri/src/codex.rs`、`app/src-tauri/src/codex/tests.rs`

- [ ] **步骤 1：编写失败的测试**

在 `codex/tests.rs` 追加（用临时目录写两个窗口文件，验证按 thread_id 合并 + ordinal 去重）：

```rust
use std::io::Write;

fn write_rollout(dir: &std::path::Path, name: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    path
}

#[test]
fn merges_paginated_windows_by_thread_id() {
    let tmp = std::env::temp_dir().join(format!("codex-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // 窗口 1：ordinal 1,2
    write_rollout(&tmp, "rollout-a_0.jsonl", &[
        r#"{"type":"session_meta","payload":{"id":"T-9","cwd":"/p/proj","history_mode":"paginated"}}"#,
        r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"问题一"}}}"#,
        r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"答一"}}}"#,
    ]);
    // 窗口 2：ordinal 2（重叠，应去重）,3
    write_rollout(&tmp, "rollout-a_1.jsonl", &[
        r#"{"type":"session_meta","payload":{"id":"T-9","cwd":"/p/proj","history_mode":"paginated","history_base":{"thread_id":"T-9","end_ordinal_exclusive":2}}}"#,
        r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"答一"}}}"#,
        r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"问题二"}}}"#,
    ]);
    let threads = scan_dir(&tmp).unwrap();
    std::fs::remove_dir_all(&tmp).ok();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t.session.platform_session_id, "T-9");
    // 去重后：user 问题一 / assistant 答一 / user 问题二 —— 只有一个"答一"
    let contents: Vec<&str> = t.session.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["问题一", "答一", "问题二"]);
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --no-default-features codex::tests::merges_paginated_windows`
预期：编译失败（`scan_dir` 未定义）。

- [ ] **步骤 3：实现 thread_id 提取与文件分组**

在 `codex.rs` 追加：

```rust
/// 从 session_meta 首行提取 thread_id 与 parent 提示（不解析全文，快速分组用）。
fn read_thread_head(path: &Path) -> Option<(String, Option<String>)> {
    let content = std::fs::read_to_string(path).ok()?;
    let first = content.lines().next()?;
    let value: Value = serde_json::from_str(first).ok()?;
    if value.get("type").and_then(Value::as_str)? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    let id = payload.get("id").and_then(Value::as_str)?.to_string();
    let parent = payload
        .get("parent_thread_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("source")
                .and_then(|s| s.get("subagent"))
                .and_then(|s| s.get("parent_thread_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Some((id, parent))
}

/// 读一个 rollout 文件的所有行为 Value（跳过解析失败的行，容忍尾部截断）。
fn read_rollout_values(path: &Path) -> Vec<Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}
```

- [ ] **步骤 4：实现 scan_dir**

```rust
/// 扫描一个目录下的 rollout 文件，按 thread_id 分组合并并解析。
pub fn scan_dir(dir: &Path) -> crate::error::Result<Vec<CodexThread>> {
    use std::collections::BTreeMap;
    // thread_id -> (parent_hint, 该线程所有窗口文件路径)
    let mut groups: BTreeMap<String, (Option<String>, Vec<PathBuf>)> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
        let is_rollout = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("rollout-"))
            .unwrap_or(false);
        if !is_jsonl || !is_rollout {
            continue;
        }
        if let Some((thread_id, parent)) = read_thread_head(path) {
            let slot = groups.entry(thread_id).or_default();
            if slot.0.is_none() {
                slot.0 = parent;
            }
            slot.1.push(path.to_path_buf());
        }
    }
    let mut threads = Vec::new();
    for (thread_id, (parent, mut paths)) in groups {
        // 窗口文件名后缀 _0/_1 决定顺序；无后缀者视为单窗口
        paths.sort();
        let mut all_lines = Vec::new();
        for p in &paths {
            all_lines.extend(read_rollout_values(p));
        }
        match parse_lines(&thread_id, &all_lines, parent.as_deref()) {
            Ok(thread) => threads.push(thread),
            Err(error) => tracing::warn!(%thread_id, %error, "codex thread 解析失败，跳过"),
        }
    }
    Ok(threads)
}
```

- [ ] **步骤 5：实现工作 JSONL 写入/读取/删除**

```rust
/// 把某线程的工作项写入 `<work_dir>/<thread_id>.jsonl`（每行一个 WorkItem）。
pub fn write_work_items(work_dir: &Path, thread_id: &str, items: &[WorkItem]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(work_dir)?;
    let path = work_dir.join(sanitize_thread_id(thread_id)).with_extension("jsonl");
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        for item in items {
            let line = serde_json::to_string(item).unwrap_or_default();
            writeln!(f, "{line}")?;
        }
        f.flush()?;
    }
    std::fs::rename(&tmp, &path)?; // 原子替换，避免读到半截文件
    Ok(())
}

/// 读取某线程工作项（文件缺失返回空）。
pub fn read_work_items(work_dir: &Path, thread_id: &str) -> Vec<WorkItem> {
    let path = work_dir.join(sanitize_thread_id(thread_id)).with_extension("jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
        .collect()
}

pub fn delete_work_items(work_dir: &Path, thread_id: &str) {
    let path = work_dir.join(sanitize_thread_id(thread_id)).with_extension("jsonl");
    let _ = std::fs::remove_file(path);
}

/// thread_id 全是 UUID/短标识，但仍防御路径穿越：仅保留字母数字/连字符/下划线。
fn sanitize_thread_id(thread_id: &str) -> String {
    thread_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
```

- [ ] **步骤 6：工作 JSONL 往返测试**

在 `codex/tests.rs` 追加：

```rust
#[test]
fn work_items_roundtrip_through_jsonl() {
    let tmp = std::env::temp_dir().join(format!("codex-work-{}", uuid::Uuid::new_v4()));
    let items = vec![WorkItem {
        work_seq: 0, seq: 0, kind: "command".into(), title: "$ ls".into(),
        body: Some("out".into()), expandable: true, truncated: false,
        agent_thread_id: None, agent_label: None,
    }];
    write_work_items(&tmp, "T-x", &items).unwrap();
    let back = read_work_items(&tmp, "T-x");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].title, "$ ls");
    delete_work_items(&tmp, "T-x");
    assert!(read_work_items(&tmp, "T-x").is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}
```

- [ ] **步骤 7：运行确认通过**

运行：`cd app/src-tauri; cargo test --no-default-features codex::tests`
预期：全部 PASS。

- [ ] **步骤 8：Commit**

```bash
git add app/src-tauri/src/codex.rs app/src-tauri/src/codex/tests.rs
git commit -m "feat(codex): 实现目录扫描分页合并与工作 JSONL 读写"
```

## 任务 9：service 层 import_codex / get_codex_work / list_child_sessions

**文件：**
- 修改：`app/src-tauri/src/service.rs`、`app/src-tauri/src/database/mod.rs`、`app/src-tauri/src/database/maintenance.rs`

- [ ] **步骤 1：codex_work_dir 与默认 codex 根**

在 `service.rs` `impl AppService` 内新增（`self.data_dir` 为 `Arc<PathBuf>`）：

```rust
    /// 工作 JSONL 根目录：<数据目录>/codex-work。
    pub(crate) fn codex_work_dir(&self) -> std::path::PathBuf {
        self.data_dir.as_ref().join("codex-work")
    }

    /// 默认 Codex 数据根：设置里显式配置优先，否则 %USERPROFILE%\.codex。
    fn resolve_codex_home(settings: &AppSettings) -> Option<std::path::PathBuf> {
        if let Some(dir) = settings.codex.codex_home.as_deref().filter(|s| !s.trim().is_empty()) {
            return Some(std::path::PathBuf::from(dir));
        }
        Self::user_home_dir().map(|home| home.join(".codex"))
    }

    /// 不引新依赖，直接读环境变量定位用户主目录。
    fn user_home_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
            .map(std::path::PathBuf::from)
    }
```

> `AppError` 无 `Internal` 变体（现有变体见 `error.rs`：Database/Io/Json/Zip/Crypto/Credential/Cloud/SyncProtocol/InvalidData/NotFound/Configuration/Cancelled）。下面 `spawn_blocking` 的 JoinError 一律映射为 `AppError::Configuration(e.to_string())`。

- [ ] **步骤 2：import_codex 主流程**

在 `service.rs` `impl AppService` 内新增（参照 `import`/`import_history` 的 sync_gate + 索引 + notify 结构）：

```rust
    pub async fn import_codex(&self) -> Result<crate::models::CodexImportResponse> {
        self.ensure_writable()?;
        let settings = self.settings().await;
        let Some(codex_home) = Self::resolve_codex_home(&settings) else {
            return Err(AppError::InvalidData("无法定位 Codex 数据目录".into()));
        };
        let work_dir = self.codex_work_dir();

        // 磁盘 IO + 解析放到阻塞线程池
        let threads = tokio::task::spawn_blocking(move || -> Result<Vec<crate::codex::CodexThread>> {
            let mut all = Vec::new();
            for sub in ["sessions", "archived_sessions"] {
                let dir = codex_home.join(sub);
                if dir.is_dir() {
                    all.extend(crate::codex::scan_dir(&dir)?);
                }
            }
            Ok(all)
        })
        .await
        .map_err(|e| AppError::Configuration(e.to_string()))??;

        if threads.is_empty() {
            return Ok(crate::models::CodexImportResponse::default());
        }

        // 先落工作 JSONL（失败仅告警，不阻断入库）
        {
            let work_dir = work_dir.clone();
            let payload: Vec<(String, Vec<WorkItem>)> = threads
                .iter()
                .map(|t| (t.session.platform_session_id.clone(), t.work_items.clone()))
                .collect();
            let _ = tokio::task::spawn_blocking(move || {
                for (thread_id, items) in payload {
                    if let Err(error) = crate::codex::write_work_items(&work_dir, &thread_id, &items) {
                        tracing::warn!(%thread_id, %error, "codex 工作文件写入失败");
                    }
                }
            })
            .await;
        }

        let normalized: Vec<NormalizedSession> = threads.iter().map(|t| t.session.clone()).collect();
        let imported = {
            let _guard = self.sync_gate.lock().await;
            import_local_sessions(&self.pool, &normalized).await?
        };
        for session in &normalized {
            if let Ok(Some(id)) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
            )
            .bind(&session.platform)
            .bind(&session.platform_session_id)
            .fetch_optional(&self.pool)
            .await
            {
                let _ = self.semantic.request_session_index(&id).await;
            }
        }
        let total = normalized.len();
        tracing::info!(total, imported, "codex import completed");
        self.notify_local_sync();
        Ok(crate::models::CodexImportResponse {
            imported,
            updated: total.saturating_sub(imported),
            skipped: 0,
            failed: 0,
        })
    }
```

> `import_local_sessions` 返回值语义为"落库会话数"（upsert 全部计入）；`updated` 这里用 `total - imported` 作近似。若需要精确区分新增/更新，可让 imports 返回 (inserted, updated)——本期不做，保持最小改动。

- [ ] **步骤 3：get_codex_work / list_child_sessions**

```rust
    pub async fn get_codex_work(&self, thread_id: &str) -> Result<Vec<WorkItem>> {
        let work_dir = self.codex_work_dir();
        let thread_id = thread_id.to_string();
        Ok(tokio::task::spawn_blocking(move || {
            crate::codex::read_work_items(&work_dir, &thread_id)
        })
        .await
        .map_err(|e| AppError::Configuration(e.to_string()))?)
    }

    pub async fn list_child_sessions(&self, parent_platform_session_id: &str) -> Result<Vec<SessionSummary>> {
        database::list_child_sessions(&self.pool, parent_platform_session_id).await
    }
```

（`WorkItem`、`SessionSummary` 需在 service.rs 顶部 `use crate::models::{...}` 补齐。）

- [ ] **步骤 4：删除会话时清理工作 JSONL**

现有 `delete`（service.rs:2117）实现为：

```rust
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.ensure_writable()?;
        {
            let _guard = self.sync_gate.lock().await;
            delete_local_session(&self.pool, id).await?;
        }
        let _ = self.semantic.delete_session(id).await;
        tracing::info!("session deleted");
        self.notify_local_sync();
        Ok(())
    }
```

改为在删库前取 key、删库后清 JSONL（不改动既有 notify/semantic 逻辑）：

```rust
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.ensure_writable()?;
        let key = database::session_platform_key(&self.pool, id).await?;
        {
            let _guard = self.sync_gate.lock().await;
            delete_local_session(&self.pool, id).await?;
        }
        let _ = self.semantic.delete_session(id).await;
        if let Some((platform, platform_session_id)) = key
            && platform == "codex"
        {
            let work_dir = self.codex_work_dir();
            let _ = tokio::task::spawn_blocking(move || {
                crate::codex::delete_work_items(&work_dir, &platform_session_id)
            })
            .await;
        }
        tracing::info!("session deleted");
        self.notify_local_sync();
        Ok(())
    }
```

- [ ] **步骤 5：运行编译 + 既有测试**

运行：`cd app/src-tauri; cargo test --no-default-features service::`
预期：编译通过，既有 service 测试不回归。

- [ ] **步骤 6：Commit**

```bash
git add app/src-tauri/src/service.rs app/src-tauri/src/database
git commit -m "feat(codex): service 层导入、工作项读取与子会话列表"
```

## 任务 10：Tauri 命令与启动增量导入

**文件：**
- 修改：`app/src-tauri/src/commands.rs`、`app/src-tauri/src/lib.rs`

- [ ] **步骤 1：三个新命令（commands.rs）**

在 `commands.rs` 追加，并在顶部 `use crate::models::{...}` 补 `CodexImportResponse`、`WorkItem`、`SessionSummary`：

```rust
#[tauri::command]
pub async fn import_codex(
    service: State<'_, AppService>,
) -> Result<CodexImportResponse, String> {
    service.import_codex().await.map_err(message)
}

#[tauri::command]
pub async fn get_codex_work(
    service: State<'_, AppService>,
    thread_id: String,
) -> Result<Vec<WorkItem>, String> {
    service.get_codex_work(&thread_id).await.map_err(message)
}

#[tauri::command]
pub async fn list_child_sessions(
    service: State<'_, AppService>,
    parent_platform_session_id: String,
) -> Result<Vec<SessionSummary>, String> {
    service
        .list_child_sessions(&parent_platform_session_id)
        .await
        .map_err(message)
}
```

- [ ] **步骤 2：注册命令（lib.rs）**

`lib.rs` 的 `tauri::generate_handler![...]`（约 232-259 行）在 `commands::print_to_pdf` 后加：

```rust
            ,
            commands::import_codex,
            commands::get_codex_work,
            commands::list_child_sessions
```

- [ ] **步骤 3：启动时按设置做增量导入**

`lib.rs` setup 中 `app.manage(service.clone());`（约 194 行）之后、HTTP 服务 spawn 附近加入：仅当 `settings.codex.auto_watch` 为真时后台触发一次导入（不阻塞启动）。

```rust
            let codex_service = service.clone();
            tauri::async_runtime::spawn(async move {
                if codex_service.settings().await.codex.auto_watch {
                    match codex_service.import_codex().await {
                        Ok(resp) => tracing::info!(
                            imported = resp.imported,
                            updated = resp.updated,
                            "codex 启动增量导入完成"
                        ),
                        Err(error) => tracing::warn!(%error, "codex 启动增量导入失败"),
                    }
                }
            });
```

> 说明：本期"自动监听"落地为**启动时导入一次**（增量由任务 4 的 upsert + 后续 `codex_import_state` 去重保证幂等）。基于 `notify` 的实时 watcher 作为增强项列在任务 21，可后续接入；先确保开关驱动的导入闭环可用。

- [ ] **步骤 4：编译验证**

运行：`cd app/src-tauri; cargo build --no-default-features`
预期：无错误、无 clippy 阻断（命令签名符合 tauri 约定）。

- [ ] **步骤 5：Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/lib.rs
git commit -m "feat(codex): 注册导入/工作项/子会话命令并支持启动增量导入"
```

## 任务 11：前端类型与 API 方法（desktop-api.ts / conversation.ts）

**文件：**
- 修改：`app/src/desktop-api.ts`、`app/src/conversation.ts`
- 测试：`app/src/desktop-api.test.ts`

- [ ] **步骤 1：编写失败的测试**

在 `app/src/desktop-api.test.ts` 的 `describe('desktopApi', ...)` 内追加：

```ts
  it('maps codex commands to stable Tauri payloads', async () => {
    invoke.mockResolvedValue({ imported: 0, updated: 0, skipped: 0, failed: 0 })
    const { desktopApi } = await import('./desktop-api')

    await desktopApi.importCodex()
    expect(invoke).toHaveBeenLastCalledWith('import_codex')

    invoke.mockResolvedValue([])
    await desktopApi.getCodexWork('thread-abc')
    expect(invoke).toHaveBeenLastCalledWith('get_codex_work', { threadId: 'thread-abc' })

    await desktopApi.listChildSessions('parent-xyz')
    expect(invoke).toHaveBeenLastCalledWith('list_child_sessions', { parentPlatformSessionId: 'parent-xyz' })
  })
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/desktop-api.test.ts -t "codex commands"`
预期：FAIL，`desktopApi.importCodex is not a function`。

- [ ] **步骤 3：conversation.ts 扩展 SessionSummary 并新增 WorkItem**

`app/src/conversation.ts` 的 `SessionSummary`（第 1-9 行）在 `imported_at` 后加两个可选字段：

```ts
export type SessionSummary = {
  id: string
  platform: string
  platform_session_id: string
  title: string
  created_at?: string
  updated_at?: string
  imported_at?: string
  project?: string
  child_count?: number
}
```

在同文件 `ToolCall` 类型（第 27-31 行）之后新增 `WorkItem` 类型（字段与 Rust `models::WorkItem` 的 serde 输出一一对应）：

```ts
export type WorkItem = {
  work_seq: number
  seq: number
  kind: 'commentary' | 'reasoning' | 'command' | 'mcp_tool' | 'file_change' | 'subagent' | 'plan' | 'output'
  title: string
  body?: string
  expandable: boolean
  truncated: boolean
  agent_thread_id?: string
  agent_label?: string
}
```

- [ ] **步骤 4：desktop-api.ts 新增类型**

`app/src/desktop-api.ts` 顶部已从 `./conversation` 汇出类型的位置附近（保持与既有 `SessionSummary` 汇出一致），新增导出 `WorkItem`。找到现有的 `export type { ... } from './conversation'`（若存在）加入 `WorkItem`；若 `SessionSummary` 是在本文件直接从 conversation re-export，则同处追加 `WorkItem`。同时新增导入侧响应类型：

```ts
export type CodexImportResponse = {
  imported: number
  updated: number
  skipped: number
  failed: number
}

export type CodexSettings = {
  auto_watch: boolean
  codex_home?: string
}
```

在 `SettingsModel`（第 41-57 行）的 `custom_themes?` 前加：

```ts
  codex: CodexSettings
```

- [ ] **步骤 5：desktop-api.ts 接口与实现新增三方法**

`DesktopApi` 接口（第 167-195 行）在 `setNativeLocale` 前加：

```ts
  importCodex(): Promise<CodexImportResponse>
  getCodexWork(threadId: string): Promise<WorkItem[]>
  listChildSessions(parentPlatformSessionId: string): Promise<SessionSummary[]>
```

`desktopApi` 实现对象（第 197-225 行）在 `setNativeLocale` 前加：

```ts
  importCodex: () => invoke('import_codex'),
  getCodexWork: (threadId) => invoke('get_codex_work', { threadId }),
  listChildSessions: (parentPlatformSessionId) => invoke('list_child_sessions', { parentPlatformSessionId }),
```

> 说明：`WorkItem` 已在 conversation.ts 定义并在 desktop-api.ts re-export，因此实现文件顶部需 `import type { ... WorkItem }`（若 desktop-api.ts 已从 conversation 导入 `SessionSummary` 等，追加 `WorkItem` 即可）。确认文件内 `SessionSummary` 的导入路径，把 `WorkItem` 加进同一条 import。

- [ ] **步骤 6：运行确认通过**

运行：`cd app; npm test -- src/desktop-api.test.ts -t "codex commands"`
预期：PASS。

- [ ] **步骤 7：类型检查**

运行：`cd app; npm run typecheck`
预期：无类型错误（`SettingsModel` 新增必填 `codex` 字段后，App.vue 内联的 settings 初值将在任务 14 补齐；若 typecheck 此刻因该初值报错，先跳到任务 14 步骤 1 补 `codex` 默认值再回来验证）。

- [ ] **步骤 8：Commit**

```bash
git add app/src/desktop-api.ts app/src/conversation.ts app/src/desktop-api.test.ts
git commit -m "feat(codex): 前端新增 WorkItem 类型与 import/work/child 三个 API 方法"
```

## 任务 12：子会话目录缓存（useSessionCatalog.ts）

**文件：**
- 修改：`app/src/composables/useSessionCatalog.ts`
- 测试：`app/src/composables/useSessionCatalog.test.ts`

- [ ] **步骤 1：编写失败的测试**

在 `useSessionCatalog.test.ts` 追加（惰性拉取并缓存子会话）：

```ts
  it('lazily loads and caches child sessions per parent', async () => {
    const api = makeApi()
    api.listChildSessions = vi.fn().mockResolvedValue([
      { id: 'c1', platform: 'codex', platform_session_id: 'child-1', title: '子代理会话' },
    ])
    const catalog = useSessionCatalog(api as never)

    const first = await catalog.loadChildSessions('parent-1')
    const second = await catalog.loadChildSessions('parent-1')

    expect(first).toHaveLength(1)
    expect(second).toBe(first)
    expect(api.listChildSessions).toHaveBeenCalledTimes(1)
    expect(catalog.childSessions.value.get('parent-1')?.[0].id).toBe('c1')
  })
```

> 若测试文件已有 `makeApi()` 工厂，复用它并补 `listChildSessions`；若没有，按文件现有 mock 风格构造一个含所有 `DesktopApi` 方法的存根（`searchSessions` 返回 `{ sessions: [], total: 0, search_mode: 'hybrid', semantic_status: 'ready' }`）。

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/composables/useSessionCatalog.test.ts -t "child sessions"`
预期：FAIL，`catalog.loadChildSessions is not a function`。

- [ ] **步骤 3：实现子会话缓存**

`useSessionCatalog.ts` 在 `const semanticStatus = ref(...)` 附近新增状态与方法，并在 `return { ... }` 中导出：

```ts
  const childSessions = ref(new Map<string, SessionSummary[]>())
  const childPending = new Map<string, Promise<SessionSummary[]>>()

  async function loadChildSessions(parentPlatformSessionId: string): Promise<SessionSummary[]> {
    const cached = childSessions.value.get(parentPlatformSessionId)
    if (cached) return cached
    const inflight = childPending.get(parentPlatformSessionId)
    if (inflight) return inflight
    const request = api.listChildSessions(parentPlatformSessionId)
      .then((children) => {
        // 复用同一 Map 实例并重新赋值以触发响应式；缓存空数组同样命中，避免重复 IPC。
        const next = new Map(childSessions.value)
        next.set(parentPlatformSessionId, children)
        childSessions.value = next
        return children
      })
      .finally(() => childPending.delete(parentPlatformSessionId))
    childPending.set(parentPlatformSessionId, request)
    return request
  }

  function invalidateChildSessions() {
    childSessions.value = new Map()
    childPending.clear()
  }
```

在 `loadSessions` 的 `reset` 分支（`if (reset) { committedQuery.value = ... }` 处）追加 `invalidateChildSessions()`，使刷新目录时清空子会话缓存：

```ts
    if (reset) {
      committedQuery.value = query.value.trim()
      invalidateChildSessions()
    }
```

`return { ... }` 追加：`childSessions, loadChildSessions, invalidateChildSessions,`。

- [ ] **步骤 4：运行确认通过**

运行：`cd app; npm test -- src/composables/useSessionCatalog.test.ts`
预期：全部 PASS。

- [ ] **步骤 5：Commit**

```bash
git add app/src/composables/useSessionCatalog.ts app/src/composables/useSessionCatalog.test.ts
git commit -m "feat(codex): 会话目录支持惰性加载并缓存子会话"
```

## 任务 13：CodexWorkPanel 组件（工作过程展示）

**文件：**
- 创建：`app/src/components/CodexWorkPanel.vue`
- 测试：`app/src/components/CodexWorkPanel.test.ts`

工作项分类渲染规则（来自规格）：
- `reasoning`（思考）：`body` 为空视为加密 → 单行不可展开 `思考 · {title}`；`body` 非空 → 可展开、正文不截断。
- `subagent`：单行事件 `子代理 {agent_label} 已开始工作/已更新`，点击 emit `openSubagent`（携带 `agent_thread_id`）。
- 其余（`commentary`/`command`/`mcp_tool`/`file_change`/`plan`/`output`）：标题行 + 可展开正文（`expandable && body`），`truncated` 为真时正文尾部追加截断提示。
- 面板整体透明（无左侧色条），区别于蓝色"思考"。

- [ ] **步骤 1：编写失败的测试**

创建 `app/src/components/CodexWorkPanel.test.ts`：

```ts
/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkItem } from '../conversation'
import CodexWorkPanel from './CodexWorkPanel.vue'
import { setLocale } from '../i18n'

function mount(items: WorkItem[]) {
  document.body.innerHTML = '<div id="app"></div>'
  const openSubagent = vi.fn()
  const Root = defineComponent({
    setup: () => () => h(CodexWorkPanel, { items, onOpenSubagent: openSubagent }),
  })
  const app = createApp(Root)
  app.mount(document.getElementById('app')!)
  return { root: document.body, openSubagent, unmount: () => { app.unmount(); document.body.innerHTML = '' } }
}

function item(overrides: Partial<WorkItem> = {}): WorkItem {
  return { work_seq: 0, seq: 0, kind: 'command', title: 'ls -la', expandable: true, truncated: false, ...overrides }
}

beforeEach(() => setLocale('zh-CN'))
afterEach(() => { vi.restoreAllMocks(); document.body.innerHTML = '' })

describe('CodexWorkPanel', () => {
  it('renders an encrypted reasoning item as a non-expandable single line', () => {
    const h = mount([item({ kind: 'reasoning', title: '推理摘要', body: undefined, expandable: false })])
    try {
      const line = h.root.querySelector('.work-item.reasoning.locked')
      expect(line?.textContent).toContain('推理摘要')
      expect(h.root.querySelector('.work-item.reasoning .work-item-body')).toBeNull()
    } finally { h.unmount() }
  })

  it('expands a command item body on click', async () => {
    const h = mount([item({ kind: 'command', title: 'ls', body: 'total 0', expandable: true })])
    try {
      const toggle = h.root.querySelector<HTMLButtonElement>('.work-item.command .work-item-toggle')!
      toggle.click()
      await nextTick()
      expect(h.root.querySelector('.work-item.command .work-item-body')?.textContent).toContain('total 0')
    } finally { h.unmount() }
  })

  it('emits openSubagent with the agent thread id', () => {
    const h = mount([item({ kind: 'subagent', title: '子代理', agent_thread_id: 'sub-1', agent_label: '研究员', expandable: false })])
    try {
      h.root.querySelector<HTMLButtonElement>('.work-item.subagent .work-item-toggle')!.click()
      expect(h.openSubagent).toHaveBeenCalledWith('sub-1')
    } finally { h.unmount() }
  })

  it('shows a truncation notice when an item body is truncated', async () => {
    const h = mount([item({ kind: 'file_change', title: 'src/a.rs', body: 'diff…', expandable: true, truncated: true })])
    try {
      h.root.querySelector<HTMLButtonElement>('.work-item.file_change .work-item-toggle')!.click()
      await nextTick()
      expect(h.root.querySelector('.work-item-truncated')?.textContent).toContain('已截断')
    } finally { h.unmount() }
  })
})
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/components/CodexWorkPanel.test.ts`
预期：FAIL，无法解析 `./CodexWorkPanel.vue`。

- [ ] **步骤 3：实现 CodexWorkPanel.vue**

创建 `app/src/components/CodexWorkPanel.vue`：

```vue
<script setup lang="ts">
import { ref } from 'vue'
import type { WorkItem } from '../conversation'
import { translate as t } from '../i18n'

const props = defineProps<{ items: WorkItem[] }>()
const emit = defineEmits<{ openSubagent: [agentThreadId: string] }>()

// 每个可展开项独立记录开合状态，key 用 work_seq（同一会话内唯一）。
const openKeys = ref(new Set<number>())

function isReasoning(item: WorkItem) { return item.kind === 'reasoning' }
function isSubagent(item: WorkItem) { return item.kind === 'subagent' }
// 加密思考：reasoning 且无正文 → 只读单行。
function isLocked(item: WorkItem) { return isReasoning(item) && !item.body }
function isExpandable(item: WorkItem) { return !isSubagent(item) && item.expandable && !!item.body }

function toggle(item: WorkItem) {
  if (isSubagent(item)) {
    if (item.agent_thread_id) emit('openSubagent', item.agent_thread_id)
    return
  }
  if (!isExpandable(item)) return
  const next = new Set(openKeys.value)
  if (next.has(item.work_seq)) next.delete(item.work_seq)
  else next.add(item.work_seq)
  openKeys.value = next
}

function kindLabel(item: WorkItem) {
  const map: Record<WorkItem['kind'], string> = {
    commentary: t('work.kindCommentary'),
    reasoning: t('work.kindReasoning'),
    command: t('work.kindCommand'),
    mcp_tool: t('work.kindMcpTool'),
    file_change: t('work.kindFileChange'),
    subagent: t('work.kindSubagent'),
    plan: t('work.kindPlan'),
    output: t('work.kindOutput'),
  }
  return map[item.kind] ?? item.kind
}

function headerText(item: WorkItem) {
  if (isLocked(item)) return t('work.reasoningLine', { summary: item.title })
  if (isSubagent(item)) {
    return t('work.subagentLine', { agent: item.agent_label || item.agent_thread_id || '' })
  }
  return item.title
}
</script>

<template>
  <div class="codex-work" role="group" :aria-label="t('work.panelLabel')">
    <div v-for="item in props.items" :key="item.work_seq" :class="['work-item', item.kind, { locked: isLocked(item), open: openKeys.has(item.work_seq) }]">
      <button
        v-if="!isLocked(item)"
        class="work-item-toggle"
        type="button"
        :aria-expanded="isExpandable(item) ? openKeys.has(item.work_seq) : undefined"
        :data-interactive="isExpandable(item) || isSubagent(item) ? 'true' : 'false'"
        @click="toggle(item)"
      >
        <span class="work-item-kind">{{ kindLabel(item) }}</span>
        <span class="work-item-title">{{ headerText(item) }}</span>
      </button>
      <div v-else class="work-item-line">
        <span class="work-item-kind">{{ kindLabel(item) }}</span>
        <span class="work-item-title">{{ headerText(item) }}</span>
      </div>
      <div v-if="isExpandable(item) && openKeys.has(item.work_seq)" class="work-item-reveal">
        <pre class="work-item-body">{{ item.body }}</pre>
        <p v-if="item.truncated" class="work-item-truncated">{{ t('work.truncated') }}</p>
      </div>
    </div>
  </div>
</template>
```

- [ ] **步骤 4：补 i18n（先加 zh-CN，避免测试 `t()` 回退键名）**

`app/src/i18n/locales/zh-CN.ts` 在 `message: { ... }` 后新增一个 `work` 段（任务 17 会补 en-US）：

```ts
  work: {
    panelLabel: '工作过程',
    showWork: '查看工作过程',
    kindCommentary: '说明',
    kindReasoning: '思考',
    kindCommand: '命令',
    kindMcpTool: '工具',
    kindFileChange: '改动',
    kindSubagent: '子代理',
    kindPlan: '计划',
    kindOutput: '输出',
    reasoningLine: '思考 · {summary}',
    subagentLine: '子代理 {agent} 已开始工作',
    truncated: '内容较长，已截断',
  },
```

- [ ] **步骤 5：运行确认通过**

运行：`cd app; npm test -- src/components/CodexWorkPanel.test.ts`
预期：全部 PASS。

- [ ] **步骤 6：Commit**

```bash
git add app/src/components/CodexWorkPanel.vue app/src/components/CodexWorkPanel.test.ts app/src/i18n/locales/zh-CN.ts
git commit -m "feat(codex): 新增 CodexWorkPanel 工作过程展示组件"
```

## 任务 14：MessageBlock 渲染工作过程面板

**文件：**
- 修改：`app/src/MessageBlock.vue`
- 测试：`app/src/MessageBlock.test.ts`

思路：MessageBlock 新增可选 prop `workItems?: WorkItem[]`。当该消息带有工作项时，用 `CodexWorkPanel` 替换默认的"思考"下拉；`thinking` 段仅在无 `workItems` 时渲染（保持既有平台行为不变）。子代理跳转事件透传给上层。

- [ ] **步骤 1：编写失败的测试**

在 `app/src/MessageBlock.test.ts` 末尾（`describe` 之外或新增一个 describe）追加：

```ts
describe('MessageBlock codex work items', () => {
  it('renders the work panel instead of the thinking dropdown when work items exist', async () => {
    const items = [{ work_seq: 0, seq: 0, kind: 'command' as const, title: 'ls', body: 'total 0', expandable: true, truncated: false }]
    document.body.innerHTML = '<div id="app"></div>'
    const openSubagent = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(MessageBlock, {
        message: messageFixture({ metadata: { thinking: '被抑制的思考' } }),
        references: new Map(),
        query: '',
        expanded: true,
        formattedDate: '2026-09-27',
        roleLabel: '助手',
        workItems: items,
        onOpenSubagent: openSubagent,
      }),
    })
    const app = createApp(Root)
    app.mount(document.getElementById('app')!)
    try {
      await nextTick()
      expect(document.querySelector('.codex-work')).toBeTruthy()
      expect(document.querySelector('.thinking-toggle')).toBeNull()
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })
})
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/MessageBlock.test.ts -t "codex work"`
预期：FAIL（`.codex-work` 不存在；`.thinking-toggle` 仍在）。

- [ ] **步骤 3：MessageBlock 引入组件与 prop**

`MessageBlock.vue` 顶部 import 追加：

```ts
import CodexWorkPanel from './components/CodexWorkPanel.vue'
import { toolCallsFromMetadata, type Message, type Reference, type WorkItem } from './conversation'
```

（把原有 `import { toolCallsFromMetadata, type Message, type Reference } from './conversation'` 替换为上面这一行。）

`defineProps` 增加可选字段：

```ts
const props = defineProps<{
  message: Message
  references: Map<number, Reference>
  query: string
  expanded: boolean
  formattedDate: string
  roleLabel: string
  workItems?: WorkItem[]
}>()
```

`defineEmits` 增加透传事件：

```ts
const emit = defineEmits<{ toggleThinking: [messageId: string]; contentRendered: []; openSubagent: [agentThreadId: string] }>()
```

新增计算属性（放在 `thinking` 计算属性附近）：

```ts
const hasWorkItems = computed(() => Array.isArray(props.workItems) && props.workItems.length > 0)
```

- [ ] **步骤 4：模板改造**

将模板中"思考"`<section v-if="thinking" ...>`（第 121-124 行）的条件改为**仅无工作项时渲染**，并在其前插入工作面板：

```html
    <CodexWorkPanel v-if="hasWorkItems" :items="workItems!" @open-subagent="(id) => emit('openSubagent', id)" />
    <section v-if="thinking && !hasWorkItems" :class="['thinking', { open: expanded }]">
      <button class="thinking-toggle" :aria-expanded="expanded" @click="$emit('toggleThinking', message.id)">{{ t('message.showThinking') }}</button>
      <div class="thinking-reveal" :aria-hidden="!expanded"><div><div v-if="expanded" class="markdown" data-search-field="thinking" v-html="thinkingHtml"></div></div></div>
    </section>
```

（工具调用 `tool-calls` 段保持原样——Codex 会话不写 metadata.tool_calls，故不会与工作面板冲突。）

- [ ] **步骤 5：运行确认通过**

运行：`cd app; npm test -- src/MessageBlock.test.ts`
预期：新用例 PASS，既有用例不回归。

- [ ] **步骤 6：Commit**

```bash
git add app/src/MessageBlock.vue app/src/MessageBlock.test.ts
git commit -m "feat(codex): MessageBlock 支持渲染 Codex 工作过程面板"
```

## 任务 15：App.vue 加载并分发 Codex 工作项 + 子代理跳转

**文件：**
- 修改：`app/src/App.vue`

思路：选中会话若 `platform === 'codex'`，调用 `desktopApi.getCodexWork(platform_session_id)` 拉取全部工作项，按 `seq` 分组为 `Map<number, WorkItem[]>`；MessageBlock 按消息 `seq` 取对应切片。子代理事件 → 通过 `platform_session_id === agent_thread_id` 找到子会话并 `selectSession`。

- [ ] **步骤 1：新增状态与加载逻辑**

`App.vue` `<script setup>` 中，`const expandedThinking = ref(...)`（第 111 行）附近新增：

```ts
import type { WorkItem } from './conversation'
const codexWorkBySeq = ref(new Map<number, WorkItem[]>())

async function loadCodexWork(session: { platform: string; platform_session_id: string }) {
  codexWorkBySeq.value = new Map()
  if (session.platform !== 'codex') return
  try {
    const items = await desktopApi.getCodexWork(session.platform_session_id)
    const grouped = new Map<number, WorkItem[]>()
    for (const item of items) {
      const bucket = grouped.get(item.seq)
      if (bucket) bucket.push(item)
      else grouped.set(item.seq, [item])
    }
    // 组内按 work_seq 稳定排序，保证时间序渲染。
    for (const bucket of grouped.values()) bucket.sort((a, b) => a.work_seq - b.work_seq)
    codexWorkBySeq.value = grouped
  } catch (reason) {
    console.error('[CODEX] failed to load work items:', reason)
  }
}
```

- [ ] **步骤 2：在 selectSession 成功后触发加载**

`selectSession` 中拿到 `opened` 后（`const opened = selected.value` 之后、`try {` 内合适处，约第 554 行）加：

```ts
    void loadCodexWork(opened)
```

并在 `clearSelectedSession()`（第 205-211 行）末尾追加 `codexWorkBySeq.value = new Map()`。

- [ ] **步骤 3：子代理跳转**

新增函数（`toggleThinking` 附近）：

```ts
async function openSubagentSession(agentThreadId: string) {
  // 子会话以 platform_session_id === agent_thread_id 存储；优先在已加载目录中找，
  // 命中则直接切换（selectSession 内部会按 id 打开）。
  const child = sessions.value.find((s) => s.platform_session_id === agentThreadId)
  if (child) {
    await selectSession(child.id)
    return
  }
  // 目录未包含（子会话默认折叠未加载）：拉取当前父会话的子列表再匹配。
  if (selected.value) {
    const children = await loadChildSessions(selected.value.platform_session_id)
    const match = children.find((s) => s.platform_session_id === agentThreadId)
    if (match) await selectSession(match.id)
  }
}
```

> 依赖任务 12 的 `loadChildSessions`。在解构 `useSessionCatalog(...)` 的返回（第 213-217 行）中追加 `loadChildSessions, childSessions,`。

- [ ] **步骤 4：模板把工作项与事件传给 MessageBlock**

`<MessageBlock ...>`（第 1021-1031 行）追加两个绑定：

```html
                      :work-items="codexWorkBySeq.get(messageSlots[displayedMessageSeqs[virtualMessage.index]]!.seq)"
                      @open-subagent="openSubagentSession"
```

- [ ] **步骤 5：SettingsModel 内联初值补 codex 默认值**

`settings` 的 `ref<SettingsModel>({ ... })` 内联默认对象（第 112 行）在 `mcp_enabled: false,` 附近追加：

```ts
codex: { auto_watch: false },
```

（补齐任务 11 引入的必填字段，消除 typecheck 报错。）

- [ ] **步骤 6：类型检查 + 相关测试**

运行：`cd app; npm run typecheck`
预期：无错误。
运行：`cd app; npm test -- src/app-initialization.test.ts`
预期：不回归（若该测试构造了 SettingsModel，需同步补 `codex` 字段——按报错定位补齐）。

- [ ] **步骤 7：Commit**

```bash
git add app/src/App.vue
git commit -m "feat(codex): App 加载并分发工作项、支持子代理会话跳转"
```

## 任务 16：SessionList 新增"项目"列与可展开子会话行

**文件：**
- 修改：`app/src/components/SessionList.vue`
- 测试：`app/src/components/SessionList.test.ts`（若不存在则创建）

思路：表头与每行新增"项目"列（`session.project`）。当 `session.child_count > 0` 时标题前显示折叠箭头；点击箭头（不触发选中）向上 `emit('toggleChildren', session.id)`。子会话行由父组件通过 `childSessions` prop 传入并在展开时缩进渲染。SessionList 保持无状态：展开集合与子数据都来自 props。

- [ ] **步骤 1：编写失败的测试**

创建 `app/src/components/SessionList.test.ts`：

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick } from 'vue'
import SessionList from './SessionList.vue'
import { setLocale } from '../i18n'
import type { SessionSummary } from '../conversation'

function summary(over: Partial<SessionSummary>): SessionSummary {
  return { id: 'x', platform: 'codex', platform_session_id: 't', title: 'T', updated_at: '2026-09-27T00:00:00Z', message_count: 1, ...over }
}

async function mountList(props: Record<string, unknown>) {
  document.body.innerHTML = '<div id="app"></div>'
  const Root = defineComponent({ setup: () => () => h(SessionList, props) })
  const app = createApp(Root)
  app.mount(document.getElementById('app')!)
  await nextTick()
  return app
}

describe('SessionList project column and children', () => {
  setLocale('zh-CN')
  afterEach(() => { document.body.innerHTML = '' })

  it('renders the project cell', async () => {
    const app = await mountList({ sessions: [summary({ project: 'my-repo' })], total: 1, loading: false, filtered: false, query: '', childSessions: new Map(), expandedParents: new Set() })
    try { expect(document.querySelector('.project-cell')?.textContent).toContain('my-repo') }
    finally { app.unmount() }
  })

  it('emits toggleChildren when the collapse arrow is clicked', async () => {
    const onToggle = vi.fn()
    const app = await mountList({ sessions: [summary({ id: 'p', child_count: 2 })], total: 1, loading: false, filtered: false, query: '', childSessions: new Map(), expandedParents: new Set(), onToggleChildren: onToggle })
    try {
      const arrow = document.querySelector('.child-toggle') as HTMLElement
      expect(arrow).toBeTruthy()
      arrow.click()
      expect(onToggle).toHaveBeenCalledWith('p')
    } finally { app.unmount() }
  })

  it('renders child rows when the parent is expanded', async () => {
    const children = new Map([['p', [summary({ id: 'c', title: '子会话' })]]])
    const app = await mountList({ sessions: [summary({ id: 'p', child_count: 1 })], total: 1, loading: false, filtered: false, query: '', childSessions: children, expandedParents: new Set(['p']) })
    try { expect(document.querySelector('.session-row.child')?.textContent).toContain('子会话') }
    finally { app.unmount() }
  })
})
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/components/SessionList.test.ts`
预期：FAIL（`.project-cell` / `.child-toggle` / `.session-row.child` 均不存在）。

- [ ] **步骤 3：扩展 props 与 emits**

`defineProps` 增加两个字段：

```ts
const props = defineProps<{
  sessions: SessionSummary[]
  total: number
  loading: boolean
  selectedId?: string
  filtered: boolean
  query: string
  childSessions: Map<string, SessionSummary[]>
  expandedParents: Set<string>
}>()
const emit = defineEmits<{ select: [id: string]; loadMore: []; toggleChildren: [parentId: string] }>()
```

`platformName` 映射追加 `codex: 'Codex',`。新增箭头点击处理（阻止冒泡到行选中）：

```ts
function handleToggleChildren(id: string, event: MouseEvent) {
  event.stopPropagation()
  emit('toggleChildren', id)
}
```

- [ ] **步骤 4：模板改造**

表头改为三列 → 四列（在 source 与 updated 间插入项目）：

```html
    <div class="table-head"><span>{{ t('session.conversation') }}</span><span>{{ t('session.source') }}</span><span>{{ t('session.project') }}</span><span>{{ t('session.updated') }}</span></div>
```

行渲染改为渲染父行 + 展开时的子行。将 `.session-items` 内的 `v-for` 替换为：

```html
        <div class="session-items">
          <template v-for="session in sessions" :key="session.id">
            <button :class="['session-row', { selected: selectedId === session.id }]" @pointerdown="handleSessionPointerDown(session.id, $event)" @click="handleSessionClick(session.id)">
              <span class="session-title">
                <span v-if="(session.child_count ?? 0) > 0" :class="['child-toggle', { open: expandedParents.has(session.id) }]" role="button" @pointerdown.stop @click="handleToggleChildren(session.id, $event)"></span>
                <strong v-html="highlightTitle(session.title)"></strong>
              </span>
              <span class="platform-cell"><i :class="session.platform"></i>{{ platformName(session.platform) }}</span>
              <span class="project-cell">{{ session.project || '-' }}</span>
              <time>{{ formatDate(session.updated_at) }}</time>
            </button>
            <button
              v-for="child in (expandedParents.has(session.id) ? (childSessions.get(session.id) ?? []) : [])"
              :key="child.id"
              :class="['session-row', 'child', { selected: selectedId === child.id }]"
              @pointerdown="handleSessionPointerDown(child.id, $event)"
              @click="handleSessionClick(child.id)"
            >
              <span class="session-title"><strong v-html="highlightTitle(child.title)"></strong></span>
              <span class="platform-cell"><i :class="child.platform"></i>{{ platformName(child.platform) }}</span>
              <span class="project-cell">{{ child.project || '-' }}</span>
              <time>{{ formatDate(child.updated_at) }}</time>
            </button>
          </template>
        </div>
```

- [ ] **步骤 5：运行确认通过**

运行：`cd app; npm test -- src/components/SessionList.test.ts`
预期：全部 PASS。

- [ ] **步骤 6：Commit**

```bash
git add app/src/components/SessionList.vue app/src/components/SessionList.test.ts
git commit -m "feat(codex): 会话列表新增项目列与可展开子会话行"
```

## 任务 17：App.vue 将子会话数据与展开状态接入 SessionList

**文件：**
- 修改：`app/src/App.vue`

思路：任务 12 已在 `useSessionCatalog` 暴露 `childSessions`/`loadChildSessions`。App.vue 维护 `expandedParents = ref(new Set<string>())`，切换时惰性加载子会话，并把两者传给 SessionList。

- [ ] **步骤 1：新增展开状态与切换函数**

`App.vue` 中（`codexWorkBySeq` 附近）：

```ts
const expandedParents = ref(new Set<string>())

async function toggleChildren(parentId: string) {
  const next = new Set(expandedParents.value)
  if (next.has(parentId)) {
    next.delete(parentId)
    expandedParents.value = next
    return
  }
  next.add(parentId)
  expandedParents.value = next
  const parent = sessions.value.find((s) => s.id === parentId)
  if (parent) await loadChildSessions(parent.platform_session_id)
}
```

> `childSessions` 以 `platform_session_id` 为键（见任务 12）。因此模板传给 SessionList 时需按父行的 `platform_session_id` 取子数组——为避免在 SessionList 里再持有该映射，改为在 App.vue 构造一个"以父会话 id 为键"的视图 Map。

在 `computed` 区新增：

```ts
const childSessionsByParentId = computed(() => {
  const view = new Map<string, SessionSummary[]>()
  for (const session of sessions.value) {
    const kids = childSessions.value.get(session.platform_session_id)
    if (kids) view.set(session.id, kids)
  }
  return view
})
```

> 需从 `useSessionCatalog` 解构出 `childSessions`、`loadChildSessions`（若任务 15 已加则复用）。并 `import type { SessionSummary } from './conversation'`（如尚未导入）。

- [ ] **步骤 2：模板接线**

`<SessionList ...>`（约第 990 行，查找 `<SessionList`）追加：

```html
        :child-sessions="childSessionsByParentId"
        :expanded-parents="expandedParents"
        @toggle-children="toggleChildren"
```

- [ ] **步骤 3：类型检查**

运行：`cd app; npm run typecheck`
预期：无错误。

- [ ] **步骤 4：Commit**

```bash
git add app/src/App.vue
git commit -m "feat(codex): App 接入子会话展开状态与惰性加载"
```

## 任务 18：SettingsDialog 新增 Codex 导入区

**文件：**
- 修改：`app/src/components/SettingsDialog.vue`
- 修改：`app/src/App.vue`
- 测试：`app/src/components/SettingsDialog.test.ts`（若不存在则创建最小用例）

思路：在"常规"页新增 Codex 分区：自动监听开关（`v-model` 绑定 `settings.codex.auto_watch`）、`codex_home` 路径输入、"立即导入"按钮。按钮点击 `emit('importCodex')`；App.vue 处理该事件调用 `desktopApi.importCodex()` 并刷新目录。

- [ ] **步骤 1：编写失败的测试**

创建/追加 `app/src/components/SettingsDialog.test.ts`：

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick } from 'vue'
import SettingsDialog from './SettingsDialog.vue'
import { setLocale } from '../i18n'
import type { SettingsModel } from '../desktop-api'

function baseSettings(): SettingsModel {
  return {
    theme: 'system', language: 'zh-CN', accent: 'emerald', font_scale: 1,
    close_behavior: 'ask', tray_single_click: 'show', semantic_enabled: false,
    semantic_backend: 'local', mcp_enabled: false, codex: { auto_watch: false },
  } as SettingsModel
}

describe('SettingsDialog codex import', () => {
  setLocale('zh-CN')
  afterEach(() => { document.body.innerHTML = '' })

  it('emits importCodex when the import button is clicked', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const onImport = vi.fn()
    const Root = defineComponent({ setup: () => () => h(SettingsDialog, { open: true, settings: baseSettings(), page: 'general', onImportCodex: onImport } as Record<string, unknown>) })
    const app = createApp(Root)
    app.mount(document.getElementById('app')!)
    try {
      await nextTick()
      const button = document.querySelector('.codex-import-now') as HTMLButtonElement
      expect(button).toBeTruthy()
      button.click()
      expect(onImport).toHaveBeenCalled()
    } finally { app.unmount() }
  })
})
```

> 若 SettingsDialog 需要更多必填 prop 才能挂载，按 typecheck/运行报错补齐到 `Root` 的 props（参考组件 `defineProps`）。

- [ ] **步骤 2：运行确认失败**

运行：`cd app; npm test -- src/components/SettingsDialog.test.ts`
预期：FAIL（`.codex-import-now` 不存在）。

- [ ] **步骤 3：新增 emit 与分区模板**

`SettingsDialog.vue` emits（第 107-125 行区块）追加：

```ts
  (event: 'importCodex'): void
```

在"常规"页 `closeBehavior/trayClick` 之后（约第 500 行）插入：

```html
        <div class="setting-group codex-settings">
          <h3>{{ t('settings.codexTitle') }}</h3>
          <label class="setting-row switch-row">
            <span>{{ t('settings.codexAutoWatch') }}</span>
            <input type="checkbox" v-model="settings.codex.auto_watch" />
          </label>
          <label class="setting-row">
            <span>{{ t('settings.codexHome') }}</span>
            <input type="text" v-model="settings.codex.codex_home" :placeholder="t('settings.codexHomePlaceholder')" />
          </label>
          <button class="codex-import-now" @click="emit('importCodex')">{{ t('settings.codexImportNow') }}</button>
        </div>
```

> `settings` 是 `v-model:settings` 传入的可写对象（组件已用 `defineModel`/props 双向）。若组件用 props + emit 模式而非可写 model，需改为在本地副本上编辑并 `emit('update:settings', ...)`——按组件既有写法对齐（查看第 74-125 行的既有 setting-row 如何回写）。

- [ ] **步骤 4：App.vue 处理 importCodex**

`App.vue` 新增处理函数（`toggleChildren` 附近）：

```ts
async function handleImportCodex() {
  try {
    const result = await desktopApi.importCodex()
    console.log('[CODEX] import result:', result)
    await loadSessions()
  } catch (reason) {
    console.error('[CODEX] import failed:', reason)
  }
}
```

`<SettingsDialog ...>`（第 1050-1082 行）追加：

```html
        @import-codex="handleImportCodex"
```

- [ ] **步骤 5：运行确认通过**

运行：`cd app; npm test -- src/components/SettingsDialog.test.ts`
预期：PASS。
运行：`cd app; npm run typecheck`
预期：无错误。

- [ ] **步骤 6：Commit**

```bash
git add app/src/components/SettingsDialog.vue app/src/components/SettingsDialog.test.ts app/src/App.vue
git commit -m "feat(codex): 设置面板新增 Codex 导入区与自动监听开关"
```

## 任务 19：i18n 文案（zh-CN 补全 + en-US 镜像）

**文件：**
- 修改：`app/src/i18n/locales/zh-CN.ts`
- 修改：`app/src/i18n/locales/en-US.ts`
- 测试：`app/src/i18n/locale.test.ts`（若有键完整性测试则复用；否则本任务靠 typecheck 保证结构一致）

思路：任务 13 已加 `work` 块到 zh-CN。本任务补齐 `session.project` 与 `settings.codex*` 键，并在 en-US 完整镜像 `work` 块与新增键，保持两个 locale 结构对称（若项目有键对齐测试会强制）。

- [ ] **步骤 1：zh-CN 补键**

`app/src/i18n/locales/zh-CN.ts` 的 `session` 块追加：

```ts
    project: '项目',
```

`settings` 块追加：

```ts
    codexTitle: 'Codex 导入',
    codexAutoWatch: '启动时自动导入 Codex 会话',
    codexHome: 'Codex 目录',
    codexHomePlaceholder: '默认 %USERPROFILE%\\.codex',
    codexImportNow: '立即导入',
```

- [ ] **步骤 2：en-US 镜像 work 块**

`app/src/i18n/locales/en-US.ts` 追加与 zh-CN 对称的 `work` 块（键名一致，值为英文）：

```ts
  work: {
    panelLabel: 'Work process',
    showWork: 'View work process',
    kindCommentary: 'Note',
    kindReasoning: 'Thinking',
    kindCommand: 'Command',
    kindMcpTool: 'Tool call',
    kindFileChange: 'File change',
    kindSubagent: 'Subagent',
    kindPlan: 'Plan',
    kindOutput: 'Output',
    reasoningLine: 'Thinking · {summary}',
    subagentLine: 'Subagent {agent} started working',
    truncated: 'Truncated',
  },
```

> 键集必须与 zh-CN 的 `work` 块（任务 13）逐一对应；若任务 13 的实际键与此处不同，以任务 13 为准并同步调整此处。

- [ ] **步骤 3：en-US 补 session/settings 键**

en-US `session` 块追加 `project: 'Project',`；`settings` 块追加：

```ts
    codexTitle: 'Codex import',
    codexAutoWatch: 'Auto-import Codex sessions on startup',
    codexHome: 'Codex directory',
    codexHomePlaceholder: 'Default %USERPROFILE%\\.codex',
    codexImportNow: 'Import now',
```

- [ ] **步骤 4：类型检查 + 全量前端测试**

运行：`cd app; npm run typecheck`
预期：无错误（若有 locale 键对齐测试，此步会暴露遗漏键）。
运行：`cd app; npm test`
预期：全绿。

- [ ] **步骤 5：Commit**

```bash
git add app/src/i18n/locales/zh-CN.ts app/src/i18n/locales/en-US.ts
git commit -m "feat(codex): 补全中英文 Codex 相关文案"
```

## 任务 20：样式（项目列网格、子会话缩进、工作面板）

**文件：**
- 修改：`app/src/style.css`

思路：`.table-head/.session-row` 网格从三列改四列（新增项目列）；`.session-row.child` 缩进；`.child-toggle` 箭头；`.codex-work/.work-item` 面板样式——**透明背景、无左侧色条**（区别于 `.thinking` 的浅色块），`.work-item.reasoning.locked` 单行不可展开，`.work-item-truncated` 弱化提示。

- [ ] **步骤 1：网格改四列**

`app/src/style.css` 第 171 行 `.table-head, .session-row { ... grid-template-columns: minmax(190px, 1fr) 102px 108px; ... }` 改为：

```css
.table-head,
.session-row {
  display: grid;
  grid-template-columns: minmax(160px, 1fr) 96px 120px 108px;
  gap: 14px;
  align-items: center;
}
```

- [ ] **步骤 2：项目列、折叠箭头、子行缩进**

追加：

```css
.project-cell {
  font-size: 12px;
  color: var(--muted, #8a8f98);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.session-title { display: flex; align-items: center; gap: 6px; min-width: 0; }
.child-toggle {
  width: 14px;
  height: 14px;
  flex: none;
  cursor: pointer;
  position: relative;
}
.child-toggle::before {
  content: '';
  position: absolute;
  inset: 0;
  margin: auto;
  width: 0;
  height: 0;
  border-left: 5px solid currentColor;
  border-top: 4px solid transparent;
  border-bottom: 4px solid transparent;
  transition: transform 0.15s ease;
}
.child-toggle.open::before { transform: rotate(90deg); }
.session-row.child { padding-left: 26px; }
.session-row.child .session-title strong { font-weight: 500; opacity: 0.9; }
```

- [ ] **步骤 3：工作面板样式**

追加：

```css
.codex-work {
  margin-top: 8px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  background: transparent;
}
.work-item {
  border: none;
  background: transparent;
  padding: 2px 0;
}
.work-item-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  border: none;
  background: transparent;
  color: var(--muted, #8a8f98);
  font-size: 12px;
  cursor: pointer;
  text-align: left;
  padding: 2px 0;
}
.work-item.reasoning.locked .work-item-toggle { cursor: default; }
.work-item-body {
  margin: 2px 0 4px 14px;
  padding: 6px 10px;
  border-left: 2px solid var(--border, #2a2d31);
  font-size: 12px;
  white-space: pre-wrap;
  word-break: break-word;
}
.work-item-truncated {
  color: var(--warn, #c99a4b);
  font-size: 11px;
  margin-left: 14px;
}
```

- [ ] **步骤 4：暗色微调（如需）**

若第 672-677 行附近有 `.thinking` 暗色变量，为 `.work-item-body` 追加暗色边框对齐既有变量即可（若已用 `var(--border)` 则无需额外规则）。

- [ ] **步骤 5：构建校验**

运行：`cd app; npm run build`
预期：构建成功（CSS 无语法错误，Vue 模板引用的 class 均存在）。

- [ ] **步骤 6：Commit**

```bash
git add app/src/style.css
git commit -m "style(codex): 项目列、子会话缩进与工作过程面板样式"
```

## 任务 21（可选增强）：notify 实时目录监听

**文件：**
- 修改：`app/src-tauri/src/import_history.rs`（或新增 `app/src-tauri/src/codex_watch.rs`）
- 修改：`app/src-tauri/src/lib.rs`（启动时按 `settings.codex.auto_watch` 拉起 watcher）

思路：默认交付的 auto_watch 为"启动时一次性导入"。此任务把它升级为 `notify` v8 实时监听 `.codex/sessions` 与 `archived_sessions`，防抖后对新增/修改的 `rollout-*.jsonl` 增量导入。属增强项，可在核心功能验收后再做。

- [ ] **步骤 1：编写防抖导入的单元测试**

`import_history.rs` 测试模块新增：给定一个临时目录，写入一个 `rollout-*.jsonl`，调用 `import_codex_path(single_file)`，断言返回 `imported == 1`；再次调用断言 `updated`/`skipped`（幂等）。

```rust
#[tokio::test]
async fn codex_watch_incremental_import_is_idempotent() {
    // 构造临时 rollout 文件，首次导入 imported==1，二次导入不重复计入 imported。
    // 复用既有 import_codex_file 逻辑（见任务 4/5 定义的函数）。
}
```

- [ ] **步骤 2：运行确认失败**

运行：`cd app/src-tauri; cargo test --all-features codex_watch_incremental`
预期：FAIL（函数未导出或行为未实现）。

- [ ] **步骤 3：实现 watcher（防抖 500ms）**

用 `notify::recommended_watcher` 递归监听两个子目录，事件经 `tokio::sync::mpsc` + 500ms 防抖聚合，对收到路径若匹配 `rollout-*.jsonl` 调用既有单文件导入函数；导入后 `notify_local_sync` 触发前端刷新。

- [ ] **步骤 4：lib.rs 启动接线**

`setup` 中读取 `settings.codex.auto_watch`，为真则 `tauri::async_runtime::spawn` 启动 watcher（句柄存入 state 以便设置变更时启停）。

- [ ] **步骤 5：运行确认通过**

运行：`cd app/src-tauri; cargo test --all-features codex_watch_incremental`
预期：PASS。

- [ ] **步骤 6：Commit**

```bash
git add app/src-tauri/src
git commit -m "feat(codex): 新增 notify 实时目录监听与增量导入"
```




