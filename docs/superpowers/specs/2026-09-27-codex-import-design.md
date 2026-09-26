# Codex 对话记录导入设计

日期：2026-09-27
状态：待审阅

## 1. 目标

把本机 OpenAI Codex（桌面端 / TUI / exec）的对话记录导入 ai-chat-memory：

- 用户消息、最终回答、过程说明可全文 / 语义搜索；
- 每轮的工作过程（思考摘要、工具调用、文件 diff、子代理事件、计划）可在"工作过程"下拉栏中查看，但不进入 SQLite；
- 子代理会话挂在父会话下，列表可展开；
- 对话列表新增"项目"列；
- 可选的自动监听，增量导入新对话。

非目标：云同步 `codex-work/` 目录（第一版不做）；解密 Codex 加密思考内容（不可能）；写回 Codex。

## 2. 数据源（已在本机与 openai/codex main 源码核实）

Codex 数据根目录默认 `%USERPROFILE%\.codex`：

| 路径 | 用途 |
|---|---|
| `sessions/YYYY/MM/DD/rollout-<ts>-<thread_id>.jsonl` | 会话完整记录（主数据源） |
| `archived_sessions/*.jsonl` | 已归档会话，格式相同 |
| `state_5.sqlite` 表 `threads` | 标题 `title`、`cwd`、`archived`、`agent_nickname`、`agent_role`、`model` 等；只读打开 |
| `session_index.jsonl` | `id → thread_name`，标题兜底 |

Rollout 每行 `{timestamp, ordinal?, type, payload}`（上游 `codex-rs/history/src/lib.rs` `RolloutLine` / `rollout_payload.rs` `RolloutItemWire`）。首行必为 `session_meta`（`id`、`cwd`、`originator`、`cli_version`、`source`、`parent_thread_id?`、`agent_nickname?`、`agent_role?`）。

界面可见内容以 `event_msg` 中 `type = "item_completed"` 的 `payload.item` 为准（上游 `rollout/src/policy.rs`：新版分页 rollout 持久化 `ItemCompleted` 的 `TurnItem`；旧 `user_message`/`agent_message` 事件已不再写入）。`item.type` 取值与用途：

| item.type | 去向 |
|---|---|
| `UserMessage`（`content[].text`，已剔除注入上下文） | SQLite 用户消息 |
| `AgentMessage`，`phase = "final_answer"` | SQLite 助手消息（最终回答） |
| `AgentMessage`，`phase = "commentary"` | SQLite 助手消息，标记为过程说明 |
| `Reasoning`（`summary_text[]`、`raw_content[]`） | JSONL `reasoning` |
| `McpToolCall` / `CommandExecution` / `FunctionCallOutput` 及 `response_item` 的 `function_call`/`custom_tool_call` + `*_output` | JSONL `tool` |
| `FileChange`（`changes{path:{type, unified_diff}}`） | JSONL `file_change` |
| `SubAgentActivity`（`kind`、`agent_thread_id`、`agent_path`） | JSONL `subagent` |
| `Plan`（`text`） | JSONL `plan` |
| `ContextCompaction` | 忽略 |

本机现状：664 个 rollout、631 MB；最大单文件 81 MB；447 个为子代理会话；思考条目 28266 条全部只有摘要标题，`raw_content` 为空。

### 2.1 兜底：无 `item_completed` 的 rollout

旧版本会话与多数子代理会话没有 `item_completed`。此时改用 `response_item`：

- `message` 且 `role = "user"`：逐个 `input_text` 用上游 `core/src/context/contextual_user_message.rs` 的规则判定是否为注入片段（AGENTS.md 指令、`<environment_context>`、`<permissions instructions>`、技能注入、子代理通知、turn aborted 等标签前缀），全部为注入则丢弃，否则保留非注入片段为用户消息；
- `message` 且 `role = "assistant"`：按 `phase` 分为最终回答 / 过程说明，`phase` 缺失时视为最终回答；
- `role = "developer"` 一律丢弃；
- `reasoning`：取 `summary[].text`，`content` 非空时作为明文正文，`encrypted_content` 视为加密；
- `function_call` / `custom_tool_call` 与对应 `*_output` 按 `call_id` 配对成 `tool` 工作项。

同一 rollout 只要出现任一 `item_completed`，就只用 `item_completed` 路径，避免重复。

### 2.2 轮次（turn）划分

以 `event_msg/task_started` 的 `turn_id` 开启一轮；`item_completed` 自带 `turn_id`。兜底路径中没有 `turn_id` 时，以每条用户消息开启新一轮，turn_id 取 `"implicit-<序号>"`。

## 3. 存储

### 3.1 SQLite（可搜索部分）

`platform = "codex"`，`platform_session_id = thread_id`。

`messages` 行：

- 用户消息：`role = "user"`；
- 最终回答：`role = "assistant"`，`metadata = {"codex":{"turn_id":T,"phase":"final_answer","work_count":N}}`；
- 过程说明：`role = "assistant"`，`metadata = {"codex":{"turn_id":T,"phase":"commentary","work_seq":S}}`，`S` 为它在本轮工作项序列中的位置，用于在下拉栏内按时间顺序穿插。

