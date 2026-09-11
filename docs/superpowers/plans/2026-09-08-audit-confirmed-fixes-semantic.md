# 语义搜索可靠性与确定性实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 修复单毒丸无限重试、后台取消污染前台查询、HTTP 请求不可取消、BGE 截断丢 SEP 和 RRF 平局抖动。

**架构：** 新增可克隆的代次取消源/票据，EmbeddingManager 为每轮后台工作轮换票据并把当前票据传给本地和 HTTP 后端；交互查询不消费后台取消。SemanticEngine 用健康 canary 区分内容毒丸与后端故障，同时保留 generation 作为陈旧写入屏障；BGE token 截断和 RRF 排序保持为纯函数，便于边界测试。

**技术栈：** Rust、Tokio watch/select、async-trait、reqwest、tokenizers、sqlx、sqlite-vec

---

## 文件结构

- 创建 `app/src-tauri/src/embedding/cancellation.rs`：定义 `CancellationSource` 和 `CancellationToken`，只负责工作代次失效与等待。
- 修改 `app/src-tauri/src/embedding/mod.rs`：导出取消类型；扩展 `EmbeddingBackend` 方法签名；EmbeddingManager 轮换后台票据并构建带共享取消源的后端。
- 修改 `app/src-tauri/src/embedding/local.rs`：Harrier 文档嵌入检查传入票据；查询绕过后台取消；下载仍使用显式取消票据。
- 修改 `app/src-tauri/src/embedding/bge.rs`：BGE 文档嵌入检查票据；抽取保留 SEP 的截断纯函数。
- 修改 `app/src-tauri/src/embedding/http.rs`：每个 send/json future 与取消票据 `select!`；查询传 `None`。
- 修改 `app/src-tauri/src/embedding/mock.rs`：适配 trait 新签名。
- 修改 `app/src-tauri/src/semantic/engine.rs`：每次 drain 捕获一个后台票据；canary 分类；更新取消与毒丸测试。
- 修改 `app/src-tauri/src/semantic/index.rs`：RRF 使用总序和 id 决胜；补稳定性测试。

### 任务 1：实现不粘滞的取消代次

**文件：**
- 创建：`app/src-tauri/src/embedding/cancellation.rs`
- 修改：`app/src-tauri/src/embedding/mod.rs:1-190`
- 修改：`app/src-tauri/src/embedding/local.rs:200-280`
- 修改：`app/src-tauri/src/embedding/bge.rs:200-230,637-655`
- 修改：`app/src-tauri/src/embedding/mock.rs`
- 测试：`app/src-tauri/src/embedding/cancellation.rs`
- 测试：`app/src-tauri/src/semantic/engine.rs:1360-1410`

- [ ] **步骤 1：编写取消代次失败测试**

在新文件先写类型骨架和以下测试；生产方法可用 `unimplemented!()` 使测试先编译后失败：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_one_generation_does_not_cancel_the_next() {
        let source = CancellationSource::new();
        let first = source.token();
        source.cancel_current();
        assert!(first.is_cancelled());
        let second = source.begin_work();
        assert!(!second.is_cancelled());
        assert!(first.is_cancelled(), "starting new work must not revive old work");
    }

    #[tokio::test]
    async fn cancelled_waiter_wakes() {
        let source = CancellationSource::new();
        let token = source.token();
        source.cancel_current();
        tokio::time::timeout(std::time::Duration::from_millis(100), token.cancelled())
            .await
            .expect("cancelled token must wake")
    }
}
```

同时把 `engine.rs` 旧的 `cancel_flag_resets_for_incremental_and_drained_work` 改为行为测试：取消旧 token 后，`request_session_index` 得到新 token，旧 token 保持取消，且 `embed_query` 仍调用测试后端。

- [ ] **步骤 2：运行测试验证失败**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::cancellation::tests -- --nocapture
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::cancelled_background_generation_does_not_cancel_query_or_new_work -- --nocapture
```

预期：FAIL；第一组因 `unimplemented!()`，第二项因新工作会清除同一布尔值、使旧工作被重新放行或查询被粘滞标志拒绝。

