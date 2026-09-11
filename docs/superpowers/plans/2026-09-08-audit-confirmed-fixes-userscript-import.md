# 导入与 Userscript 同步完整性实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 保留 DeepSeek 纯思考消息，把 userscript 导入拆成安全大小的请求，并让失败会话跨游标和页面刷新持续重试。

**架构：** Rust normalizer 把 THINK 作为 assistant 的有效内容类型；userscript 新增纯函数 `buildImportBatches` 和版本化 `SyncRetryStore`，SyncCoordinator 把增量候选与重试队列去重合并，逐批导入并在每批结束后持久更新成功/失败状态。

**技术栈：** Rust/serde_json、原生 JavaScript、GM_getValue/GM_setValue、TextEncoder、Node test runner

---

## 文件结构

- 修改 `app/src-tauri/src/normalizer.rs`：纯 THINK 节点变为 assistant 消息；补导出规范化测试。
- 修改 `userscript/dist/ai-chat-memory.user.js`：导入批次纯函数、重试存储、SyncCoordinator 选择/汇总/持久化。
- 修改 `userscript/tests/capture.test.mjs`：UTF-8 分批、部分失败、游标越过重试、刷新恢复和平台隔离测试。

### 任务 1：保留 DeepSeek 纯思考消息

**文件：**
- 修改：`app/src-tauri/src/normalizer.rs:285-365,420-530`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn preserves_thinking_only_deepseek_export_message() {
    let session = normalize_deepseek_export(&json!({
        "id": "conversation",
        "mapping": {
            "node": {
                "id": "node",
                "parent": null,
                "children": [],
                "message": {
                    "inserted_at": 1780853706,
                    "fragments": [{"type": "THINK", "content": "unfinished reasoning"}]
                }
            }
        }
    })).unwrap();
    assert_eq!(session.messages.len(), 1);
    assert_eq!(session.messages[0].role, "assistant");
    assert_eq!(session.messages[0].content, "");
    assert_eq!(session.messages[0].metadata["thinking"], "unfinished reasoning");
    assert_eq!(session.messages[0].metadata["node_id"], "node");
}
```

另加完全空 fragments 仍产生 0 消息的测试。

- [ ] **步骤 2：运行测试验证失败**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml normalizer::tests::preserves_thinking_only_deepseek_export_message -- --nocapture
```

预期：FAIL，messages 长度为 0。

- [ ] **步骤 3：实现最小角色选择变更**

把 role/content 分支改为：

```rust
let (role, content) = if !user.is_empty() {
    ("user", user.join("\n"))
} else if !assistant.is_empty() {
    ("assistant", assistant.join("\n"))
} else if !thinking.is_empty() {
    ("assistant", String::new())
} else {
    continue;
};
```

其余 metadata 和排序不变。

- [ ] **步骤 4：运行 normalizer 测试**

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml normalizer::tests
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/normalizer.rs
git commit -m "fix(import): 保留 DeepSeek 纯思考消息`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 2：按条目数和 UTF-8 字节拆分导入

**文件：**
- 修改：`userscript/dist/ai-chat-memory.user.js:24-45,1490-1740,1950-1975`
- 修改：`userscript/tests/capture.test.mjs:1-40,680-775`

- [ ] **步骤 1：编写分批失败测试**

在 test API 暴露 `buildImportBatches`。添加：

```javascript
test('buildImportBatches respects count and UTF-8 body limits', () => {
  const sessions = [
    { id: 'a', title: '你'.repeat(20) },
    { id: 'b', title: '好'.repeat(20) },
    { id: 'c', title: 'ok' },
  ]
  const { batches, oversized } = api.buildImportBatches('deepseek', sessions, {
    maxItems: 2,
    maxBodyBytes: 100,
  })
  assert.equal(oversized.length, 0)
  assert.ok(batches.every(batch => new TextEncoder().encode(JSON.stringify({ platform: 'deepseek', sessions: batch })).byteLength <= 100))
  assert.deepEqual(batches.flat().map(item => item.id), ['a', 'b', 'c'])
})
```

另测单项超限进入 `oversized` 且其余项继续分批。

- [ ] **步骤 2：运行测试验证失败**

```powershell
node --test --test-name-pattern="buildImportBatches" userscript/tests/capture.test.mjs
```

预期：FAIL，函数未定义。

- [ ] **步骤 3：实现批次纯函数与配置**

在 RuntimeConfig 加：

```javascript
importBatchMaxItems: 100,
importBatchMaxBodyBytes: 19 * 1024 * 1024,
```

实现 `encodedImportBody(platform, sessions)` 返回 `{ body, bytes }`，`buildImportBatches` 按追加候选后的完整 envelope 字节判定；单项超限进入 `oversized`。严禁用字符串 length 代替 `TextEncoder().encode(body).byteLength`。

- [ ] **步骤 4：改造 fetchDetailsAndPush 逐批发送**

