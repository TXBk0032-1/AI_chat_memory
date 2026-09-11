# Windows 路径与本地持久化实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 阻断 Windows 8.3 路径与字符串前缀绕过，保护设置备份自愈，统一 SQLite 池连接 PRAGMA，并避免清理仍在写入的临时文件。

**架构：** 把 Windows 路径标准化和边界判断抽到专用模块，文件/目录命令共用一个安全策略；SettingsStore 的 persist 显式携带备份策略，自愈只替换主文件；连接选项成为每条池连接的唯一 PRAGMA 来源；临时清理只处理超过年龄阈值的本命名空间文件。

**技术栈：** Rust、Windows std::fs canonicalize、Tokio fs、sqlx SQLite、filetime（仅 dev-dependency）

---

## 文件结构

- 创建 `app/src-tauri/src/safe_path.rs`：词法规范化、最长已存在祖先 canonicalize、Windows 大小写无关组件边界判断和保护目录策略。
- 修改 `app/src-tauri/src/lib.rs`：注册 `safe_path` 模块。
- 修改 `app/src-tauri/src/commands.rs`：文件与目录验证共用 `safe_path`，保留扩展名/保留设备名规则。
- 修改 `app/src-tauri/src/settings.rs`：`BackupPolicy`、恢复时保留备份、5 分钟清理阈值。
- 修改 `app/src-tauri/src/database/connection.rs`：PRAGMA 移到 `SqliteConnectOptions` 并补多连接测试。
- 修改 `app/src-tauri/Cargo.toml`、`app/src-tauri/Cargo.lock`：固定 dev-dependency `filetime = "0.2.26"`，只用于可靠设置测试时间戳。

### 任务 1：统一 Windows 安全路径解析

**文件：**
- 创建：`app/src-tauri/src/safe_path.rs`
- 修改：`app/src-tauri/src/lib.rs`
- 修改：`app/src-tauri/src/commands.rs:317-570,770-815`
- 测试：`app/src-tauri/src/safe_path.rs`

- [ ] **步骤 1：编写边界与已存在祖先失败测试**

在新模块建立以下公共接口骨架：

```rust
pub(crate) fn resolve_for_validation(path: &std::path::Path) -> Result<std::path::PathBuf, String> { unimplemented!() }
pub(crate) fn path_is_within(path: &std::path::Path, root: &std::path::Path) -> bool { unimplemented!() }
pub(crate) fn validate_writable_destination(path: &std::path::Path) -> Result<std::path::PathBuf, String> { unimplemented!() }
```

添加平台无关边界测试和 Windows 条件测试：

```rust
#[test]
fn component_boundary_does_not_match_sibling_prefix() {
    assert!(!path_is_within(Path::new(r"C:\Temporary\x"), Path::new(r"C:\Temp")));
    assert!(!path_is_within(Path::new(r"C:\Temp_Evil\x"), Path::new(r"C:\Temp")));
    assert!(path_is_within(Path::new(r"C:\Temp\x"), Path::new(r"C:\Temp")));
}

#[test]
fn preserves_nonexistent_tail_after_resolving_existing_ancestor() {
    let root = std::env::temp_dir().join(format!("acm-safe-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let result = resolve_for_validation(&root.join("new/child/file.md")).unwrap();
    assert!(result.ends_with(Path::new("new/child/file.md")));
}
```

Windows 测试通过 `std::fs::canonicalize` 对可用的短路径 fixture 或系统返回别名进行验证；无法取得 8.3 别名时打印明确 skip 并 return。

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml safe_path::tests -- --nocapture
```

预期：FAIL，骨架 `unimplemented!()`。

- [ ] **步骤 3：实现最长已存在祖先解析**

实现算法：拒绝相对路径、`..`、UNC/设备命名空间；从目标向父级回退直到 `exists()`，对该祖先 `std::fs::canonicalize`，再按原顺序拼回不存在尾段。Windows 下把 `\?\` disk 前缀规范为普通盘符路径，并把每个组件转小写后比较；非 Windows 保持 `Path::starts_with` 语义。

核心循环应等价于：

```rust
let mut ancestor = normalized.as_path();
let mut tail = Vec::new();
while !ancestor.exists() {
    let name = ancestor.file_name().ok_or_else(|| "路径没有可解析的已存在祖先".to_string())?;
    tail.push(name.to_os_string());
    ancestor = ancestor.parent().ok_or_else(|| "路径没有可解析的已存在祖先".to_string())?;
}
let mut resolved = std::fs::canonicalize(ancestor)
    .map_err(|e| format!("无法解析目标路径：{e}"))?;
for component in tail.into_iter().rev() { resolved.push(component); }
```

`validate_writable_destination` 用 `path_is_within` 检查 TEMP/TMP、Windows、Program Files、ProgramData、AppData、其他用户目录和 Startup；temp 例外也必须组件边界匹配。

- [ ] **步骤 4：替换 commands 中重复字符串校验**

`validate_file_path` 保留文件扩展名与设备名检查，然后调用 `validate_writable_destination`；`validate_directory_path` 直接调用同一函数。返回值必须是解析并拼回尾段后的路径，而不是原始短路径。

- [ ] **步骤 5：运行路径测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml safe_path::tests -- --nocapture
cargo test --manifest-path app/src-tauri/Cargo.toml commands::tests -- --nocapture
```

预期：全部 PASS；Windows 上短路径指向 Program Files/AppData 时被拒，相邻前缀目录不被误判为 TEMP。

- [ ] **步骤 6：Commit**

```powershell
git add app/src-tauri/src/safe_path.rs app/src-tauri/src/lib.rs app/src-tauri/src/commands.rs
git commit -m "fix(path): 解析 Windows 别名并按组件校验`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 2：保护备份恢复并只清理陈旧临时文件