一轮没有 `final_answer` 时（中断、子代理），该轮最后一条 AgentMessage 视为最终回答；一轮完全没有 AgentMessage 时不生成助手消息，工作项挂到该轮用户消息的 `metadata.codex` 上。

过程说明作为普通消息进入现有 FTS 与语义索引，无需改动索引代码。

`sessions.raw_data` 只存 `session_meta` 要点：`{cwd, originator, cli_version, source, model, archived}`，不存整份 rollout。

`sessions.created_at` / `updated_at` 取 `threads.created_at_ms` / `updated_at_ms`，缺失时取首行与末行 `timestamp`。

### 3.2 结构变更（`database/connection.rs`，沿用 `pragma_table_info` + `ALTER TABLE ADD COLUMN` 模式）

- `sessions.project TEXT`：cwd 最后一级目录名；其他平台为 NULL。
- `sessions.parent_platform_session_id TEXT`：子代理会话的父 thread_id；并建索引 `idx_sessions_parent(platform, parent_platform_session_id)`。
- `sessions.agent_label TEXT`：子代理显示名，`agent_nickname`（+ ` · agent_role`）。
- 新表 `codex_import_state(path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime_ms INTEGER NOT NULL, thread_id TEXT, imported_at TEXT NOT NULL)`。

### 3.3 工作过程 JSONL（不可搜索部分）

路径：`<应用数据目录>/codex-work/<thread_id>.jsonl`，每会话一个文件，导入时整体重写（先写 `.tmp` 再原子 rename）。

每行一个工作项：

```json
{"turn_id":"…","seq":3,"kind":"reasoning","title":"Planning local screenshot approach","text":null,"encrypted":true}
{"turn_id":"…","seq":4,"kind":"tool","name":"shell_command","input":"git status","output":"…","truncated":false,"original_bytes":812}
{"turn_id":"…","seq":5,"kind":"file_change","path":"src/a.rs","change":"update","diff":"@@ …","added":12,"removed":3,"truncated":false,"original_bytes":2048}
{"turn_id":"…","seq":6,"kind":"subagent","event":"started","agent_thread_id":"…","label":"Euler"}
{"turn_id":"…","seq":7,"kind":"plan","text":"# …"}
```

- `seq` 与 3.1 中过程说明的 `work_seq` 共用同一轮内的递增序列。
- `title` 取 `summary_text` 首条，去掉 `**` 包裹与 `<!-- -->`；无摘要时为 null。
- `text`：明文思考正文（`raw_content` 或 `content`），不截断；无明文时 null，`encrypted = true`。
- 截断：`tool.input`、`tool.output`、`file_change.diff` 各自超过 **130 KB**（按 UTF-8 字节，在字符边界截断）时截断，`truncated = true`，`original_bytes` 记原长。
- `subagent.event`：`started` → "已开始工作"，其余 kind → "已更新"。
- 删除会话时同步删除对应 JSONL；数据目录迁移时 `codex-work/` 随之复制。

## 4. 后端组件

### 4.1 `codex.rs`（新模块）

- `parse_rollout(path, meta_lookup) -> Result<CodexThread>`：`BufReader` 逐行流式解析，单行解析失败记 warn 并跳过；返回 `NormalizedSession` + `Vec<WorkItem>` + 父子关系。
- `ThreadMetaLookup`：只读打开 `state_5.sqlite`（`mode=ro`，失败时降级为 `session_index.jsonl`，再降级为首条用户消息截断 60 字符）提供标题、归档状态、nickname/role。
- `is_contextual_user_text(&str) -> bool`：注入片段判定，规则集中列出并附上游出处注释。
- `write_work_file(dir, thread_id, &[WorkItem])`、`read_work_items(dir, thread_id, turn_id)`。

### 4.2 导入服务

- `AppService::import_codex(root: PathBuf) -> ImportResponse`：扫描 `sessions/**/rollout-*.jsonl` 与 `archived_sessions/*.jsonl`；对比 `codex_import_state` 的 size/mtime，仅处理新增或变更文件；每个会话替换式写入（删除旧消息后插入），持 `sync_gate`；完成后对变更会话触发语义索引。
- 解析在 `spawn_blocking` 中执行，按文件顺序处理，避免 81 MB 文件整体入内存。
- 现有文件导入 `parse_import_history` 追加嗅探：`.jsonl` 且首行 `type = "session_meta"` → 单个 rollout 导入（`validate_file_path` 允许 `jsonl`）。

### 4.3 自动监听

- 设置项 `codex.root`（默认 `%USERPROFILE%\.codex`）、`codex.auto_watch`（默认 false）。
- 开启时用 `notify` crate（新增依赖）递归监听 `sessions/` 与 `archived_sessions/`，事件防抖 3 秒后调用增量导入；关闭或修改目录时停止旧 watcher。
- 应用启动时若开启则先做一次增量导入。

### 4.4 新 Tauri 命令（`desktop-api.ts` 同步补类型）