- [ ] **步骤 3：实现取消源和票据**

在 `cancellation.rs` 实现以下完整接口：

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

#[derive(Clone)]
pub struct CancellationSource {
    generation: Arc<AtomicU64>,
    changed: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct CancellationToken {
    generation: u64,
    current: Arc<AtomicU64>,
    changed: watch::Receiver<u64>,
}

impl CancellationSource {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self { generation: Arc::new(AtomicU64::new(0)), changed }
    }

    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            generation: self.generation.load(Ordering::SeqCst),
            current: Arc::clone(&self.generation),
            changed: self.changed.subscribe(),
        }
    }

    pub fn cancel_current(&self) {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.changed.send_replace(next);
    }

    pub fn begin_work(&self) -> CancellationToken { self.token() }
}

impl CancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.current.load(Ordering::SeqCst) != self.generation
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() { return; }
        let mut changed = self.changed.clone();
        while changed.changed().await.is_ok() {
            if self.is_cancelled() { return; }
        }
    }
}
```

注意：`begin_work()` 不递增 generation；只有 `cancel_current()` 使已发票据失效。取消后的新 token 读取新 generation，因此不会被旧取消污染。

- [ ] **步骤 4：把票据传播到 embedding trait**

把 trait 文档嵌入签名统一为：

```rust
async fn embed_documents(
    &self,
    texts: &[String],
    cancellation: Option<&CancellationToken>,
) -> Result<Vec<Vec<f32>>>;
async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
```

`EmbeddingManager` 保存 `CancellationSource`，提供：

```rust
pub fn background_token(&self) -> CancellationToken { self.cancellation.begin_work() }
pub fn request_cancel(&self) { self.cancellation.cancel_current(); }
```

删除 `clear_cancel`、`cancel_flag` 与所有把同一 `AtomicBool` 清回 false 的调用。`build_backend` 将共享取消源用于模型下载；文档编码读取调用方 token，查询编码不检查后台 token。同步更新 Harrier、BGE、HTTP、Mock 和 engine 测试后端的 trait 实现。

- [ ] **步骤 5：运行取消测试和语义回归**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::cancellation::tests
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::cancelled_background_generation_does_not_cancel_query_or_new_work
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests
```

预期：全部 PASS；旧票据不会复活，查询和新 drain 均可工作。

- [ ] **步骤 6：Commit**

```powershell
git add app/src-tauri/src/embedding/cancellation.rs app/src-tauri/src/embedding/mod.rs app/src-tauri/src/embedding/local.rs app/src-tauri/src/embedding/bge.rs app/src-tauri/src/embedding/http.rs app/src-tauri/src/embedding/mock.rs app/src-tauri/src/semantic/engine.rs
git commit -m "fix(semantic): 用代次票据隔离后台取消`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 2：让 HTTP 嵌入请求响应取消

**文件：**
- 修改：`app/src-tauri/src/embedding/http.rs:12-245`
- 测试：`app/src-tauri/src/embedding/http.rs:340-500`

- [ ] **步骤 1：编写阻塞服务器取消测试**

在测试模块添加只接受请求但不响应的本地 TCP fixture，并测试文档请求：

```rust
#[tokio::test]
async fn document_request_returns_cancelled_before_http_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
    let backend = HttpEmbeddingBackend::openai_compatible(
        EmbeddingBackendKind::OpenaiCompatible,
        remote_settings(&format!("http://{addr}/v1"), None),
    ).unwrap();
    let source = CancellationSource::new();
    let token = source.token();
    let request = backend.embed_documents(&["blocked".into()], Some(&token));
    tokio::pin!(request);
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    source.cancel_current();
    let error = tokio::time::timeout(std::time::Duration::from_millis(250), request)
        .await.expect("cancel must beat 60 second timeout").unwrap_err();
    assert!(matches!(error, AppError::Cancelled(_)));
    accepted.abort();
}
```

再加入一个立即返回合法 JSON 的 fixture，断言未取消请求仍能得到向量。