**文件：**
- 修改：`app/src-tauri/src/settings.rs:35-145,230-315,340-510`
- 修改：`app/src-tauri/Cargo.toml`
- 修改：`app/src-tauri/Cargo.lock`

- [ ] **步骤 1：固定测试时间依赖并编写失败测试**

在 `[dev-dependencies]` 添加精确版本：

```toml
filetime = "=0.2.26"
```

扩充恢复测试：在损坏 main 后保存 `.bak` 原始字节，load 完成后断言 `.bak` 字节完全不变且仍可解析。新增清理年龄测试：创建 `settings.json.tmp-fresh` 和 `settings.json.tmp-stale`，用 `filetime::set_file_mtime` 把 stale 调到 6 分钟前，触发一次 update，断言 fresh 存在、stale 删除、`other.tmp-stale` 保留。

- [ ] **步骤 2：运行设置测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml settings::tests::recovers_corrupt_settings_from_backup_and_rewrites_it -- --nocapture
cargo test --manifest-path app/src-tauri/Cargo.toml settings::tests::cleanup_removes_only_old_settings_temporaries -- --nocapture
```

预期：第一项 FAIL，因为 `.bak` 被 `{ corrupt` 覆盖；第二项 FAIL，因为 fresh 也被删除。

- [ ] **步骤 3：实现显式备份策略**

在 settings 模块定义：

```rust
#[derive(Clone, Copy)]
enum BackupPolicy { RotateValidatedMain, PreserveExisting }
```

把 `persist` 改为 `persist_with_policy(value, policy)`；普通 `update`、秘密迁移调用 `RotateValidatedMain`，`recovered_from_backup` 自愈调用 `PreserveExisting`。只有 Rotate 分支才复制 main 到 `.bak`。保留现有 tmp sync_all + rename 原子写。

- [ ] **步骤 4：实现 5 分钟年龄阈值**

```rust
const STALE_TEMPORARY_AGE: std::time::Duration = std::time::Duration::from_secs(5 * 60);
```

对前缀匹配文件读取 metadata.modified；使用 `SystemTime::now().duration_since(modified).is_ok_and(|age| age >= threshold)` 决定删除。任何 metadata/modified 错误和未来时间均保留。

- [ ] **步骤 5：运行全部 settings 测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml settings::tests
```

预期：全部 PASS；备份字节未改变，fresh 临时文件保留。

- [ ] **步骤 6：Commit**

```powershell
git add app/src-tauri/src/settings.rs app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock
git commit -m "fix(settings): 保全恢复备份并延迟清理临时文件`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 3：让池中每条 SQLite 连接继承 PRAGMA

**文件：**
- 修改：`app/src-tauri/src/database/connection.rs:35-62,730-790`

- [ ] **步骤 1：编写多连接失败测试**

```rust
#[tokio::test]
async fn every_pooled_connection_inherits_connection_pragmas() {
    let root = std::env::temp_dir().join(format!("acm-pool-pragmas-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&root).await.unwrap();
    let pool = connect(&root.join("db.sqlite")).await.unwrap();
    let mut held = Vec::new();
    for _ in 0..5 { held.push(pool.acquire().await.unwrap()); }
    for connection in &mut held {
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous").fetch_one(&mut **connection).await.unwrap();
        let temp_store: i64 = sqlx::query_scalar("PRAGMA temp_store").fetch_one(&mut **connection).await.unwrap();
        assert_eq!(synchronous, 1); // NORMAL
        assert_eq!(temp_store, 2); // MEMORY
    }
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode").fetch_one(&pool).await.unwrap();
    assert_eq!(journal.to_ascii_lowercase(), "wal");
}
```

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml database::connection::tests::every_pooled_connection_inherits_connection_pragmas -- --nocapture
```

预期：FAIL，至少后建连接的 `synchronous` 或 `temp_store` 是默认值。

- [ ] **步骤 3：把 PRAGMA 移入连接选项**

```rust
let options = SqliteConnectOptions::new()
    .filename(path)
    .create_if_missing(true)
    .foreign_keys(true)
    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
    .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
    .pragma("temp_store", "MEMORY")
    .busy_timeout(Duration::from_secs(5));
```

删除建池后 3 个独立 PRAGMA query，避免两套配置来源。

- [ ] **步骤 4：运行数据库连接测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml database::connection::tests
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/database/connection.rs
git commit -m "fix(database): 为所有池连接配置 SQLite PRAGMA`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 4：批次 2 验证与审查循环

**文件：**
- 审查：`safe_path.rs`、`commands.rs`、`settings.rs`、`database/connection.rs` 和依赖锁文件

- [ ] **步骤 1：运行批次回归**

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path app/src-tauri/Cargo.toml safe_path::tests
cargo test --manifest-path app/src-tauri/Cargo.toml commands::tests
cargo test --manifest-path app/src-tauri/Cargo.toml settings::tests
cargo test --manifest-path app/src-tauri/Cargo.toml database::connection::tests
```

预期：全部退出 0。

- [ ] **步骤 2：执行高强度审查**

重点检查：canonicalize 后尾段是否可重新引入 `..`；路径比较是否按组件而非字符串；恢复写失败时 `.bak` 是否原样；未来 mtime 是否保留；每条新 SQLite 连接是否继承设置。

- [ ] **步骤 3：验证并修复发现**

对 CONFIRMED 项先补红测、再最小修复；重跑步骤 1 并重复审查直到无确认遗留项。

- [ ] **步骤 4：提交审查修复（如有）**

```powershell
git add <逐条列出本批审查修复文件>
git commit -m "fix(storage): 收敛路径与持久化审查问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```
