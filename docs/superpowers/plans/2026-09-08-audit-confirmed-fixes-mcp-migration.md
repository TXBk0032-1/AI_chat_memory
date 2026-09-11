# MCP 数据目录迁移一致性实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 桌面端迁移数据库后，让仍持有旧 SQLite 池的 MCP stdio 进程立即拒绝所有数据读取并提示重启。

**架构：** 在旧数据目录写原子、版本化 redirect marker；AppService 保存当前数据库目录并提供统一 `ensure_current_data_directory` guard。MCP tools、读取型 prompts 和 resources 在入口先调用 guard；标记路径仅用于提示，不自动换池，重启后仍由现有 SettingsStore 选择新目录。

**技术栈：** Rust、Tokio fs、serde_json、sqlx SQLite、rmcp

---

## 文件结构

- 创建 `app/src-tauri/src/data_directory_marker.rs`：marker schema、固定文件名、原子发布、读取与损坏失败关闭。
- 修改 `app/src-tauri/src/lib.rs`：注册 marker 模块。
- 修改 `app/src-tauri/src/service.rs`：AppService 保存 `data_dir`；迁移成功后发布 marker；提供 guard。
- 修改 `app/src-tauri/src/service/tests/cloud_backend_transition.rs`：更新 fixture 字段，测试标记发布/失败条件和重启新目录。
- 修改 `app/src-tauri/src/mcp/server.rs`：所有数据读取入口共用 guard；补 tool/prompt/resource 测试。

### 任务 1：实现原子迁移标记

**文件：**
- 创建：`app/src-tauri/src/data_directory_marker.rs`
- 修改：`app/src-tauri/src/lib.rs`

- [ ] **步骤 1：编写 marker 失败测试**

定义完整数据结构和 API 骨架：

```rust
pub(crate) const DATA_DIRECTORY_REDIRECT_FILE: &str = ".ai-chat-memory-data-moved.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DataDirectoryRedirect {
    pub version: u32,
    pub moved_at: String,
    pub destination_hint: String,
}

pub(crate) async fn publish_redirect(old_dir: &Path, destination: &Path) -> Result<()> { unimplemented!() }
pub(crate) async fn read_redirect(data_dir: &Path) -> Result<Option<DataDirectoryRedirect>> { unimplemented!() }
```

测试：发布后解析 version=1；目录中无 tmp；损坏 JSON 返回 `AppError::InvalidData` 而非 `None`；不存在返回 None。

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml data_directory_marker::tests -- --nocapture
```

预期：FAIL，API 未实现。

- [ ] **步骤 3：实现原子发布与严格读取**

发布流程：在 old_dir 创建 UUID 临时文件，`BufWriter.write_all`、`flush`、`sync_all`，然后 rename 到固定 marker。payload：version=1、`Utc::now().to_rfc3339()`、destination 的 display 字符串。rename 失败删除 tmp 并返回错误。

读取：NotFound → `Ok(None)`；其他 IO 错误传播；JSON 错误转 `AppError::InvalidData("数据目录迁移标记损坏，请重启 MCP 并检查数据目录")`；version != 1 同样失败关闭。

- [ ] **步骤 4：运行 marker 测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml data_directory_marker::tests
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/data_directory_marker.rs app/src-tauri/src/lib.rs
git commit -m "feat(mcp): 增加原子数据目录迁移标记`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 2：迁移完成后发布 marker 并提供 service guard

**文件：**
- 修改：`app/src-tauri/src/service.rs:251-350,1912-1945`
- 修改：`app/src-tauri/src/service/tests/cloud_backend_transition.rs:45-110,3730-3835`

- [ ] **步骤 1：编写 service 迁移失败测试**