- [ ] **步骤 2：运行测试验证失败**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::http::tests::document_request_returns_cancelled_before_http_timeout -- --nocapture
```

预期：FAIL/timeout，因为 reqwest send 未监听 token。

- [ ] **步骤 3：实现统一可取消 future helper**

在 `http.rs` 添加：

```rust
async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T>>,
    cancellation: Option<&CancellationToken>,
) -> Result<T> {
    let Some(token) = cancellation else { return future.await; };
    tokio::select! {
        biased;
        _ = token.cancelled() => Err(AppError::Cancelled("远程编码已取消".into())),
        result = future => result,
    }
}
```

将 `embed`、`embed_ollama`、`embed_ollama_batch`、`embed_openai` 接收 `Option<&CancellationToken>`；对每个 `.send()` 和 `.json()` 的映射 future 用 helper 包裹。Ollama per-text 循环每轮开始先检查 `token.is_cancelled()`。`embed_documents` 传调用方 token，`embed_queries` 传 `None`。

- [ ] **步骤 4：运行 HTTP 测试验证通过**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::http::tests
```

预期：全部 PASS，包括取消在 250ms 内返回和普通响应成功。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/embedding/http.rs
git commit -m "fix(embedding): 支持取消阻塞的远程请求`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 3：用 canary 解除单毒丸队列阻塞

**文件：**
- 修改：`app/src-tauri/src/semantic/engine.rs:698-850`
- 测试：`app/src-tauri/src/semantic/engine.rs:1190-1370`

- [ ] **步骤 1：改写毒丸测试并增加后端故障测试**

把现有测试的“单块不烧额度”预期改为：连续 drain 三次后单毒丸状态为 `error`。为测试后端约定 `"semantic canary"` 总能成功，而 `"poison"` 失败。另加总故障后端，所有输入（包括 canary）都失败：

```rust
#[tokio::test]
async fn lone_poison_chunk_is_quarantined_after_three_content_failures() {
    // 插入唯一 poison chunk，三次 drain；前两次 Err，第三次 Ok。
    // 最终 assert_eq!(chunk_state(&pool, 1).await.0, "error");
}

#[tokio::test]
async fn backend_outage_canary_does_not_consume_chunk_failure_budget() {
    // 使用 OutageBackend 连续 drain 三次均 Err。
    // chunk 仍 pending，engine.chunk_failures 不含该 id。
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::lone_poison_chunk_is_quarantined_after_three_content_failures -- --nocapture
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::backend_outage_canary_does_not_consume_chunk_failure_budget -- --nocapture
```

预期：第一项 FAIL（单块永远不计数）；第二项可先通过或因尚无 canary 观察点失败，必须确认测试确实断言 failure map 为空。

- [ ] **步骤 3：实现 canary 分类**

在 engine 常量区定义：

```rust
const EMBEDDING_HEALTH_CANARY: &str = "semantic canary";
```

在 `survivors.is_empty()` 分支执行：

```rust
let canary = [EMBEDDING_HEALTH_CANARY.to_owned()];
match backend.embed_documents(&canary, Some(&cancellation)).await {
    Ok(vectors) if vectors.len() == 1 => {
        self.record_chunk_failures(&failures).await;
        if failures.iter().any(|(id, _)| self.chunk_failure_is_active(*id)) {
            return Err(error);
        }
        continue;
    }
    Ok(_) => return Err(AppError::InvalidData("embedding canary returned no vector".into())),
    Err(canary_error) => {
        tracing::warn!(%canary_error, "embedding canary failed; treating batch failure as backend outage");
        return Err(error);
    }
}
```

实现只读 helper `chunk_failure_is_active`，其含义是该 id 仍在计数 map 中；达到阈值并隔离后 `record_chunk_failures` 应移除 id。canary 不写数据库。

- [ ] **步骤 4：运行毒丸和 engine 全测试**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::lone_poison_chunk_is_quarantined_after_three_content_failures
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests::backend_outage_canary_does_not_consume_chunk_failure_budget
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::engine::tests
```

预期：全部 PASS；混合 poison 测试仍证明健康块可入库。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/semantic/engine.rs
git commit -m "fix(semantic): 用健康探针隔离单毒丸块`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 4：BGE 截断保留特殊结束符