- `import_codex(root?: string) -> ImportResponse`
- `get_codex_work(session_id, turn_id) -> WorkItem[]`
- `list_child_sessions(session_id) -> SessionSummary[]`
- 设置读写沿用现有 `get_settings` / 保存设置命令。

### 4.5 列表与搜索查询

- 顶层列表与分页总数排除"父会话存在于库中"的子代理会话；父会话未导入或已被删除的子代理按顶层会话显示，避免不可达。`SessionSummary` 增加 `project`、`child_count`。
- 删除父会话不级联删除子代理（子代理随即回到顶层）。
- 搜索命中子代理会话时，结果返回其父会话，并附 `matched_child_id`，前端据此展开并选中子代理。

## 5. 前端

### 5.1 对话列表（`SessionList.vue`）

- 表头：对话 | 项目 | 来源 | 更新时间；项目列固定宽度、省略号，空值显示 `-`。
- `child_count > 0` 的行首显示折叠箭头与数量；点箭头展开 / 收起（懒加载 `list_child_sessions`），点行打开父会话。
- 子代理行缩进一级，标题为 `agent_label`，无则用会话标题。
- 选中子代理（点击、工作过程跳转、搜索命中）时自动展开其父会话。
- 来源名映射新增 `codex: 'Codex'`，并补平台色。

### 5.2 工作过程下拉栏（新组件 `CodexWorkPanel.vue`）

- 仅 `platform = codex` 的消息使用；替代"思考"栏，放在最终回答正文上方；标题「工作过程 · N 步」。
- 过程说明消息不作为独立消息块渲染，而是按 `work_seq` 穿插进所在轮的下拉栏。
- 样式：背景透明、无底色、无强调色条；仅用浅分隔线与缩进区分条目。
- 条目渲染：
  - 过程说明：Markdown 正文，参与搜索高亮；
  - 思考：一行「思考 · {title}」，灰色；`encrypted` 时不可展开；有明文 `text` 时可展开，全文渲染不截断；
  - 工具：「{name} {input 首行摘要}」，展开显示输入 / 输出（等宽）；`truncated` 时底部显示「已截断，原文 {X} KB」；
  - 文件变更：「修改 {path} +a −r」，展开显示着色 unified diff；
  - 子代理：「子代理 {label} 已开始工作 / 已更新」，点击切换到子代理会话并在列表中展开父会话；
  - 计划：展开渲染 Markdown。
- 首次展开时调用 `get_codex_work`，结果按 `session_id + turn_id` 缓存在内存。
- 搜索命中过程说明时自动展开对应下拉栏并滚动定位。

### 5.3 设置（`SettingsDialog.vue` 导入分区）

- Codex 目录输入 + 选择按钮、「立即导入」按钮（显示进度与结果数量）、「自动监听 Codex 新对话」开关、上次导入时间与数量。
- 所有文案补中英文 i18n。

### 5.4 其他平台

DeepSeek、Kimi 等保留原"思考"栏与工具调用栏，行为不变。

## 6. 错误处理

- Codex 目录不存在 / 无权限：导入返回明确中文错误，自动监听开关旁显示状态。
- `state_5.sqlite` 被占用或结构不符：降级到 `session_index.jsonl`，不失败。
- 单个 rollout 解析失败：跳过并计入 `failed`，不影响其他会话；不写入 `codex_import_state`，下次重试。
- 工作 JSONL 缺失或损坏：`get_codex_work` 返回空列表并记 warn，下拉栏显示"工作过程不可用"。
- 正在写入的 rollout（Codex 运行中）：只解析到最后一个完整行；size/mtime 变化后下次再导入。

## 7. 测试

Rust（`cargo test --all-features`）：

- fixture：从本机真实 rollout 裁剪并脱敏的小文件——新格式（item_completed）、旧格式（仅 response_item）、子代理、含加密思考、含超 130 KB 工具输出、末行截断的写入中文件；
- 注入片段过滤：AGENTS.md、environment_context、permissions instructions 被剔除，真实用户输入保留；
- 轮次划分、`work_seq` 与 JSONL `seq` 一致；
- 截断在 UTF-8 字符边界且 `original_bytes` 正确；
- 增量：size/mtime 不变不重导，变化后替换式更新、消息不重复；
- 列表排除子代理、`child_count` 正确、搜索命中子代理返回父会话；
- 删除会话同时删除 JSONL。

前端（`npm test`）：

- `CodexWorkPanel` 各条目渲染、加密思考不可展开、截断提示、子代理点击事件；
- 列表项目列、折叠展开、跳转时自动展开父会话；
- 过程说明穿插顺序与搜索命中自动展开。

## 8. 后续（不在本期）

- DeepSeek 同步利用 `history_messages` 的 `cache_version` / `cache_reset_at` 增量（MERGE/REPLACE）与页面 IndexedDB `deepseek-chat/history-message` 缓存，减少网络请求——单独设计。
- `codex-work/` 纳入云同步。本期云同步照常同步 Codex 会话的消息（含过程说明），但 `project` / `parent_platform_session_id` / `agent_label` 三列不随同步；在另一台机器上这些会话显示为无项目的顶层会话，工作过程下拉栏显示"工作过程不可用"。