先保留现有详情抓取重试；对成功详情调用 `buildImportBatches`。每批调用 `bridge.request('/sessions/import', ...)`，将成功响应的 imported/skipped 累加，将成功会话 id 放入 `succeededIds`；HTTP 失败时把该批全部放入 failed。返回结构保持 `{ imported, skipped, sessions, failed }`，其中 `sessions` 是成功导入或后端明确跳过的会话数。

- [ ] **步骤 5：运行分批与既有重试测试**

```powershell
node --test --test-name-pattern="buildImportBatches|fetchDetailsAndPush" userscript/tests/capture.test.mjs
```

预期：全部 PASS，包括既有 attempts 和返回结构断言。

- [ ] **步骤 6：Commit**

```powershell
git add userscript/dist/ai-chat-memory.user.js userscript/tests/capture.test.mjs
git commit -m "fix(userscript): 分批发送会话导入请求`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 3：持久化失败重试队列并与增量候选合并

**文件：**
- 修改：`userscript/dist/ai-chat-memory.user.js:24-45,1490-1790,1950-1975`
- 修改：`userscript/tests/capture.test.mjs`

- [ ] **步骤 1：编写重试存储与候选合并失败测试**

为 SyncCoordinator 注入 `getValue/setValue`（默认 GM API）。测试第一轮会话 A 失败、B 成功且 status 游标推进到 B；重新创建 coordinator 模拟刷新，服务列表不再返回 A，但选择结果仍包含持久队列中的 A。再测 deepseek 队列不会出现在 kimi coordinator。

测试断言形态：

```javascript
const retried = await reloaded.selectSessions('200')
assert.deepEqual(retried.map(item => item.id), ['failed-a'])
assert.equal(storage.get('sync_retry_queue_v1:deepseek').entries['failed-a'].attempts, 1)
assert.equal(storage.get('sync_retry_queue_v1:kimi', null), null)
```


- [ ] **步骤 2：运行测试验证失败**

```powershell
node --test --test-name-pattern="retry queue|cursor advances past failures|platform retry isolation" userscript/tests/capture.test.mjs
```

预期：FAIL，失败 A 被 `last_updated_at` 过滤并在刷新后丢失。

- [ ] **步骤 3：实现版本化 SyncRetryStore**

```javascript
class SyncRetryStore {
  constructor(platform, getValue, setValue) {
    this.key = `sync_retry_queue_v1:${platform}`
    this.getValue = getValue
    this.setValue = setValue
  }
  load() {
    const value = this.getValue(this.key, null)
    return value?.version === 1 && value.entries && typeof value.entries === 'object'
      ? value.entries : {}
  }
  save(entries) { this.setValue(this.key, { version: 1, entries }) }
  upsert(session, stage, message) { /* 保存 id、updated_at、stage、attempts、message */ }
  remove(ids) { /* 删除成功 id 并立即 save */ }
  sessions() { return Object.values(this.load()).map(entry => entry.session) }
}
```

错误 message 截断到 500 字符；不得保存会话正文详情，只保存列表级 session 元数据。

- [ ] **步骤 4：把队列接入 selectSessions 和批次结果**

`selectSessions` 无论是否有 cursor，都把正常候选与 `retryStore.sessions()` 按稳定 id 去重合并。详情抓取失败、单会话超限、批 HTTP 失败分别以 stage `detail`/`oversized`/`import` upsert；后端成功导入或明确 skipped 的 id 才 remove。每批结束立即 save。

`run()` 仍可读取后端 status 的 `last_updated_at`；游标推进不再承担失败保留职责。确保重试项不受 cursor 比较。

- [ ] **步骤 5：运行 userscript 全套测试**

```powershell
node --check userscript/dist/ai-chat-memory.user.js
node --test userscript/tests/capture.test.mjs
```

预期：0 failed；刷新恢复、平台隔离、去重和成功移除全部通过。

- [ ] **步骤 6：Commit**

```powershell
git add userscript/dist/ai-chat-memory.user.js userscript/tests/capture.test.mjs
git commit -m "fix(userscript): 持久重试游标遗漏会话`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 4：批次 3 验证与审查循环

**文件：**
- 审查：`normalizer.rs`、userscript 和 Node 测试

- [ ] **步骤 1：运行批次回归**

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --check
cargo test --manifest-path app/src-tauri/Cargo.toml normalizer::tests
node --check userscript/dist/ai-chat-memory.user.js
node --test userscript/tests/capture.test.mjs
```

预期：全部退出 0。

- [ ] **步骤 2：执行高强度审查**

重点检查：纯 THINK 是否误保留空工具节点；批次字节是否包含 envelope；部分成功统计是否重复；队列是否持久化正文；后端 skipped 是否正确移除；平台/刷新/去重是否可靠。

- [ ] **步骤 3：验证并修复发现**

对 CONFIRMED 项补红测后修复，重跑步骤 1 并重复审查直到无确认遗留项。

- [ ] **步骤 4：提交审查修复（如有）**

```powershell
git add <逐条列出本批审查修复文件>
git commit -m "fix(import): 收敛同步完整性审查问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```