fixture 返回 old data_dir。新增测试：迁移前 `ensure_current_data_directory()` Ok；迁移成功后 marker 存在且 guard 返回包含“数据目录已迁移，请重启 MCP”；目的数据库存在；新目录构建的 service guard Ok。另测试 destination 已有 db 导致迁移失败时 old dir 无 marker。

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml service::tests::cloud_backend_transition::move_data_directory_publishes_redirect_for_old_readers -- --nocapture
```

预期：编译 FAIL，AppService 无 data_dir/guard。

- [ ] **步骤 3：给 AppService 保存当前 data_dir**

新增字段：

```rust
data_dir: Arc<PathBuf>,
```

在 `build` 构造时 clone；所有测试字面量 fixture 补字段。使用 Arc 保持 Clone 便宜。

- [ ] **步骤 4：实现统一 guard**

```rust
pub async fn ensure_current_data_directory(&self) -> Result<()> {
    match crate::data_directory_marker::read_redirect(&self.data_dir).await? {
        None => Ok(()),
        Some(_) => Err(AppError::Cancelled("数据目录已迁移，请重启 MCP 后重试".into())),
    }
}
```

不要在 `list/open_session/session_messages/session_search_hits` 内自动调用，避免改变桌面进程迁移后的内部读行为；guard 由 MCP 边界统一调用。

- [ ] **步骤 5：在 move_data_directory 最后发布 marker**

顺序必须是：VACUUM 成功 → settings 成功 → `publish_redirect(old, destination)` 成功 → shutdown=true → 返回 Ok。marker 发布失败时返回 Err 且不设置 shutdown。新数据库快照与 settings 已存在，此失败会阻止“迁移完全成功”的 UI 声明，下一次操作仍可诊断；不得发布空/半写 marker。

- [ ] **步骤 6：运行迁移测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml service::tests::cloud_backend_transition::move_data_directory_ -- --nocapture
```

预期：全部 PASS。

- [ ] **步骤 7：Commit**

```powershell
git add app/src-tauri/src/service.rs app/src-tauri/src/service/tests/cloud_backend_transition.rs
git commit -m "fix(service): 迁移后标记旧数据目录失效`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 3：保护全部 MCP 数据读取入口

**文件：**
- 修改：`app/src-tauri/src/mcp/server.rs:100-390,600-820`

- [ ] **步骤 1：编写 tools、prompts、resources guard 失败测试**

在 MCP 测试 fixture 的 data_dir 写有效 marker，然后通过 ServerHandler/router 实际入口测试：

- `search_sessions`、`open_session`、`get_messages`、`search_in_session` 均返回 tool error 且含“请重启 MCP”；
- `summarize-session`（会读会话）返回 MCP error；`find-memories` 不读数据库，保持可生成静态提示词；
- `sessions://recent`、`session://id`、`session://id/messages` 均返回 MCP error；
- `list_resources` 和 `list_resource_templates` 只列元数据，不访问数据库，保持可用。

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml mcp::server::tests::redirect_marker_blocks_all_mcp_data_reads -- --nocapture
```

预期：FAIL，旧 MCP 仍返回旧数据库内容。

- [ ] **步骤 3：添加 MCP 边界 helper**

```rust
async fn ensure_current_data(&self) -> std::result::Result<(), AppError> {
    self.service.ensure_current_data_directory().await
}
```

每个 tool 在参数规范化后、数据库调用前执行；错误转 `Ok(tool_error(format_app_error(&err)))`。`summarize_session` 和 `read_resource` 使用 `resource_mcp_error`。确保每条分支只调用一次 guard。

- [ ] **步骤 4：运行 MCP 测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml mcp::server::tests
```

预期：全部 PASS；静态列表和 find-memories 仍可用。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/mcp/server.rs
git commit -m "fix(mcp): 拒绝读取已迁移的旧数据库`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 4：批次 5 验证与审查循环

**文件：**
- 审查：marker、service、service tests、MCP server

- [ ] **步骤 1：运行批次回归**

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path app/src-tauri/Cargo.toml data_directory_marker::tests
cargo test --manifest-path app/src-tauri/Cargo.toml service::tests::cloud_backend_transition::move_data_directory_
cargo test --manifest-path app/src-tauri/Cargo.toml mcp::server::tests
```

预期：全部退出 0。

- [ ] **步骤 2：执行高强度审查**

重点检查：marker 是否只在快照+settings 成功后发布；写入是否原子；损坏 marker 是否失败关闭；是否遗漏任何 MCP 数据读取入口；是否意外让 MCP 信任 destination_hint 自动换池；重启后的新目录是否不受旧 marker 影响。

- [ ] **步骤 3：验证并修复发现**

对 CONFIRMED 项补红测后修复，重跑步骤 1 并重复审查至无确认遗留项。

- [ ] **步骤 4：提交审查修复（如有）**

```powershell
git add <逐条列出本批审查修复文件>
git commit -m "fix(mcp): 收敛目录迁移审查问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```