**文件：**
- 修改：`app/src-tauri/src/embedding/bge.rs:20-35,665-705`
- 测试：`app/src-tauri/src/embedding/bge.rs` 的 `tests` 模块

- [ ] **步骤 1：编写截断纯函数失败测试**

```rust
#[test]
fn truncation_preserves_cls_and_sep_at_the_limit() {
    let ids = vec![101, 10, 11, 12, 13, 102];
    assert_eq!(truncate_with_sep(ids, 4, 102), vec![101, 10, 11, 102]);
}

#[test]
fn truncation_does_not_change_short_input() {
    let ids = vec![101, 10, 102];
    assert_eq!(truncate_with_sep(ids.clone(), 4, 102), ids);
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::bge::tests::truncation_ -- --nocapture
```

预期：编译 FAIL，`truncate_with_sep` 未定义。

- [ ] **步骤 3：实现最小截断函数并读取 tokenizer SEP id**

```rust
fn truncate_with_sep(mut ids: Vec<u32>, max_len: usize, sep_id: u32) -> Vec<u32> {
    if ids.len() > max_len {
        ids.truncate(max_len);
        if let Some(last) = ids.last_mut() { *last = sep_id; }
    }
    ids
}
```

在 `embed_single_batch` 中从 tokenizer vocabulary 查询 `"[SEP]"`；缺失时返回 `AppError::Configuration("tokenizer is missing [SEP] token")`。把原 `ids.truncate(MAX_SEQUENCE_LEN)` 替换为该函数。不要硬编码 102。

- [ ] **步骤 4：运行 BGE 测试**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::bge::tests
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/embedding/bge.rs
git commit -m "fix(embedding): BGE 截断保留 SEP 标记`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 5：稳定 RRF 平局排序

**文件：**
- 修改：`app/src-tauri/src/semantic/index.rs:560-580`
- 测试：`app/src-tauri/src/semantic/index.rs` 的 `tests` 模块

- [ ] **步骤 1：编写顺序稳定性失败测试**

```rust
#[test]
fn reciprocal_rank_fusion_breaks_equal_scores_by_session_id() {
    let first = reciprocal_rank_fusion(
        &[("b".into(), 1.0), ("a".into(), 0.5)],
        &[("a".into(), 1.0), ("b".into(), 0.5)],
        60.0,
    );
    assert_eq!(first.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
}
```

再以相反列表插入顺序构造同分集合，断言仍为 id 升序。

- [ ] **步骤 2：运行测试验证失败**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::index::tests::reciprocal_rank_fusion_breaks_equal_scores_by_session_id -- --nocapture
```

预期：在当前随机 HashMap 顺序下 FAIL；若一次偶然通过，循环运行 20 次并确认至少一次失败，然后保留确定性断言。

- [ ] **步骤 3：实现总序和决胜键**

```rust
merged.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
```

- [ ] **步骤 4：运行 index 测试**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::index::tests
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src-tauri/src/semantic/index.rs
git commit -m "fix(semantic): 固定 RRF 平局结果顺序`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 6：批次 1 验证与审查循环

**文件：**
- 审查：本计划列出的全部 Rust 文件

- [ ] **步骤 1：运行批次定向回归**

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path app/src-tauri/Cargo.toml embedding::
cargo test --manifest-path app/src-tauri/Cargo.toml semantic::
```

预期：全部退出 0。

- [ ] **步骤 2：执行高强度代码审查**

审查本批起点到 `HEAD`，重点检查：旧票据是否可能复活、查询是否仍被后台取消、所有 HTTP await 点是否可取消、canary 是否可能入库或误烧额度、BGE 是否使用真实 SEP id、RRF 是否完全确定。

- [ ] **步骤 3：验证并修复审查发现**

每条发现先写最小失败测试并运行确认失败；只修 CONFIRMED 项。完成后重跑步骤 1，并再次执行步骤 2，直到无确认遗留项。

- [ ] **步骤 4：提交审查修复（如有）**

```powershell
git add <逐条列出本批审查修复文件>
git commit -m "fix(semantic): 收敛语义批次审查问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

预期：无修复则跳过，不创建空提交。
